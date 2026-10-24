//! Map tile layers: the built-in world map, OpenStreetMap street maps
//! (`.pmtiles` from tools/street-maps), and optional raster tiles from a
//! local `.mbtiles` file or a `{z}/{x}/{y}.png` folder.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context as _;
use egui::ColorImage;
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::pmtiles::Pmtiles;
use crate::streets::{self, Labels, TileRef};
use crate::texcache::TextureCache;
use crate::work::WorkQueue;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct TileKey {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    /// Rendered edge length in pixels (0 for raster sources).
    pub px: u32,
}

pub struct TileLayer {
    pub attribution: String,
    pub min_zoom: u8,
    pub max_zoom: u8,
    /// Whether tile size follows the display's pixel density.
    pub scalable: bool,
    pub cache: TextureCache<TileKey, TileKey>,
    /// Place and street names, for street maps.
    pub labels: Option<StreetLabels>,
}

/// Names decoded alongside street map tiles, kept per data tile.
pub struct StreetLabels {
    rx: crossbeam_channel::Receiver<(DataTile, Arc<Labels>)>,
    tiles: HashMap<DataTile, (Arc<Labels>, u64)>,
    frame: u64,
    /// Deepest zoom level the data has; deeper views reuse it.
    pub data_max_zoom: u8,
}

pub type DataTile = (u8, u32, u32);

impl StreetLabels {
    pub fn get(&mut self, key: &DataTile) -> Option<Arc<Labels>> {
        let frame = self.frame;
        self.tiles.get_mut(key).map(|(l, used)| {
            *used = frame;
            l.clone()
        })
    }

    fn pump(&mut self) {
        self.frame += 1;
        for (key, labels) in self.rx.try_iter() {
            self.tiles.entry(key).or_insert((labels, self.frame));
        }
        if self.tiles.len() > 600 {
            let mut by_age: Vec<(u64, DataTile)> = self.tiles.iter().map(|(k, (_, u))| (*u, *k)).collect();
            by_age.sort_unstable();
            for (_, k) in by_age.into_iter().take(200) {
                self.tiles.remove(&k);
            }
        }
    }
}

enum Source {
    Basemap,
    Streets(Arc<Pmtiles>, crossbeam_channel::Sender<(DataTile, Arc<Labels>)>),
    MbTiles(PathBuf),
    Dir(PathBuf),
}

impl TileLayer {
    pub fn basemap(ctx: &egui::Context) -> Self {
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 6);
        Self::start(Source::Basemap, "Natural Earth".into(), 0, 20, true, threads, ctx)
    }

    /// Street maps from a vector `.pmtiles` archive, drawn in the app's
    /// style. Zooming past the data's deepest level redraws its vectors at
    /// a larger scale, so streets stay sharp down to zoom 20.
    pub fn streets(path: &Path, ctx: &egui::Context) -> anyhow::Result<Self> {
        let pm = Arc::new(Pmtiles::open(path)?);
        let (tx, rx) = crossbeam_channel::unbounded();
        let labels = StreetLabels { rx, tiles: HashMap::new(), frame: 0, data_max_zoom: pm.max_zoom };
        let (min, attribution) = (pm.min_zoom, strip_html(&pm.attribution));
        let threads = std::thread::available_parallelism().map_or(4, |n| n.get()).clamp(2, 6);
        let mut layer = Self::start(Source::Streets(pm, tx), attribution, min, 20, true, threads, ctx);
        layer.labels = Some(labels);
        Ok(layer)
    }

    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.cache.begin_frame(ctx);
        if let Some(l) = &mut self.labels {
            l.pump();
        }
    }

    /// Opens a raster `.mbtiles` file or a directory laid out as `z/x/y.png`.
    pub fn open(path: &Path, ctx: &egui::Context) -> anyhow::Result<Self> {
        anyhow::ensure!(path.exists(), "{} does not exist", path.display());
        if path.is_dir() {
            let zooms: Vec<u8> =
                std::fs::read_dir(path)?.filter_map(|e| e.ok()?.file_name().to_str()?.parse().ok()).collect();
            let (min, max) = (zooms.iter().min().copied(), zooms.iter().max().copied());
            let (Some(min), Some(max)) = (min, max) else {
                anyhow::bail!("{} has no zoom-level folders (expected z/x/y.png)", path.display());
            };
            let attribution = std::fs::read_to_string(path.join("attribution.txt")).unwrap_or_default();
            return Ok(Self::start(Source::Dir(path.into()), attribution.trim().into(), min, max, false, 4, ctx));
        }

        let conn = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("{} is not a readable .mbtiles file", path.display()))?;
        let meta = |k: &str| -> Option<String> {
            conn.query_row("SELECT value FROM metadata WHERE name = ?1", [k], |r| r.get(0)).optional().ok()?
        };
        if let Some(fmt) = meta("format") {
            anyhow::ensure!(
                matches!(fmt.as_str(), "png" | "jpg" | "jpeg" | "webp"),
                "{} contains '{fmt}' tiles; only raster (png/jpg/webp) MBTiles are supported",
                path.display()
            );
        }
        let zoom = |k: &str, sql: &str| -> u8 {
            meta(k).and_then(|v| v.parse().ok()).or_else(|| conn.query_row(sql, [], |r| r.get(0)).ok()).unwrap_or(0)
        };
        let min = zoom("minzoom", "SELECT MIN(zoom_level) FROM tiles");
        let max = zoom("maxzoom", "SELECT MAX(zoom_level) FROM tiles");
        let attribution = meta("attribution").unwrap_or_default();
        Ok(Self::start(Source::MbTiles(path.into()), strip_html(&attribution), min, max, false, 4, ctx))
    }

    fn start(
        source: Source,
        attribution: String,
        min_zoom: u8,
        max_zoom: u8,
        scalable: bool,
        threads: usize,
        ctx: &egui::Context,
    ) -> Self {
        let queue = WorkQueue::new();
        let (tx, rx) = crossbeam_channel::unbounded();
        let source = Arc::new(source);
        queue.spawn_workers("tiles", threads, tx, ctx.clone(), move || {
            let source = source.clone();
            let mut conn: Option<Connection> = None;
            move |key: TileKey| {
                let img = load_tile(&source, &mut conn, key).unwrap_or_else(|e| {
                    eprintln!("mappa-mundi: tile {key:?}: {e:#}");
                    None
                });
                (key, img)
            }
        });
        // Budget roughly 200 MB of GPU memory for tiles.
        let cache = TextureCache::new("tile", 200, queue, rx);
        Self { attribution, min_zoom, max_zoom, scalable, cache, labels: None }
    }
}

fn load_tile(source: &Source, conn: &mut Option<Connection>, k: TileKey) -> anyhow::Result<Option<ColorImage>> {
    let bytes = match source {
        Source::Basemap => {
            let px = crate::basemap::get().render_tile(k.z, k.x, k.y, k.px).context("render failed")?;
            return Ok(Some(ColorImage::from_rgba_premultiplied([k.px as usize; 2], &px)));
        }
        Source::Streets(pm, labels) => {
            let t = TileRef::new(k.z, k.x, k.y, pm.max_zoom);
            let Some(data) = pm.tile(t.dz, t.dx, t.dy)? else { return Ok(None) };
            let layers = crate::mvt::decode(&data, streets::wanted_layer)?;
            // Every tile sends its data tile's names (cheap); the UI keeps the first.
            let _ = labels.send(((t.dz, t.dx, t.dy), Arc::new(streets::labels(&layers, t.dz, t.dx, t.dy))));
            let px = streets::render(&layers, &t, k.px).context("render failed")?;
            return Ok(Some(ColorImage::from_rgba_premultiplied([k.px as usize; 2], &px)));
        }
        Source::MbTiles(path) => {
            if conn.is_none() {
                *conn = Some(Connection::open_with_flags(
                    path,
                    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
                )?);
            }
            // MBTiles rows use TMS numbering (y grows northwards).
            let row = (1u32 << k.z) - 1 - k.y;
            conn.as_ref()
                .unwrap()
                .query_row(
                    "SELECT tile_data FROM tiles WHERE zoom_level = ?1 AND tile_column = ?2 AND tile_row = ?3",
                    [k.z as u32, k.x, row],
                    |r| r.get::<_, Vec<u8>>(0),
                )
                .optional()?
        }
        Source::Dir(root) => ["png", "jpg", "jpeg", "webp"]
            .iter()
            .find_map(|ext| std::fs::read(root.join(format!("{}/{}/{}.{ext}", k.z, k.x, k.y))).ok()),
    };
    let Some(bytes) = bytes else { return Ok(None) };
    let img = image::load_from_memory(&bytes)?.to_rgba8();
    Ok(Some(ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw())))
}

fn strip_html(s: &str) -> String {
    let mut out = String::new();
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.replace("&copy;", "©").trim().to_string()
}
