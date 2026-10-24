//! The built-in offline world map: Natural Earth vector data (embedded at
//! compile time, see `tools/build_basemap.rs`) rasterised into slippy-map
//! tiles on demand with tiny-skia.

use std::io::Read;
use std::sync::OnceLock;

use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

static ASSET: &[u8] = include_bytes!("../assets/basemap.bin");
const MAGIC: &[u8; 4] = b"PMB1";
const FIXED: f64 = (1u64 << 24) as f64;

const LAND: u8 = 0;
const LAKE: u8 = 1;
const URBAN: u8 = 2;
const RIVER: u8 = 3;
const STATE: u8 = 4;
const BORDER: u8 = 5;
const ROAD: u8 = 6;

// dungeonOS stone: dark water, warm stone land, bronze borders, brass roads.
pub const OCEAN: [u8; 3] = [0x14, 0x12, 0x10];
const LAND_RGB: [u8; 3] = [0x2B, 0x26, 0x21];
const URBAN_RGB: [u8; 3] = [0x36, 0x2F, 0x28];
const RIVER_RGB: [u8; 3] = [0x19, 0x17, 0x14];
const STATE_RGB: [u8; 3] = [0x46, 0x3D, 0x34];
const BORDER_RGB: [u8; 3] = [0x7E, 0x70, 0x5D];
const ROAD_RGB: [u8; 3] = [0x40, 0x38, 0x30];
const FERRY_RGB: [u8; 3] = [0x4F, 0x49, 0x40];
const CASING_RGB: [u8; 3] = [0x1A, 0x18, 0x16];
const SECONDARY_RGB: [u8; 3] = [0x7A, 0x5E, 0x47];
const MAJOR_RGB: [u8; 3] = [0xA8, 0x8A, 0x5B];

struct Piece {
    min_zoom: f32,
    class: u8,
    bbox: [f32; 4],
    rings: std::ops::Range<u32>,
}

struct Layer {
    kind: u8,
    lod: u8,
    pieces: Vec<Piece>,
    /// Point ranges of each ring.
    rings: Vec<std::ops::Range<u32>>,
    points: Vec<[f32; 2]>,
}

pub struct City {
    pub name: String,
    pub country: String,
    pub region: String,
    pub x: f32,
    pub y: f32,
    lat: f32,
    lon: f32,
    pub min_zoom: f32,
    pub pop: u32,
    pub capital: bool,
}

pub struct Country {
    pub name: String,
    pub x: f32,
    pub y: f32,
    pub min_label: f32,
    pub max_label: f32,
}

pub struct Basemap {
    layers: Vec<Layer>,
    pub cities: Vec<City>,
    pub countries: Vec<Country>,
}

pub fn get() -> &'static Basemap {
    static MAP: OnceLock<Basemap> = OnceLock::new();
    MAP.get_or_init(|| Basemap::decode(ASSET).expect("corrupt embedded basemap"))
}

struct Dec<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Dec<'_> {
    fn u8(&mut self) -> u8 {
        self.pos += 1;
        self.b[self.pos - 1]
    }
    fn u32(&mut self) -> u32 {
        let v = u32::from_le_bytes(self.b[self.pos..self.pos + 4].try_into().unwrap());
        self.pos += 4;
        v
    }
    fn f32(&mut self) -> f32 {
        f32::from_bits(self.u32())
    }
    fn var(&mut self) -> u64 {
        let mut v = 0u64;
        let mut shift = 0;
        loop {
            let b = self.u8();
            v |= ((b & 0x7f) as u64) << shift;
            if b & 0x80 == 0 {
                return v;
            }
            shift += 7;
        }
    }
    fn svar(&mut self) -> i64 {
        let v = self.var();
        ((v >> 1) as i64) ^ -((v & 1) as i64)
    }
    fn str(&mut self) -> String {
        let n = self.var() as usize;
        self.pos += n;
        String::from_utf8_lossy(&self.b[self.pos - n..self.pos]).into_owned()
    }
    fn zoom(&mut self) -> f32 {
        self.u8() as f32 / 10.0
    }
}

impl Basemap {
    fn decode(compressed: &[u8]) -> anyhow::Result<Self> {
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(compressed).read_to_end(&mut raw)?;
        anyhow::ensure!(raw.starts_with(MAGIC), "bad basemap magic");
        let mut d = Dec { b: &raw, pos: 4 };

        let mut layers = Vec::new();
        for _ in 0..d.u32() {
            let (kind, lod, _geom) = (d.u8(), d.u8(), d.u8());
            let n = d.u32() as usize;
            let mut layer = Layer { kind, lod, pieces: Vec::with_capacity(n), rings: Vec::new(), points: Vec::new() };
            for _ in 0..n {
                let min_zoom = d.zoom();
                let class = d.u8();
                let ring_start = layer.rings.len() as u32;
                let mut bbox = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
                for _ in 0..d.var() {
                    let npts = d.var();
                    let start = layer.points.len() as u32;
                    let (mut x, mut y) = (0i64, 0i64);
                    for _ in 0..npts {
                        x += d.svar();
                        y += d.svar();
                        let p = [(x as f64 / FIXED) as f32, (y as f64 / FIXED) as f32];
                        bbox = [bbox[0].min(p[0]), bbox[1].min(p[1]), bbox[2].max(p[0]), bbox[3].max(p[1])];
                        layer.points.push(p);
                    }
                    layer.rings.push(start..layer.points.len() as u32);
                }
                layer.pieces.push(Piece { min_zoom, class, bbox, rings: ring_start..layer.rings.len() as u32 });
            }
            layers.push(layer);
        }

        let n = d.u32() as usize;
        let mut cities = Vec::with_capacity(n);
        for _ in 0..n {
            let (name, country, region) = (d.str(), d.str(), d.str());
            let (x, y) = (d.f32(), d.f32());
            let (lat, lon) = crate::geo::from_merc(x as f64, y as f64);
            cities.push(City {
                name,
                country,
                region,
                x,
                y,
                lat: lat as f32,
                lon: lon as f32,
                min_zoom: d.zoom(),
                pop: d.u32(),
                capital: d.u8() != 0,
            });
        }
        // Big cities first so label placement favours them.
        cities.sort_by_key(|c| (!c.capital, std::cmp::Reverse(c.pop)));

        let n = d.u32() as usize;
        let mut countries = Vec::with_capacity(n);
        for _ in 0..n {
            countries.push(Country { name: d.str(), x: d.f32(), y: d.f32(), min_label: d.zoom(), max_label: d.zoom() });
        }
        Ok(Self { layers, cities, countries })
    }

    /// Nearest populated place to a point, with its distance in km.
    pub fn nearest_city(&self, lat: f64, lon: f64) -> Option<(&City, f64)> {
        let coslat = lat.to_radians().cos();
        self.cities
            .iter()
            .map(|c| {
                // Equirectangular approximation: plenty for ranking.
                let mut dlon = (c.lon as f64 - lon).abs();
                if dlon > 180.0 {
                    dlon = 360.0 - dlon;
                }
                let (dx, dy) = (dlon * coslat, c.lat as f64 - lat);
                (c, 111.2 * (dx * dx + dy * dy).sqrt())
            })
            // Prefer notable places: a big city slightly further away beats a hamlet.
            .min_by(|a, b| weighted(a).total_cmp(&weighted(b)))
            .map(|(c, _)| (c, crate::geo::haversine_km(lat, lon, c.lat as f64, c.lon as f64)))
    }

    fn layers(&self, kind: u8, lod: u8) -> impl Iterator<Item = &Layer> {
        self.layers.iter().filter(move |l| l.kind == kind && l.lod == lod)
    }

    /// Renders tile (z, x, y) as a `size` x `size` premultiplied RGBA image.
    pub fn render_tile(&self, z: u8, tx: u32, ty: u32, size: u32) -> Option<Vec<u8>> {
        let mut pm = Pixmap::new(size, size)?;
        pm.fill(Color::from_rgba8(OCEAN[0], OCEAN[1], OCEAN[2], 255));
        let r = Raster::new(z, tx, ty, size);
        let zf = z as f32;
        let lod = match z {
            0..=2 => 0,
            3..=4 => 1,
            _ => 2,
        };
        let mid_lod = if z <= 4 { 1 } else { 2 };
        // Line widths are specified in points (1/256 of a tile edge).
        let pt = size as f32 / 256.0;
        let grow =
            |base: f32, per_zoom: f32, from: f32, max: f32| (base + per_zoom * (zf - from).max(0.0)).min(max) * pt;

        for l in self.layers(LAND, lod) {
            r.fill(&mut pm, l, LAND_RGB, |p| p.min_zoom <= zf + 1.5);
        }
        if z >= 5 {
            for l in self.layers(URBAN, 2) {
                r.fill(&mut pm, l, URBAN_RGB, |p| p.min_zoom <= zf + 2.5);
            }
        }
        for l in self.layers(LAKE, mid_lod) {
            r.fill(&mut pm, l, OCEAN, |p| p.min_zoom <= zf + 2.0);
        }
        if z >= 3 {
            let w = grow(0.5, 0.18, 3.0, 2.2);
            for l in self.layers(RIVER, mid_lod) {
                r.stroke(&mut pm, l, RIVER_RGB, w, None, |p| p.min_zoom <= zf + 1.5);
            }
        }
        if z >= 5 {
            let roads: Vec<&Layer> = self.layers(ROAD, 2).collect();
            let show = |p: &Piece| p.min_zoom <= zf + 1.0;
            for l in &roads {
                r.stroke(&mut pm, l, ROAD_RGB, grow(0.5, 0.2, 5.0, 2.2), None, |p| p.class == 0 && show(p));
                r.stroke(&mut pm, l, FERRY_RGB, grow(0.6, 0.1, 5.0, 1.4), Some([4.0, 3.0]), |p| {
                    p.class == 3 && show(p)
                });
                if z >= 7 {
                    // Casing under highways.
                    r.stroke(&mut pm, l, CASING_RGB, grow(1.4, 0.45, 5.0, 5.4), None, |p| {
                        (p.class == 1 || p.class == 2) && show(p)
                    });
                }
                // Muted until city zoom so roads don't compete with the photos.
                let (secondary, major) = if z < 7 { (ROAD_RGB, SECONDARY_RGB) } else { (SECONDARY_RGB, MAJOR_RGB) };
                r.stroke(&mut pm, l, secondary, grow(0.6, 0.3, 5.0, 3.6), None, |p| p.class == 1 && show(p));
                r.stroke(&mut pm, l, major, grow(0.8, 0.4, 5.0, 4.4), None, |p| p.class == 2 && show(p));
            }
        }
        if z >= 3 {
            let w = grow(0.5, 0.1, 3.0, 1.2);
            for l in self.layers(STATE, mid_lod) {
                r.stroke(&mut pm, l, STATE_RGB, w, Some([3.0, 2.0]), |p| l.lod == 1 || p.min_zoom <= zf + 3.5);
            }
        }
        let w = grow(0.7, 0.12, 0.0, 2.0);
        for l in self.layers(BORDER, lod) {
            r.stroke(&mut pm, l, BORDER_RGB, w, None, |p| p.class == 0);
            r.stroke(&mut pm, l, BORDER_RGB, w, Some([4.0, 3.0]), |p| p.class != 0);
        }
        Some(pm.take())
    }
}

fn weighted((c, d): &(&City, f64)) -> f64 {
    d - (c.pop as f64).max(1.0).log10() * 2.0
}

/// Projects normalised Mercator coordinates into one tile's pixel space and
/// clips geometry so huge pieces stay well-behaved at deep zoom levels.
struct Raster {
    x0: f64,
    y0: f64,
    scale: f64,
    size: f64,
    /// Tile bbox in normalised coordinates.
    bbox: [f64; 4],
}

impl Raster {
    fn new(z: u8, tx: u32, ty: u32, size: u32) -> Self {
        let n = (1u64 << z) as f64;
        let (x0, y0) = (tx as f64 / n, ty as f64 / n);
        Self { x0, y0, scale: n * size as f64, size: size as f64, bbox: [x0, y0, x0 + 1.0 / n, y0 + 1.0 / n] }
    }

    fn px(&self, p: [f32; 2]) -> (f64, f64) {
        ((p[0] as f64 - self.x0) * self.scale, (p[1] as f64 - self.y0) * self.scale)
    }

    fn hits(&self, b: &[f32; 4], margin_px: f64) -> bool {
        let m = margin_px / self.scale;
        (b[0] as f64) <= self.bbox[2] + m
            && (b[2] as f64) >= self.bbox[0] - m
            && (b[1] as f64) <= self.bbox[3] + m
            && (b[3] as f64) >= self.bbox[1] - m
    }

    fn fill(&self, pm: &mut Pixmap, l: &Layer, rgb: [u8; 3], keep: impl Fn(&Piece) -> bool) {
        let mut pb = PathBuilder::new();
        let lim = (-4.0, self.size + 4.0);
        for p in l.pieces.iter().filter(|p| self.hits(&p.bbox, 1.0) && keep(p)) {
            for ring in &l.rings[p.rings.start as usize..p.rings.end as usize] {
                let pts: Vec<(f64, f64)> =
                    l.points[ring.start as usize..ring.end as usize].iter().map(|&q| self.px(q)).collect();
                let pts = clip_ring(pts, lim);
                if pts.len() < 3 {
                    continue;
                }
                pb.move_to(pts[0].0 as f32, pts[0].1 as f32);
                for q in &pts[1..] {
                    pb.line_to(q.0 as f32, q.1 as f32);
                }
                pb.close();
            }
        }
        if let Some(path) = pb.finish() {
            pm.fill_path(&path, &paint(rgb), FillRule::EvenOdd, Transform::identity(), None);
        }
    }

    fn stroke(
        &self,
        pm: &mut Pixmap,
        l: &Layer,
        rgb: [u8; 3],
        width: f32,
        dash: Option<[f32; 2]>,
        keep: impl Fn(&Piece) -> bool,
    ) {
        let mut pb = PathBuilder::new();
        let m = width as f64 + 2.0;
        let lim = (-m, self.size + m);
        for p in l.pieces.iter().filter(|p| self.hits(&p.bbox, m) && keep(p)) {
            for ring in &l.rings[p.rings.start as usize..p.rings.end as usize] {
                let pts = &l.points[ring.start as usize..ring.end as usize];
                let mut last: Option<(f64, f64)> = None;
                for w in pts.windows(2) {
                    let Some((a, b)) = clip_segment(self.px(w[0]), self.px(w[1]), lim) else {
                        last = None;
                        continue;
                    };
                    if last != Some(a) {
                        pb.move_to(a.0 as f32, a.1 as f32);
                    }
                    pb.line_to(b.0 as f32, b.1 as f32);
                    last = Some(b);
                }
            }
        }
        let Some(path) = pb.finish() else { return };
        let scale = self.size as f32 / 256.0;
        let stroke = Stroke {
            width,
            line_cap: LineCap::Round,
            line_join: LineJoin::Round,
            dash: dash.and_then(|[a, b]| StrokeDash::new(vec![a * scale, b * scale], 0.0)),
            ..Default::default()
        };
        pm.stroke_path(&path, &paint(rgb), &stroke, Transform::identity(), None);
    }
}

fn paint(rgb: [u8; 3]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(rgb[0], rgb[1], rgb[2], 255);
    p.anti_alias = true;
    p
}

/// Sutherland–Hodgman clip of a ring against the square [lo, hi]².
fn clip_ring(mut pts: Vec<(f64, f64)>, (lo, hi): (f64, f64)) -> Vec<(f64, f64)> {
    let inside_all = pts.iter().all(|p| p.0 >= lo && p.0 <= hi && p.1 >= lo && p.1 <= hi);
    if inside_all {
        return pts;
    }
    for (axis, v, greater) in [(0, lo, true), (0, hi, false), (1, lo, true), (1, hi, false)] {
        if pts.is_empty() {
            break;
        }
        let coord = |p: &(f64, f64)| if axis == 0 { p.0 } else { p.1 };
        let inside = |p: &(f64, f64)| if greater { coord(p) >= v } else { coord(p) <= v };
        let input = std::mem::take(&mut pts);
        let mut prev = *input.last().unwrap();
        for &cur in &input {
            if inside(&cur) != inside(&prev) {
                let t = (v - coord(&prev)) / (coord(&cur) - coord(&prev));
                let ix = prev.0 + t * (cur.0 - prev.0);
                let iy = prev.1 + t * (cur.1 - prev.1);
                pts.push(if axis == 0 { (v, iy) } else { (ix, v) });
            }
            if inside(&cur) {
                pts.push(cur);
            }
            prev = cur;
        }
    }
    pts
}

/// Liang–Barsky clip of a segment against the square [lo, hi]².
fn clip_segment(a: (f64, f64), b: (f64, f64), (lo, hi): (f64, f64)) -> Option<((f64, f64), (f64, f64))> {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    for (p, q) in [(-dx, a.0 - lo), (dx, hi - a.0), (-dy, a.1 - lo), (dy, hi - a.1)] {
        if p == 0.0 {
            if q < 0.0 {
                return None;
            }
        } else {
            let t = q / p;
            if p < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return None;
            }
        }
    }
    Some(((a.0 + t0 * dx, a.1 + t0 * dy), (a.0 + t1 * dx, a.1 + t1 * dy)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_and_renders() {
        let m = get();
        assert!(m.layers.len() > 10);
        assert!(m.cities.len() > 5000);
        // A z=3 tile over western Europe should contain both land and sea.
        let px = m.render_tile(3, 3, 2, 256).unwrap();
        let land = px.chunks(4).filter(|p| p[..3] == LAND_RGB).count();
        let sea = px.chunks(4).filter(|p| p[..3] == OCEAN).count();
        assert!(land > 1000 && sea > 1000, "land={land} sea={sea}");
    }

    #[test]
    fn nearest_city_london() {
        let (c, d) = get().nearest_city(51.5007, -0.1246).unwrap();
        assert_eq!(c.name, "London");
        assert!(d < 10.0);
    }
}
