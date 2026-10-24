//! Converts Natural Earth GeoJSON layers into the compact basemap asset that
//! is embedded into the mappa-mundi binary (`assets/basemap.bin`).
//!
//! Usage: cargo run --release --example build_basemap -- <natural-earth-geojson-dir> [out-file]
//!
//! Everything is projected to normalised Web Mercator ([0,1] x [0,1], y down),
//! simplified per level of detail, and polygons are split into small quadtree
//! pieces so the app can rasterise any map tile by touching only a handful of
//! points.

use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

// Must match src/basemap.rs.
const MAGIC: &[u8; 4] = b"PMB1";
const FIXED: f64 = (1u64 << 24) as f64;

const LAND: u8 = 0;
const LAKE: u8 = 1;
const URBAN: u8 = 2;
const RIVER: u8 = 3;
const STATE: u8 = 4;
const BORDER: u8 = 5;
const ROAD: u8 = 6;

const POLY: u8 = 0;
const LINE: u8 = 1;

/// Max points per polygon piece before it is split into quadrants.
const MAX_PIECE_POINTS: usize = 384;
/// Max points per polyline chunk.
const MAX_LINE_POINTS: usize = 128;

type Pt = (f64, f64);
type Ring = Vec<Pt>;

struct Piece {
    min_zoom: f32,
    class: u8,
    rings: Vec<Ring>,
}

struct Layer {
    kind: u8,
    lod: u8,
    geom: u8,
    pieces: Vec<Piece>,
}

fn merc(lon: f64, lat: f64) -> Pt {
    let lat = lat.clamp(-85.051_128_78, 85.051_128_78);
    let x = (lon + 180.0) / 360.0;
    let s = lat.to_radians().sin();
    let y = 0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * std::f64::consts::PI);
    (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0))
}

/// Simplification tolerance (normalised units) for the zoom level a LOD is
/// expected to be displayed at.
fn tolerance(lod: u8) -> f64 {
    let z = match lod {
        0 => 2.0,
        1 => 5.0,
        _ => 9.5,
    };
    0.35 / (256.0 * f64::powf(2.0, z))
}

fn load(dir: &Path, name: &str) -> Vec<Value> {
    let path = dir.join(format!("{name}.geojson"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    let mut v: Value = serde_json::from_str(&text).expect("invalid geojson");
    match v["features"].take() {
        Value::Array(a) => a,
        _ => panic!("{name}: no features"),
    }
}

fn prop_f(p: &Value, keys: &[&str], default: f64) -> f64 {
    for k in keys {
        if let Some(x) = p.get(*k).and_then(Value::as_f64) {
            return x;
        }
    }
    default
}

fn prop_s<'a>(p: &'a Value, keys: &[&str]) -> &'a str {
    for k in keys {
        if let Some(s) = p.get(*k).and_then(Value::as_str) {
            return s;
        }
    }
    ""
}

fn coords_ring(v: &Value) -> Ring {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| {
                    let p = p.as_array()?;
                    Some(merc(p.first()?.as_f64()?, p.get(1)?.as_f64()?))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Returns polygons (each a list of rings) or lines, depending on geometry type.
fn polygons(geom: &Value) -> Vec<Vec<Ring>> {
    let c = &geom["coordinates"];
    match geom["type"].as_str() {
        Some("Polygon") => vec![c.as_array().unwrap().iter().map(coords_ring).collect()],
        Some("MultiPolygon") => c
            .as_array()
            .unwrap()
            .iter()
            .map(|poly| poly.as_array().unwrap().iter().map(coords_ring).collect())
            .collect(),
        _ => vec![],
    }
}

fn lines(geom: &Value) -> Vec<Ring> {
    let c = &geom["coordinates"];
    match geom["type"].as_str() {
        Some("LineString") => vec![coords_ring(c)],
        Some("MultiLineString") => c.as_array().unwrap().iter().map(coords_ring).collect(),
        _ => vec![],
    }
}

fn seg_dist2(p: Pt, a: Pt, b: Pt) -> f64 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (qx, qy) = (a.0 + t * dx - p.0, a.1 + t * dy - p.1);
    qx * qx + qy * qy
}

fn douglas_peucker(pts: &[Pt], tol: f64) -> Vec<Pt> {
    if pts.len() < 3 {
        return pts.to_vec();
    }
    let tol2 = tol * tol;
    let mut keep = vec![false; pts.len()];
    keep[0] = true;
    keep[pts.len() - 1] = true;
    let mut stack = vec![(0usize, pts.len() - 1)];
    while let Some((s, e)) = stack.pop() {
        let mut best = 0.0;
        let mut idx = 0;
        for i in s + 1..e {
            let d = seg_dist2(pts[i], pts[s], pts[e]);
            if d > best {
                best = d;
                idx = i;
            }
        }
        if best > tol2 {
            keep[idx] = true;
            stack.push((s, idx));
            stack.push((idx, e));
        }
    }
    pts.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

fn simplify_ring(ring: &Ring, tol: f64) -> Option<Ring> {
    if ring.len() < 4 {
        return None;
    }
    // Split the closed ring at its farthest point from the start so that DP
    // never collapses the whole ring onto a single segment.
    let start = ring[0];
    let far = (1..ring.len())
        .max_by(|&a, &b| {
            let da = (ring[a].0 - start.0).powi(2) + (ring[a].1 - start.1).powi(2);
            let db = (ring[b].0 - start.0).powi(2) + (ring[b].1 - start.1).powi(2);
            da.total_cmp(&db)
        })
        .unwrap();
    let mut a = douglas_peucker(&ring[..=far], tol);
    let b = douglas_peucker(&ring[far..], tol);
    a.extend_from_slice(&b[1..]);
    if a.len() < 4 {
        return None;
    }
    let (x0, y0, x1, y1) = bbox(std::slice::from_ref(&a));
    if (x1 - x0).max(y1 - y0) < tol * 2.0 {
        return None;
    }
    Some(a)
}

fn bbox(rings: &[Ring]) -> (f64, f64, f64, f64) {
    let mut b = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for r in rings {
        for &(x, y) in r {
            b.0 = b.0.min(x);
            b.1 = b.1.min(y);
            b.2 = b.2.max(x);
            b.3 = b.3.max(y);
        }
    }
    b
}

/// Sutherland–Hodgman clip of a (possibly concave) ring against an
/// axis-aligned rectangle. The output may contain degenerate edges along the
/// rectangle border, which is harmless for even-odd filling.
fn clip_ring(ring: &Ring, r: (f64, f64, f64, f64)) -> Ring {
    let mut out = ring.clone();
    // (axis, value, keep_greater)
    let edges = [(0, r.0, true), (0, r.2, false), (1, r.1, true), (1, r.3, false)];
    for (axis, v, greater) in edges {
        if out.is_empty() {
            break;
        }
        let inside = |p: &Pt| {
            let c = if axis == 0 { p.0 } else { p.1 };
            if greater { c >= v } else { c <= v }
        };
        let input = std::mem::take(&mut out);
        let mut prev = *input.last().unwrap();
        for &cur in &input {
            let (ci, pi) = (inside(&cur), inside(&prev));
            if ci != pi {
                let (pc, cc) = if axis == 0 { (prev.0, cur.0) } else { (prev.1, cur.1) };
                let t = (v - pc) / (cc - pc);
                let ix = prev.0 + t * (cur.0 - prev.0);
                let iy = prev.1 + t * (cur.1 - prev.1);
                out.push(if axis == 0 { (v, iy) } else { (ix, v) });
            }
            if ci {
                out.push(cur);
            }
            prev = cur;
        }
    }
    out
}

fn split_polygon(
    rings: Vec<Ring>,
    cell: (f64, f64, f64, f64),
    depth: u32,
    min_zoom: f32,
    class: u8,
    out: &mut Vec<Piece>,
) {
    let total: usize = rings.iter().map(Vec::len).sum();
    if total <= MAX_PIECE_POINTS || depth >= 12 {
        out.push(Piece { min_zoom, class, rings });
        return;
    }
    let mx = (cell.0 + cell.2) / 2.0;
    let my = (cell.1 + cell.3) / 2.0;
    let quads =
        [(cell.0, cell.1, mx, my), (mx, cell.1, cell.2, my), (cell.0, my, mx, cell.3), (mx, my, cell.2, cell.3)];
    for q in quads {
        let clipped: Vec<Ring> = rings.iter().map(|r| clip_ring(r, q)).filter(|r| r.len() >= 3).collect();
        if !clipped.is_empty() {
            split_polygon(clipped, q, depth + 1, min_zoom, class, out);
        }
    }
}

fn poly_layer(dir: &Path, name: &str, kind: u8, lod: u8, meta: impl Fn(&Value) -> Option<(f32, u8)>) -> Layer {
    let tol = tolerance(lod);
    let mut pieces = Vec::new();
    for f in load(dir, name) {
        let Some((min_zoom, class)) = meta(&f["properties"]) else { continue };
        for poly in polygons(&f["geometry"]) {
            let mut rings = poly.iter().filter_map(|r| simplify_ring(r, tol));
            let Some(outer) = rings.next() else { continue };
            let mut all = vec![outer];
            all.extend(rings);
            let b = bbox(&all);
            let cell = (0.0, 0.0, 1.0, 1.0);
            if b.2 > b.0 {
                split_polygon(all, cell, 0, min_zoom, class, &mut pieces);
            }
        }
    }
    report(name, &pieces);
    Layer { kind, lod, geom: POLY, pieces }
}

fn line_layer(dir: &Path, name: &str, kind: u8, lod: u8, meta: impl Fn(&Value) -> Option<(f32, u8)>) -> Layer {
    let tol = tolerance(lod);
    let mut pieces = Vec::new();
    for f in load(dir, name) {
        let Some((min_zoom, class)) = meta(&f["properties"]) else { continue };
        for line in lines(&f["geometry"]) {
            let s = douglas_peucker(&line, tol);
            if s.len() < 2 {
                continue;
            }
            let mut i = 0;
            while i + 1 < s.len() {
                let end = (i + MAX_LINE_POINTS).min(s.len() - 1);
                pieces.push(Piece { min_zoom, class, rings: vec![s[i..=end].to_vec()] });
                i = end;
            }
        }
    }
    report(name, &pieces);
    Layer { kind, lod, geom: LINE, pieces }
}

fn report(name: &str, pieces: &[Piece]) {
    let pts: usize = pieces.iter().flat_map(|p| &p.rings).map(Vec::len).sum();
    eprintln!("{name:45} {:7} pieces {:9} points", pieces.len(), pts);
}

fn min_zoom(p: &Value) -> f32 {
    prop_f(p, &["min_zoom", "MIN_ZOOM"], 0.0) as f32
}

struct Enc(Vec<u8>);

impl Enc {
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn f32(&mut self, v: f32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn var(&mut self, mut v: u64) {
        while v >= 0x80 {
            self.0.push((v as u8) | 0x80);
            v >>= 7;
        }
        self.0.push(v as u8);
    }
    fn svar(&mut self, v: i64) {
        self.var(((v << 1) ^ (v >> 63)) as u64);
    }
    fn str(&mut self, s: &str) {
        self.var(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
    fn zoom(&mut self, z: f32) {
        self.u8((z.clamp(0.0, 25.0) * 10.0).round() as u8);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let dir = PathBuf::from(args.get(1).map(String::as_str).unwrap_or("ne-data"));
    let out = PathBuf::from(args.get(2).map(String::as_str).unwrap_or("assets/basemap.bin"));
    let d = dir.as_path();

    let any = |p: &Value| Some((min_zoom(p), 0u8));
    let border_class = |p: &Value| {
        let c = prop_s(p, &["FEATURECLA", "featurecla"]).to_ascii_lowercase();
        let disputed = c.contains("disputed") || c.contains("indefinite") || c.contains("claim");
        Some((min_zoom(p), disputed as u8))
    };
    let road_class = |p: &Value| {
        let class = match prop_s(p, &["type"]) {
            "Major Highway" | "Beltway" => 2,
            "Secondary Highway" => 1,
            t if t.starts_with("Ferry") => 3,
            _ => 0,
        };
        Some((min_zoom(p), class))
    };

    let layers = vec![
        poly_layer(d, "ne_110m_land", LAND, 0, |_| Some((0.0, 0))),
        poly_layer(d, "ne_50m_land", LAND, 1, |_| Some((0.0, 0))),
        poly_layer(d, "ne_10m_land", LAND, 2, |_| Some((0.0, 0))),
        poly_layer(d, "ne_10m_minor_islands", LAND, 2, any),
        poly_layer(d, "ne_50m_lakes", LAKE, 1, any),
        poly_layer(d, "ne_10m_lakes", LAKE, 2, any),
        poly_layer(d, "ne_10m_urban_areas", URBAN, 2, any),
        line_layer(d, "ne_50m_rivers_lake_centerlines", RIVER, 1, any),
        line_layer(d, "ne_10m_rivers_lake_centerlines", RIVER, 2, any),
        line_layer(d, "ne_50m_admin_1_states_provinces_lines", STATE, 1, any),
        line_layer(d, "ne_10m_admin_1_states_provinces_lines", STATE, 2, any),
        line_layer(d, "ne_110m_admin_0_boundary_lines_land", BORDER, 0, border_class),
        line_layer(d, "ne_50m_admin_0_boundary_lines_land", BORDER, 1, border_class),
        line_layer(d, "ne_10m_admin_0_boundary_lines_land", BORDER, 2, border_class),
        line_layer(d, "ne_10m_roads", ROAD, 2, road_class),
    ];

    let mut e = Enc(Vec::new());
    e.0.extend_from_slice(MAGIC);
    e.u32(layers.len() as u32);
    for l in &layers {
        e.u8(l.kind);
        e.u8(l.lod);
        e.u8(l.geom);
        e.u32(l.pieces.len() as u32);
        for p in &l.pieces {
            e.zoom(p.min_zoom);
            e.u8(p.class);
            e.var(p.rings.len() as u64);
            for r in &p.rings {
                e.var(r.len() as u64);
                let (mut px, mut py) = (0i64, 0i64);
                for &(x, y) in r {
                    let (ix, iy) = ((x * FIXED).round() as i64, (y * FIXED).round() as i64);
                    e.svar(ix - px);
                    e.svar(iy - py);
                    (px, py) = (ix, iy);
                }
            }
        }
    }

    // Cities.
    let cities = load(d, "ne_10m_populated_places_simple");
    e.u32(cities.len() as u32);
    for c in &cities {
        let p = &c["properties"];
        let g = &c["geometry"]["coordinates"];
        let (x, y) = merc(g[0].as_f64().unwrap(), g[1].as_f64().unwrap());
        e.str(prop_s(p, &["name", "nameascii"]));
        e.str(prop_s(p, &["adm0name"]));
        e.str(prop_s(p, &["adm1name"]));
        e.f32(x as f32);
        e.f32(y as f32);
        e.zoom(min_zoom(p));
        e.u32(prop_f(p, &["pop_max"], 0.0).max(0.0) as u32);
        let capital = prop_s(p, &["featurecla"]).contains("capital") && prop_f(p, &["adm0cap"], 0.0) > 0.0;
        e.u8(capital as u8);
    }
    eprintln!("{:45} {:7}", "cities", cities.len());

    // Country labels.
    let countries = load(d, "ne_50m_admin_0_countries");
    e.u32(countries.len() as u32);
    for c in &countries {
        let p = &c["properties"];
        let (x, y) = merc(prop_f(p, &["LABEL_X"], 0.0), prop_f(p, &["LABEL_Y"], 0.0));
        e.str(prop_s(p, &["NAME_EN", "NAME"]));
        e.f32(x as f32);
        e.f32(y as f32);
        e.zoom(prop_f(p, &["MIN_LABEL"], 2.0) as f32);
        e.zoom(prop_f(p, &["MAX_LABEL"], 8.0) as f32);
    }

    let raw = e.0.len();
    let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::best());
    z.write_all(&e.0).unwrap();
    let bytes = z.finish().unwrap();
    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(&out, &bytes).unwrap();
    eprintln!("wrote {} ({:.1} MB, {:.1} MB uncompressed)", out.display(), bytes.len() as f64 / 1e6, raw as f64 / 1e6);
}
