//! Scans a folder of photos and videos, extracts capture location and time,
//! pairs Live Photos with their motion clips, and caches the results so that
//! re-opening a large library is instant.

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::UNIX_EPOCH;

use exif::{Exif, In, Tag, Value};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

use crate::geo;
use crate::xmp::{self, Xmp};

const INDEX_VERSION: u32 = 1;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Serialize, Deserialize)]
pub enum Kind {
    Photo,
    Video,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Meta {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// Capture time as local wall-clock seconds since 1970 (timezone-less,
    /// like the clock on the phone when the picture was taken).
    pub taken: Option<i64>,
    pub model: Option<String>,
    pub orientation: u8,
    pub width: u32,
    pub height: u32,
    pub duration: Option<f32>,
}

impl Default for Meta {
    fn default() -> Self {
        Self { lat: None, lon: None, taken: None, model: None, orientation: 1, width: 0, height: 0, duration: None }
    }
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    size: u64,
    mtime: i64,
    meta: Meta,
}

#[derive(Serialize, Deserialize, Default)]
struct IndexCache {
    version: u32,
    entries: HashMap<PathBuf, CacheEntry>,
}

pub struct Item {
    /// The original file; its metadata places the item.
    pub path: PathBuf,
    pub kind: Kind,
    /// Edited version, shown instead of the original.
    pub edited: Option<PathBuf>,
    /// Motion clip of a Live Photo.
    pub live: Option<PathBuf>,
    pub lat: f64,
    pub lon: f64,
    /// Normalised Web Mercator position.
    pub x: f64,
    pub y: f64,
    pub taken: Option<i64>,
    pub model: Option<String>,
    /// EXIF orientation of the file that is shown.
    pub orientation: u8,
    pub width: u32,
    pub height: u32,
    pub duration: Option<f32>,
    /// Title or description from an XMP sidecar.
    pub caption: Option<String>,
    pub favorite: bool,
    /// Identifies this exact file version in the thumbnail cache.
    pub key: u64,
}

impl Item {
    /// The file to display: the edited version when there is one.
    pub fn image_path(&self) -> &Path {
        self.edited.as_deref().unwrap_or(&self.path)
    }

    pub fn file_name(&self) -> String {
        self.path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    }

    pub fn taken_local(&self) -> Option<chrono::NaiveDateTime> {
        chrono::DateTime::from_timestamp(self.taken?, 0).map(|d| d.naive_utc())
    }

    /// False for photos without GPS data; their coordinates are NaN.
    pub fn has_location(&self) -> bool {
        self.lat.is_finite()
    }
}

pub struct Library {
    /// Every photo and video: the `located` geotagged ones first (all the
    /// map can show), then those without a location.
    pub items: Vec<Item>,
    pub located: usize,
    pub photos: usize,
    pub videos: usize,
    pub live: usize,
    pub without_location: usize,
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("mappa-mundi")
}

pub fn classify(path: &Path) -> Option<Kind> {
    let name = path.file_name()?.to_str()?;
    // AppleDouble resource forks that macOS sprinkles over exFAT drives.
    if name.starts_with("._") {
        return None;
    }
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    match ext.as_str() {
        "heic" | "heif" | "hif" | "jpg" | "jpeg" | "png" | "webp" => Some(Kind::Photo),
        "mov" | "mp4" | "m4v" => Some(Kind::Video),
        _ => None,
    }
}

pub fn is_heif(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif" | "hif"))
}

/// FNV-1a, used for stable cache file names.
pub fn fnv1a(parts: &[&[u8]]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for p in parts {
        for &b in *p {
            h ^= b as u64;
            h = h.wrapping_mul(0x100_0000_01b3);
        }
        h ^= 0xff;
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

fn index_path(root: &Path) -> PathBuf {
    let h = fnv1a(&[root.as_os_str().as_encoded_bytes()]);
    cache_dir().join("index").join(format!("{h:016x}.json"))
}

fn load_index(root: &Path) -> IndexCache {
    std::fs::read(index_path(root))
        .ok()
        .and_then(|b| serde_json::from_slice::<IndexCache>(&b).ok())
        .filter(|c| c.version == INDEX_VERSION)
        .unwrap_or_default()
}

fn save_index(root: &Path, cache: &IndexCache) -> anyhow::Result<()> {
    let path = index_path(root);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("tmp");
    serde_json::to_writer(std::io::BufWriter::new(File::create(&tmp)?), cache)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

struct Found {
    path: PathBuf,
    kind: Kind,
    size: u64,
    mtime: i64,
}

/// Scans `root` recursively. Blocking; `progress(done, total)` is called
/// from worker threads as files are processed.
pub fn scan(root: &Path, progress: impl Fn(usize, usize) + Sync) -> anyhow::Result<Library> {
    anyhow::ensure!(root.is_dir(), "{} is not a folder", root.display());

    let mut files: Vec<Found> = Vec::new();
    let mut sidecars: HashSet<PathBuf> = HashSet::new();
    let walk = walkdir::WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| e.depth() == 0 || !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file());
    for e in walk {
        if is_ext(e.path(), "xmp") {
            sidecars.insert(e.into_path());
            continue;
        }
        let Some(kind) = classify(e.path()) else { continue };
        let Ok(md) = e.metadata() else { continue };
        let mtime =
            md.modified().ok().and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos() as i64);
        files.push(Found { path: e.into_path(), kind, size: md.len(), mtime });
    }

    let total = files.len();
    progress(0, total);

    let old = load_index(root);
    let done = AtomicUsize::new(0);
    let (metas, xmps): (Vec<Meta>, Vec<Option<Xmp>>) = files
        .par_iter()
        .map(|f| {
            let meta = match old.entries.get(&f.path) {
                Some(c) if c.size == f.size && c.mtime == f.mtime => c.meta.clone(),
                _ => read_meta(&f.path, f.kind, f.mtime),
            };
            // Sidecars are tiny and get rewritten independently of the
            // photo, so they're read fresh rather than cached.
            let xmp = sidecar_for(&f.path, &sidecars).and_then(|p| xmp::read(&p));
            let n = done.fetch_add(1, Ordering::Relaxed) + 1;
            if n.is_multiple_of(100) || n == total {
                progress(n, total);
            }
            (meta, xmp)
        })
        .unzip();

    let index = IndexCache {
        version: INDEX_VERSION,
        entries: files
            .iter()
            .zip(&metas)
            .map(|(f, m)| (f.path.clone(), CacheEntry { size: f.size, mtime: f.mtime, meta: m.clone() }))
            .collect(),
    };
    if let Err(e) = save_index(root, &index) {
        eprintln!("mappa-mundi: could not save index cache: {e:#}");
    }

    Ok(build_library(&files, metas, xmps))
}

/// Groups a folder's files into assets. iCloud exports (icloudpd, Image
/// Capture) put everything belonging to one photo next to each other:
///
/// - `IMG_1234.HEIC`: the original
/// - `IMG_1234.MOV` or `IMG_1234_HEVC.MOV`: its Live Photo clip
/// - `IMG_1234-adjusted.HEIC`, or `IMG_1234.JPG` beside the HEIC: the edited
///   version, which is shown in place of the original, as Photos does
fn build_library(files: &[Found], metas: Vec<Meta>, xmps: Vec<Option<Xmp>>) -> Library {
    let mut edited = vec![false; files.len()];
    let mut groups: HashMap<(&Path, String), Vec<usize>> = HashMap::new();
    for (i, f) in files.iter().enumerate() {
        let parent = f.path.parent().unwrap_or(Path::new(""));
        let mut stem = f.path.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
        if let Some(s) = stem.strip_suffix("-adjusted") {
            stem = s.to_string();
            edited[i] = true;
        }
        if f.kind == Kind::Video
            && let Some(s) = stem.strip_suffix("_hevc")
        {
            stem = s.to_string();
        }
        groups.entry((parent, stem)).or_default().push(i);
    }
    adopt_orphan_edits(&mut groups, &edited);

    let mut lib = Library { items: Vec::new(), located: 0, photos: 0, videos: 0, live: 0, without_location: 0 };
    for idxs in groups.into_values() {
        let pick = |kind: Kind, want_edited: bool| -> Vec<usize> {
            let mut v: Vec<usize> =
                idxs.iter().copied().filter(|&i| files[i].kind == kind && edited[i] == want_edited).collect();
            // HEIC before JPEG and MOV before MP4: the iPhone's own formats.
            v.sort_by_key(|&i| (!is_heif(&files[i].path), !is_ext(&files[i].path, "mov"), i));
            v
        };
        let (stills, edited_stills) = (pick(Kind::Photo, false), pick(Kind::Photo, true));
        let (clips, edited_clips) = (pick(Kind::Video, false), pick(Kind::Video, true));

        let (primary, shown, live) = if let Some(&p) = stills.first().or(edited_stills.first()) {
            let shown = edited_stills.first().or(stills.get(1)).copied().filter(|&s| s != p);
            (p, shown, clips.first().or(edited_clips.first()).copied())
        } else if let Some(&p) = clips.first().or(edited_clips.first()) {
            (p, edited_clips.first().copied().filter(|&s| s != p), None)
        } else {
            continue;
        };
        match files[primary].kind {
            Kind::Photo => lib.photos += 1,
            Kind::Video => lib.videos += 1,
        }
        lib.live += live.is_some() as usize;

        // Sidecars hold iCloud's current location and date (including fixes
        // made in Photos), so they win over what the camera wrote.
        let m = &metas[primary];
        let xmp = xmps[primary].as_ref().or_else(|| idxs.iter().find_map(|&i| xmps[i].as_ref()));
        let fallback = shown.map(|s| &metas[s]);
        let lat = xmp.and_then(|x| x.lat).or(m.lat).or(fallback.and_then(|f| f.lat));
        let lon = xmp.and_then(|x| x.lon).or(m.lon).or(fallback.and_then(|f| f.lon));
        // Photos without a location stay in the library (for the timeline),
        // with NaN coordinates.
        let location = lat.zip(lon);
        lib.without_location += location.is_none() as usize;
        let (lat, lon) = location.unwrap_or((f64::NAN, f64::NAN));
        let (x, y) = if location.is_some() { geo::to_merc(lat, lon) } else { (f64::NAN, f64::NAN) };
        let image = shown.unwrap_or(primary);
        let (f, im) = (&files[image], &metas[image]);
        let key = fnv1a(&[f.path.as_os_str().as_encoded_bytes(), &f.size.to_le_bytes(), &f.mtime.to_le_bytes()]);
        lib.items.push(Item {
            path: files[primary].path.clone(),
            kind: files[primary].kind,
            edited: shown.map(|s| files[s].path.clone()),
            live: live.map(|c| files[c].path.clone()),
            lat,
            lon,
            x,
            y,
            taken: xmp.and_then(|x| x.taken).or(m.taken),
            model: m.model.clone(),
            orientation: im.orientation,
            width: im.width,
            height: im.height,
            duration: m.duration,
            caption: xmp.and_then(|x| x.caption.clone()),
            favorite: xmp.and_then(|x| x.rating).is_some_and(|r| r >= 5),
            key,
        });
    }
    // Located items first, for the map; each part newest first (cluster
    // representatives, grids, viewer).
    lib.items.sort_by(|a, b| {
        (b.has_location().cmp(&a.has_location())).then(b.taken.cmp(&a.taken)).then_with(|| a.path.cmp(&b.path))
    });
    lib.located = lib.items.len() - lib.without_location;
    lib
}

/// icloudpd's `name-id7` policy gives originals a 7-character id
/// (`IMG_1234_QVRmWmV.HEIC`) but, due to a bug, not their edited copies
/// (`IMG_1234-adjusted.HEIC`). Reunite them when the match is unambiguous.
fn adopt_orphan_edits(groups: &mut HashMap<(&Path, String), Vec<usize>>, edited: &[bool]) {
    let mut by_prefix: HashMap<(&Path, String), Vec<String>> = HashMap::new();
    for ((dir, stem), idxs) in groups.iter() {
        if let Some((prefix, id)) = stem.rsplit_once('_')
            && id.chars().count() == 7
            && idxs.iter().any(|&i| !edited[i])
        {
            by_prefix.entry((*dir, prefix.to_string())).or_default().push(stem.clone());
        }
    }
    let orphans: Vec<(&Path, String)> =
        groups.iter().filter(|(_, idxs)| idxs.iter().all(|&i| edited[i])).map(|(k, _)| k.clone()).collect();
    for (dir, stem) in orphans {
        if let Some([target]) = by_prefix.get(&(dir, stem.clone())).map(Vec::as_slice) {
            let moved = groups.remove(&(dir, stem)).unwrap_or_default();
            groups.entry((dir, target.clone())).or_default().extend(moved);
        }
    }
}

fn is_ext(path: &Path, ext: &str) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext))
}

/// `IMG_1234.HEIC.xmp` (icloudpd) or `IMG_1234.xmp` (Apple Photos, Lightroom).
fn sidecar_for(path: &Path, sidecars: &HashSet<PathBuf>) -> Option<PathBuf> {
    let mut full = path.as_os_str().to_owned();
    full.push(".xmp");
    [PathBuf::from(full), path.with_extension("xmp"), path.with_extension("XMP")]
        .into_iter()
        .find(|p| sidecars.contains(p))
}

pub fn read_meta(path: &Path, kind: Kind, mtime_ns: i64) -> Meta {
    let mut m = match kind {
        Kind::Photo => photo_meta(path),
        Kind::Video => crate::quicktime::read(path)
            .map(|q| Meta {
                lat: q.lat,
                lon: q.lon,
                taken: q.created,
                model: q.model,
                duration: q.duration,
                ..Default::default()
            })
            .unwrap_or_default(),
    };
    if m.taken.is_none() {
        // Fall back to the file's modification time in local time.
        let secs = mtime_ns.div_euclid(1_000_000_000);
        m.taken = chrono::DateTime::from_timestamp(secs, 0)
            .map(|d| d.with_timezone(&chrono::Local).naive_local().and_utc().timestamp());
    }
    m
}

fn photo_meta(path: &Path) -> Meta {
    let mut m = Meta::default();
    let Ok(file) = File::open(path) else { return m };
    let Ok(exif) = exif::Reader::new().read_from_container(&mut BufReader::new(file)) else {
        return m;
    };
    if let (Some(lat), Some(lon)) = (
        gps_coord(&exif, Tag::GPSLatitude, Tag::GPSLatitudeRef, b'S'),
        gps_coord(&exif, Tag::GPSLongitude, Tag::GPSLongitudeRef, b'W'),
    ) {
        // (0, 0) is what broken GPS writers emit; nobody photographs Null Island.
        let valid = lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat != 0.0 || lon != 0.0);
        if valid {
            m.lat = Some(lat);
            m.lon = Some(lon);
        }
    }
    m.taken =
        [Tag::DateTimeOriginal, Tag::DateTimeDigitized, Tag::DateTime].into_iter().find_map(|t| exif_date(&exif, t));
    m.model = exif.get_field(Tag::Model, In::PRIMARY).and_then(|f| match &f.value {
        Value::Ascii(v) => v.first().map(|s| String::from_utf8_lossy(s).trim().to_string()),
        _ => None,
    });
    m.orientation = exif
        .get_field(Tag::Orientation, In::PRIMARY)
        .and_then(|f| f.value.get_uint(0))
        .filter(|o| (1..=8).contains(o))
        .unwrap_or(1) as u8;
    let dim = |t| exif.get_field(t, In::PRIMARY).and_then(|f| f.value.get_uint(0)).unwrap_or(0);
    m.width = dim(Tag::PixelXDimension);
    m.height = dim(Tag::PixelYDimension);
    m
}

fn gps_coord(exif: &Exif, tag: Tag, ref_tag: Tag, negative: u8) -> Option<f64> {
    let Value::Rational(v) = &exif.get_field(tag, In::PRIMARY)?.value else { return None };
    let part = |i: usize| v.get(i).map_or(0.0, |r| r.to_f64());
    let deg = part(0) + part(1) / 60.0 + part(2) / 3600.0;
    if v.is_empty() || !deg.is_finite() {
        return None;
    }
    let hemisphere = exif.get_field(ref_tag, In::PRIMARY).and_then(|f| match &f.value {
        Value::Ascii(a) => a.first().and_then(|s| s.first().copied()),
        _ => None,
    });
    Some(if hemisphere == Some(negative) { -deg } else { deg })
}

fn exif_date(exif: &Exif, tag: Tag) -> Option<i64> {
    let Value::Ascii(v) = &exif.get_field(tag, In::PRIMARY)?.value else { return None };
    let d = exif::DateTime::from_ascii(v.first()?).ok()?;
    chrono::NaiveDate::from_ymd_opt(d.year as i32, d.month as u32, d.day as u32)?
        .and_hms_opt(d.hour as u32, d.minute as u32, d.second as u32)
        .map(|t| t.and_utc().timestamp())
}

#[cfg(test)]
mod perf_tests {
    /// Scans `$MAPPA_MUNDI_SCAN_DIR` twice (cold, then with the index cache).
    #[test]
    #[ignore]
    fn scan_timing() {
        let Ok(dir) = std::env::var("MAPPA_MUNDI_SCAN_DIR") else { return };
        for pass in ["cold", "cached"] {
            let t = std::time::Instant::now();
            let lib = super::scan(std::path::Path::new(&dir), |_, _| {}).unwrap();
            println!("{pass}: {:?} for {} items on map", t.elapsed(), lib.items.len());
        }
    }
}

#[cfg(test)]
mod grouping_tests {
    use super::*;

    fn found(path: &str) -> Found {
        let path = PathBuf::from(path);
        Found { kind: classify(&path).unwrap(), path, size: 1, mtime: 0 }
    }

    fn here() -> Meta {
        Meta { lat: Some(51.5), lon: Some(-0.12), taken: Some(1_700_000_000), ..Default::default() }
    }

    fn build(names: &[&str], meta: impl Fn(&str) -> Meta, xmp: impl Fn(&str) -> Option<Xmp>) -> Library {
        let files: Vec<Found> = names.iter().map(|n| found(n)).collect();
        let metas = names.iter().map(|n| meta(n)).collect();
        let xmps = names.iter().map(|n| xmp(n)).collect();
        build_library(&files, metas, xmps)
    }

    fn names(lib: &Library) -> Vec<(String, Option<String>, Option<String>)> {
        let name = |p: &Path| p.file_name().unwrap().to_string_lossy().into_owned();
        let mut v: Vec<_> = lib
            .items
            .iter()
            .map(|i| (name(&i.path), i.edited.as_deref().map(name), i.live.as_deref().map(name)))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn icloudpd_layouts() {
        let lib = build(
            &[
                // name-id7 + original MOV policy
                "/l/2023/08/15/IMG_0001_QVRmWmV.HEIC",
                "/l/2023/08/15/IMG_0001_QVRmWmV.MOV",
                // default suffix policy
                "/l/2023/08/15/IMG_0002.HEIC",
                "/l/2023/08/15/IMG_0002_HEVC.MOV",
                // edited copy that lost its id (icloudpd name-id7 bug)
                "/l/2023/08/15/IMG_0003_AbCdEfG.HEIC",
                "/l/2023/08/15/IMG_0003-adjusted.HEIC",
                // edit rendered to another format
                "/l/2023/08/15/IMG_0004_ZZZZZZZ.HEIC",
                "/l/2023/08/15/IMG_0004_ZZZZZZZ.JPG",
                // edited (trimmed) video
                "/l/2023/08/15/IMG_0005_YYYYYYY.MOV",
                "/l/2023/08/15/IMG_0005-adjusted.MOV",
                // a plain video and a plain JPEG Live Photo
                "/l/2023/08/15/IMG_0006.MOV",
                "/l/2023/08/16/IMG_0007.JPG",
                "/l/2023/08/16/IMG_0007.MOV",
            ],
            |_| here(),
            |_| None,
        );
        let s = |x: &str| Some(x.to_string());
        assert_eq!(
            names(&lib),
            vec![
                ("IMG_0001_QVRmWmV.HEIC".into(), None, s("IMG_0001_QVRmWmV.MOV")),
                ("IMG_0002.HEIC".into(), None, s("IMG_0002_HEVC.MOV")),
                ("IMG_0003_AbCdEfG.HEIC".into(), s("IMG_0003-adjusted.HEIC"), None),
                ("IMG_0004_ZZZZZZZ.HEIC".into(), s("IMG_0004_ZZZZZZZ.JPG"), None),
                ("IMG_0005_YYYYYYY.MOV".into(), s("IMG_0005-adjusted.MOV"), None),
                ("IMG_0006.MOV".into(), None, None),
                ("IMG_0007.JPG".into(), None, s("IMG_0007.MOV")),
            ]
        );
        assert_eq!((lib.photos, lib.videos, lib.live), (5, 2, 3));
    }

    #[test]
    fn ambiguous_orphan_edit_is_left_alone() {
        // Two originals could own this edit; don't guess.
        let lib = build(
            &["/l/IMG_0003_AAAAAAA.HEIC", "/l/IMG_0003_BBBBBBB.HEIC", "/l/IMG_0003-adjusted.HEIC"],
            |_| here(),
            |_| None,
        );
        assert_eq!(lib.items.len(), 3);
    }

    #[test]
    fn sidecar_location_and_date_win() {
        let galway = Xmp {
            lat: Some(53.27),
            lon: Some(-9.05),
            taken: Some(1_600_000_000),
            rating: Some(5),
            caption: Some("Pier".into()),
        };
        let lib = build(
            &["/l/a.HEIC", "/l/b.HEIC"],
            // b.HEIC has no GPS of its own; its location was added in Photos.
            |n| if n.ends_with("a.HEIC") { here() } else { Meta::default() },
            |_| Some(galway.clone()),
        );
        assert_eq!(lib.items.len(), 2);
        for it in &lib.items {
            assert_eq!((it.lat, it.lon, it.taken), (53.27, -9.05, Some(1_600_000_000)));
            assert!(it.favorite && it.caption.as_deref() == Some("Pier"));
        }
    }

    #[test]
    fn items_without_location_come_last() {
        let lib = build(
            &["/l/new.PNG", "/l/old.HEIC", "/l/mid.HEIC"],
            |n| match n {
                "/l/new.PNG" => Meta { taken: Some(1_800_000_000), ..Default::default() },
                "/l/old.HEIC" => Meta { taken: Some(1_600_000_000), ..here() },
                _ => here(),
            },
            |_| None,
        );
        assert_eq!((lib.items.len(), lib.located, lib.without_location), (3, 2, 1));
        let order: Vec<String> = lib.items.iter().map(Item::file_name).collect();
        assert_eq!(order, ["mid.HEIC", "old.HEIC", "new.PNG"]);
        assert!(!lib.items[2].has_location() && lib.items[2].x.is_nan());
    }
}
