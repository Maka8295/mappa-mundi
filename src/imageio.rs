//! Decoding photos (HEIC via libheif, everything else via `image`) into
//! RGBA buffers, plus the on-disk thumbnail cache.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::Context as _;
use image::{DynamicImage, RgbaImage};
use libheif_rs::{ColorSpace, HeifContext, LibHeif, RgbChroma};
use rayon::prelude::*;

use crate::library::{self, Kind};

/// Long edge of cached thumbnails, in pixels.
pub const THUMB_SIZE: u32 = 320;

fn libheif() -> &'static LibHeif {
    static LIB: OnceLock<LibHeif> = OnceLock::new();
    LIB.get_or_init(LibHeif::new)
}

pub struct Source<'a> {
    pub path: &'a Path,
    pub kind: Kind,
    pub orientation: u8,
    pub key: u64,
}

fn thumb_path(key: u64) -> PathBuf {
    let name = format!("{key:016x}");
    library::cache_dir().join("thumbs").join(&name[..2]).join(name + ".jpg")
}

/// Returns a small preview, from the disk cache when possible.
pub fn thumbnail(src: &Source) -> anyhow::Result<RgbaImage> {
    let cached = thumb_path(src.key);
    if let Ok(img) = image::open(&cached) {
        return Ok(img.to_rgba8());
    }
    let img = make_thumbnail(src)?;
    if let Err(e) = save_jpeg(&img, &cached) {
        eprintln!("mappa-mundi: caching thumbnail for {}: {e:#}", src.path.display());
    }
    Ok(img)
}

/// Decodes a fresh thumbnail, bypassing the disk cache.
fn make_thumbnail(src: &Source) -> anyhow::Result<RgbaImage> {
    let img = match src.kind {
        Kind::Video => crate::video::thumbnail(src.path)?,
        Kind::Photo if library::is_heif(src.path) => decode_heif(src.path, true)?,
        Kind::Photo => decode_generic(src.path, src.orientation)?,
    };
    Ok(if img.width().max(img.height()) > THUMB_SIZE {
        DynamicImage::ImageRgba8(img).thumbnail(THUMB_SIZE, THUMB_SIZE).to_rgba8()
    } else {
        img
    })
}

fn save_jpeg(img: &RgbaImage, path: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension(format!("{}.tmp", std::process::id()));
    let rgb = DynamicImage::ImageRgba8(img.clone()).to_rgb8();
    let mut out = std::io::BufWriter::new(std::fs::File::create(&tmp)?);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 85).encode_image(&rgb)?;
    drop(out);
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Full-resolution image, downscaled so its long edge is at most `max_dim`.
pub fn full(src: &Source, max_dim: u32) -> anyhow::Result<RgbaImage> {
    let img = match src.kind {
        Kind::Video => crate::video::thumbnail(src.path)?,
        Kind::Photo if library::is_heif(src.path) => decode_heif(src.path, false)?,
        Kind::Photo => decode_generic(src.path, src.orientation)?,
    };
    Ok(if img.width().max(img.height()) > max_dim {
        let (w, h) = fit(img.width(), img.height(), max_dim);
        // Lanczos keeps fine detail crisp; the viewer shows this at 1:1.
        image::imageops::resize(&img, w, h, image::imageops::FilterType::Lanczos3)
    } else {
        img
    })
}

fn fit(w: u32, h: u32, max: u32) -> (u32, u32) {
    let s = max as f64 / w.max(h) as f64;
    (((w as f64 * s).round() as u32).max(1), ((h as f64 * s).round() as u32).max(1))
}

/// Decodes a HEIF/HEIC file. With `thumb`, the small embedded preview is used
/// when present (iPhones store a ~320 px one), which is far cheaper than
/// decoding the full 12 MP grid image.
fn decode_heif(path: &Path, thumb: bool) -> anyhow::Result<RgbaImage> {
    let name = path.to_str().context("non UTF-8 path")?;
    let ctx = HeifContext::read_from_file(name)?;
    let primary = ctx.primary_image_handle()?;
    let p3 = primary.color_profile_raw().is_some_and(|p| is_display_p3(&p.data));

    let mut handle = primary;
    if thumb && handle.number_of_thumbnails() > 0 {
        let mut ids = vec![0; handle.number_of_thumbnails()];
        let n = handle.thumbnail_ids(&mut ids);
        if let Some(t) =
            ids[..n].iter().filter_map(|&id| handle.thumbnail(id).ok()).max_by_key(|t| t.width() * t.height())
        {
            handle = t;
        }
    }

    let decoded = libheif().decode(&handle, ColorSpace::Rgb(RgbChroma::Rgba), None)?;
    let planes = decoded.planes();
    let plane = planes.interleaved.context("no interleaved plane")?;
    let (w, h, stride) = (plane.width, plane.height, plane.stride);
    let mut buf = Vec::with_capacity((w * h * 4) as usize);
    for row in plane.data.chunks(stride).take(h as usize) {
        buf.extend_from_slice(&row[..w as usize * 4]);
    }
    let mut img = RgbaImage::from_raw(w, h, buf).context("bad plane size")?;
    if p3 {
        p3_to_srgb(&mut img);
    }
    Ok(img)
}

fn decode_generic(path: &Path, orientation: u8) -> anyhow::Result<RgbaImage> {
    let img = image::ImageReader::open(path)?.with_guessed_format()?.decode()?;
    Ok(orient(img, orientation).to_rgba8())
}

/// Applies an EXIF orientation (1–8). HEIF files carry their rotation in the
/// container instead, which libheif already honours.
fn orient(img: DynamicImage, o: u8) -> DynamicImage {
    match o {
        2 => img.fliph(),
        3 => img.rotate180(),
        4 => img.flipv(),
        5 => img.rotate90().fliph(),
        6 => img.rotate90(),
        7 => img.rotate270().fliph(),
        8 => img.rotate270(),
        _ => img,
    }
}

/// iPhones tag photos with the Display P3 profile. Showing those pixels as
/// sRGB looks washed out, so convert to sRGB (clipping out-of-gamut colours).
fn is_display_p3(icc: &[u8]) -> bool {
    let utf16: Vec<u8> = "Display P3".bytes().flat_map(|b| [0, b]).collect();
    let has = |needle: &[u8]| icc.windows(needle.len()).any(|w| w == needle);
    has(b"Display P3") || has(&utf16)
}

fn p3_to_srgb(img: &mut RgbaImage) {
    static LUTS: OnceLock<([f32; 256], Vec<u8>)> = OnceLock::new();
    const ENC: usize = 4096;
    let (dec, enc) = LUTS.get_or_init(|| {
        let mut dec = [0f32; 256];
        for (i, d) in dec.iter_mut().enumerate() {
            let c = i as f32 / 255.0;
            *d = if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) };
        }
        let enc = (0..=ENC)
            .map(|i| {
                let l = i as f32 / ENC as f32;
                let c = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * l.powf(1.0 / 2.4) - 0.055 };
                (c * 255.0 + 0.5) as u8
            })
            .collect();
        (dec, enc)
    });
    const M: [[f32; 3]; 3] = [
        [1.224_940_2, -0.224_940_2, 0.0],
        [-0.042_056_955, 1.042_057, 0.0],
        [-0.019_637_555, -0.078_636_05, 1.098_273_6],
    ];
    let to_u8 = |l: f32| enc[(l.clamp(0.0, 1.0) * ENC as f32 + 0.5) as usize];
    img.par_chunks_mut(4).for_each(|px| {
        let (r, g, b) = (dec[px[0] as usize], dec[px[1] as usize], dec[px[2] as usize]);
        px[0] = to_u8(M[0][0] * r + M[0][1] * g + M[0][2] * b);
        px[1] = to_u8(M[1][0] * r + M[1][1] * g + M[1][2] * b);
        px[2] = to_u8(M[2][0] * r + M[2][1] * g + M[2][2] * b);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn p3_conversion_keeps_neutrals_and_saturates_primaries() {
        let mut img = RgbaImage::from_raw(2, 1, vec![128, 128, 128, 255, 200, 50, 50, 255]).unwrap();
        p3_to_srgb(&mut img);
        let p = img.as_raw();
        assert!((p[0] as i32 - 128).abs() <= 1 && (p[1] as i32 - 128).abs() <= 1);
        // P3 red is more saturated than the same numbers in sRGB.
        assert!(p[4] > 200 && p[5] < 50);
    }
}

/// Decodes every file in `$MAPPA_MUNDI_SAMPLES` (thumbnail and full size),
/// writing PNGs to `$MAPPA_MUNDI_SAMPLES_OUT` for eyeballing orientation and
/// colour. Run with `cargo test --release -- --ignored --nocapture`.
#[cfg(test)]
mod sample_tests {
    use super::*;

    #[test]
    #[ignore]
    fn decode_samples() {
        let Ok(dir) = std::env::var("MAPPA_MUNDI_SAMPLES") else { return };
        let out = PathBuf::from(std::env::var("MAPPA_MUNDI_SAMPLES_OUT").unwrap_or_else(|_| "/tmp".into()));
        let mut paths: Vec<_> = std::fs::read_dir(dir).unwrap().map(|e| e.unwrap().path()).collect();
        paths.sort();
        for path in paths {
            let Some(kind) = library::classify(&path) else { continue };
            let t = std::time::Instant::now();
            let meta = library::read_meta(&path, kind, 0);
            let t_meta = t.elapsed();
            let src = Source { path: &path, kind, orientation: meta.orientation, key: 0 };
            let t = std::time::Instant::now();
            let thumb = make_thumbnail(&src).unwrap();
            let t_thumb = t.elapsed();
            let t = std::time::Instant::now();
            let big = full(&src, 2048).unwrap();
            let t_full = t.elapsed();
            let name = path.file_stem().unwrap().to_string_lossy().to_string();
            thumb.save(out.join(format!("{name}-thumb.png"))).unwrap();
            big.save(out.join(format!("{name}-full.jpg"))).unwrap_or_else(|_| {
                DynamicImage::ImageRgba8(big.clone()).to_rgb8().save(out.join(format!("{name}-full.jpg"))).unwrap()
            });
            println!(
                "{name}: {:?} {:?} {:?} {:?} o={} | meta {t_meta:?} thumb {t_thumb:?} {}x{} full {t_full:?} {}x{}",
                meta.lat,
                meta.lon,
                meta.taken.and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|d| d.naive_utc()),
                meta.model,
                meta.orientation,
                thumb.width(),
                thumb.height(),
                big.width(),
                big.height()
            );
        }
    }
}
