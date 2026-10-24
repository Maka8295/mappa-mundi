//! Street maps: OpenStreetMap vector tiles (the Protomaps basemap schema)
//! drawn in the dungeonOS stone style, plus the names to label them with.

use tiny_skia::{Color, FillRule, LineCap, LineJoin, Paint, PathBuilder, Pixmap, Stroke, StrokeDash, Transform};

use crate::basemap::{self, clip_ring, clip_segment};
use crate::mvt::{Feature, GeomType, Layer};

const LAND: [u8; 3] = basemap::LAND_RGB;
const WATER: [u8; 3] = basemap::OCEAN;
const GREEN: [u8; 3] = [0x2C, 0x32, 0x26];
const PAVED: [u8; 3] = [0x38, 0x31, 0x2A];
const CIVIC: [u8; 3] = [0x32, 0x2B, 0x24];
const WORKS: [u8; 3] = [0x2F, 0x29, 0x24];
const SAND: [u8; 3] = [0x3B, 0x34, 0x29];
const BUILDING: [u8; 3] = [0x3F, 0x36, 0x2D];
const BUILDING_EDGE: [u8; 3] = [0x4D, 0x43, 0x38];
const CASING: [u8; 3] = [0x19, 0x17, 0x14];
const HIGHWAY: [u8; 3] = [0xA8, 0x8A, 0x5B];
const MAJOR: [u8; 3] = [0x8A, 0x6B, 0x4F];
const SECONDARY: [u8; 3] = [0x75, 0x5C, 0x46];
const MINOR: [u8; 3] = [0x5C, 0x52, 0x46];
const SERVICE: [u8; 3] = [0x4A, 0x42, 0x39];
const PEDESTRIAN: [u8; 3] = [0x52, 0x49, 0x3E];
const PATH: [u8; 3] = [0x6F, 0x67, 0x5E];
const RAIL: [u8; 3] = [0x8F, 0x8B, 0x81];
const BORDER: [u8; 3] = [0x7E, 0x70, 0x5D];

/// A street map tile at display zoom `z` (possibly past the data's deepest
/// level, `dz`; the data is then scaled up, staying sharp).
pub struct TileRef {
    pub z: u8,
    pub x: u32,
    pub y: u32,
    pub dz: u8,
    pub dx: u32,
    pub dy: u32,
}

impl TileRef {
    pub fn new(z: u8, x: u32, y: u32, max_data_zoom: u8) -> Self {
        let dz = z.min(max_data_zoom);
        let d = z - dz;
        Self { z, x, y, dz, dx: x >> d, dy: y >> d }
    }
}

pub struct PlaceLabel {
    pub name: String,
    /// Cities and towns, as opposed to neighbourhoods and the like.
    pub major: bool,
    pub min_zoom: f32,
    pub x: f64,
    pub y: f64,
}

pub struct RoadLabel {
    pub name: String,
    /// Higher is more important.
    pub rank: u8,
    /// Normalised Web Mercator points.
    pub points: Vec<[f64; 2]>,
}

#[derive(Default)]
pub struct Labels {
    pub places: Vec<PlaceLabel>,
    pub roads: Vec<RoadLabel>,
}

/// Which layers [`render`] and [`labels`] use.
pub fn wanted_layer(name: &str) -> bool {
    matches!(name, "earth" | "landuse" | "water" | "buildings" | "roads" | "boundaries" | "places")
}

struct Road {
    rank: u8,
    width: f32,
    color: [u8; 3],
    casing: bool,
    dash: Option<[f32; 2]>,
}

/// Exponential interpolation between (zoom, width) stops.
fn ramp(z: f32, stops: &[(f32, f32)]) -> f32 {
    if z <= stops[0].0 {
        return stops[0].1;
    }
    for w in stops.windows(2) {
        let ((z0, a), (z1, b)) = (w[0], w[1]);
        if z <= z1 {
            return a * (b / a).powf((z - z0) / (z1 - z0));
        }
    }
    stops[stops.len() - 1].1
}

fn road_style(f: &Feature, z: f32) -> Option<Road> {
    let road = |rank, stops: &[(f32, f32)], color, casing: bool| {
        let casing = (casing || f.is_bridge) && !f.is_tunnel;
        Some(Road { rank, width: ramp(z, stops), color, casing, dash: None })
    };
    let style = match (f.kind.as_str(), f.kind_detail.as_str()) {
        ("highway", _) => road(
            5,
            &[(9.0, 1.0), (12.0, 1.8), (14.0, 3.5), (16.0, 9.0), (18.0, 24.0), (20.0, 72.0)],
            HIGHWAY,
            z >= 11.0,
        ),
        ("major_road", "primary" | "trunk") => {
            road(4, &[(9.0, 0.6), (12.0, 1.4), (14.0, 3.0), (16.0, 8.0), (18.0, 20.0), (20.0, 60.0)], MAJOR, z >= 13.0)
        }
        ("major_road" | "medium_road", _) if z >= 10.0 => road(
            3,
            &[(10.0, 0.5), (13.0, 1.4), (15.0, 4.0), (17.0, 10.0), (19.0, 28.0), (20.0, 50.0)],
            SECONDARY,
            z >= 14.0,
        ),
        ("minor_road", "service") if z >= 14.0 => {
            road(1, &[(14.0, 0.6), (16.0, 2.5), (18.0, 7.0), (20.0, 20.0)], SERVICE, false)
        }
        ("minor_road", _) if z >= 12.0 => {
            road(2, &[(12.0, 0.5), (14.0, 1.6), (16.0, 5.0), (18.0, 15.0), (20.0, 45.0)], MINOR, z >= 15.0)
        }
        // Pedestrian streets (old towns, shopping streets) are streets, not footpaths.
        ("path", "pedestrian") if z >= 13.0 => {
            road(2, &[(13.0, 0.5), (14.0, 1.4), (16.0, 4.5), (18.0, 14.0), (20.0, 42.0)], PEDESTRIAN, false)
        }
        ("path", _) if z >= 15.0 => Some(Road {
            rank: 0,
            width: ramp(z, &[(15.0, 0.7), (18.0, 1.6), (20.0, 3.0)]),
            color: PATH,
            casing: false,
            dash: Some([2.0, 2.0]),
        }),
        ("rail", "subway") => None,
        ("rail", _) if z >= 11.0 => Some(Road {
            rank: 1,
            width: ramp(z, &[(11.0, 0.5), (16.0, 1.2), (20.0, 2.5)]),
            color: RAIL,
            casing: false,
            dash: Some([4.0, 3.0]),
        }),
        _ => None,
    }?;
    // Tunnels: faded, dashed, beneath everything else on the surface.
    Some(if f.is_tunnel {
        Road { rank: 0, color: mix(style.color, LAND, 0.55), dash: Some([3.0, 2.0]), ..style }
    } else {
        style
    })
}

fn landuse_color(kind: &str) -> Option<[u8; 3]> {
    Some(match kind {
        "park" | "garden" | "grass" | "forest" | "wood" | "nature_reserve" | "cemetery" | "golf_course"
        | "recreation_ground" | "playground" | "pitch" | "meadow" | "village_green" | "allotments" | "scrub"
        | "grassland" | "national_park" | "protected_area" | "zoo" | "dog_park" => GREEN,
        "pedestrian" | "platform" | "pier" | "parking" | "plaza" | "runway" | "taxiway" | "apron" => PAVED,
        "hospital" | "school" | "university" | "college" | "kindergarten" | "place_of_worship" => CIVIC,
        "industrial" | "commercial" | "retail" | "railway" | "military" | "brownfield" | "aerodrome" => WORKS,
        "beach" | "sand" | "bare_rock" | "quarry" => SAND,
        _ => return None,
    })
}

fn polys(fs: &[Feature]) -> impl Iterator<Item = &Feature> {
    fs.iter().filter(|f| f.geom == GeomType::Polygon)
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    std::array::from_fn(|i| (a[i] as f32 * (1.0 - t) + b[i] as f32 * t) as u8)
}

fn paint(rgb: [u8; 3]) -> Paint<'static> {
    let mut p = Paint::default();
    p.set_color_rgba8(rgb[0], rgb[1], rgb[2], 255);
    p.anti_alias = true;
    p
}

struct Canvas<'a> {
    pm: &'a mut Pixmap,
    /// Tile units → pixels.
    scale: f32,
    off: (f32, f32),
    size: f32,
    /// One point (1/256 of a tile edge) in pixels.
    pt: f32,
}

impl Canvas<'_> {
    fn px(&self, p: [f32; 2]) -> (f64, f64) {
        ((p[0] * self.scale - self.off.0) as f64, (p[1] * self.scale - self.off.1) as f64)
    }

    fn fill(&mut self, parts: &[&Vec<[f32; 2]>], rgb: [u8; 3]) {
        let mut pb = PathBuilder::new();
        let lim = (-4.0, self.size as f64 + 4.0);
        for ring in parts {
            let pts = clip_ring(ring.iter().map(|&p| self.px(p)).collect(), lim);
            if pts.len() < 3 {
                continue;
            }
            pb.move_to(pts[0].0 as f32, pts[0].1 as f32);
            for p in &pts[1..] {
                pb.line_to(p.0 as f32, p.1 as f32);
            }
            pb.close();
        }
        if let Some(path) = pb.finish() {
            self.pm.fill_path(&path, &paint(rgb), FillRule::EvenOdd, Transform::identity(), None);
        }
    }

    fn stroke(&mut self, parts: &[&Vec<[f32; 2]>], rgb: [u8; 3], width_pt: f32, dash: Option<[f32; 2]>, round: bool) {
        let width = width_pt * self.pt;
        let mut pb = PathBuilder::new();
        let m = width as f64 + 2.0;
        let lim = (-m, self.size as f64 + m);
        for line in parts {
            let mut last = None;
            for w in line.windows(2) {
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
        let Some(path) = pb.finish() else { return };
        let cap = if round { LineCap::Round } else { LineCap::Butt };
        let stroke = Stroke {
            width,
            line_cap: cap,
            line_join: LineJoin::Round,
            dash: dash.and_then(|[a, b]| StrokeDash::new(vec![a * self.pt, b * self.pt], 0.0)),
            ..Default::default()
        };
        self.pm.stroke_path(&path, &paint(rgb), &stroke, Transform::identity(), None);
    }
}

/// Draws tile `t` from the decoded data tile `layers` as a `size`² image
/// (premultiplied RGBA).
pub fn render(layers: &[Layer], t: &TileRef, size: u32) -> Option<Vec<u8>> {
    let mut pm = Pixmap::new(size, size)?;
    pm.fill(Color::from_rgba8(WATER[0], WATER[1], WATER[2], 255));
    let extent = layers.first().map_or(4096.0, |l| l.extent);
    let d = (t.z - t.dz) as i32;
    let scale = size as f32 * (1 << d) as f32 / extent;
    let off = (((t.x - (t.dx << d)) * size) as f32, ((t.y - (t.dy << d)) * size) as f32);
    let mut c = Canvas { pm: &mut pm, scale, off, size: size as f32, pt: size as f32 / 256.0 };
    let z = t.z as f32;
    let layer = |name: &str| layers.iter().find(|l| l.name == name).map(|l| l.features.as_slice()).unwrap_or(&[]);

    let earth: Vec<_> = polys(layer("earth")).flat_map(|f| &f.parts).collect();
    c.fill(&earth, LAND);
    for f in polys(layer("landuse")) {
        if let Some(rgb) = landuse_color(&f.kind) {
            c.fill(&f.parts.iter().collect::<Vec<_>>(), rgb);
        }
    }
    let water: Vec<_> = polys(layer("water")).flat_map(|f| &f.parts).collect();
    c.fill(&water, WATER);
    for f in layer("water").iter().filter(|f| f.geom == GeomType::Line) {
        let w = match f.kind.as_str() {
            "river" => ramp(z, &[(10.0, 0.8), (14.0, 3.0), (18.0, 12.0)]),
            "canal" => ramp(z, &[(12.0, 0.8), (16.0, 3.0), (18.0, 8.0)]),
            _ if z >= 14.0 => ramp(z, &[(14.0, 0.6), (18.0, 3.0)]),
            _ => continue,
        };
        c.stroke(&f.parts.iter().collect::<Vec<_>>(), WATER, w, None, true);
    }
    if z >= 14.0 {
        let blocks: Vec<_> = polys(layer("buildings")).flat_map(|f| &f.parts).collect();
        c.fill(&blocks, BUILDING);
        if z >= 15.0 {
            c.stroke(&blocks, BUILDING_EDGE, 0.35 * (z - 14.0).min(3.0), None, false);
        }
    }

    // Roads: all casings first, then fills from least to most important,
    // so junctions join cleanly and big roads sit on top.
    let mut roads: Vec<(&Feature, Road)> = layer("roads")
        .iter()
        .filter(|f| f.geom == GeomType::Line)
        .filter_map(|f| Some((f, road_style(f, z)?)))
        .collect();
    // Tunnels underneath, bridges on top.
    roads.sort_by_key(|(f, r)| (!f.is_tunnel, f.is_bridge, r.rank));
    for (f, r) in roads.iter().filter(|(_, r)| r.casing) {
        let extra = (r.width * 0.25).clamp(0.6, 2.5);
        c.stroke(&f.parts.iter().collect::<Vec<_>>(), CASING, r.width + 2.0 * extra, None, true);
    }
    for (f, r) in &roads {
        c.stroke(&f.parts.iter().collect::<Vec<_>>(), r.color, r.width, r.dash, r.dash.is_none());
    }

    for f in layer("boundaries") {
        let (w, dash) = match f.kind.as_str() {
            "country" => (1.4, None),
            "region" if z < 13.0 => (0.8, Some([3.0, 2.0])),
            _ => continue,
        };
        c.stroke(&f.parts.iter().collect::<Vec<_>>(), BORDER, w, dash, true);
    }
    Some(pm.take())
}

/// Place and street names in a data tile, in world coordinates.
pub fn labels(layers: &[Layer], dz: u8, dx: u32, dy: u32) -> Labels {
    let mut out = Labels::default();
    let n = (1u64 << dz) as f64;
    for l in layers {
        let world = |p: [f32; 2]| {
            [(dx as f64 + p[0] as f64 / l.extent as f64) / n, (dy as f64 + p[1] as f64 / l.extent as f64) / n]
        };
        match l.name.as_str() {
            "places" => {
                for f in &l.features {
                    let (Some(name), Some(p)) = (&f.name, f.parts.first().and_then(|p| p.first())) else { continue };
                    let [x, y] = world(*p);
                    let major = matches!(f.kind_detail.as_str(), "city" | "town")
                        || f.kind == "locality" && f.kind_detail.is_empty();
                    out.places.push(PlaceLabel { name: name.clone(), major, min_zoom: f.min_zoom, x, y });
                }
            }
            "roads" if dz >= 13 => {
                for f in &l.features {
                    let Some(name) = &f.name else { continue };
                    let rank = match f.kind.as_str() {
                        "highway" => 4,
                        "major_road" => 3,
                        "minor_road" if f.kind_detail != "service" => 2,
                        "path" if f.kind_detail == "pedestrian" => 1,
                        _ => continue,
                    };
                    for part in &f.parts {
                        let points = part.iter().map(|&p| world(p)).collect();
                        out.roads.push(RoadLabel { name: name.clone(), rank, points });
                    }
                }
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ramps_interpolate_exponentially() {
        let stops = [(10.0, 1.0), (12.0, 4.0)];
        assert_eq!(ramp(9.0, &stops), 1.0);
        assert!((ramp(11.0, &stops) - 2.0).abs() < 1e-6);
        assert_eq!(ramp(13.0, &stops), 4.0);
    }

    #[test]
    fn overzoom_maps_to_the_parent_data_tile() {
        let t = TileRef::new(18, 130_960, 87_170, 15);
        assert_eq!((t.dz, t.dx, t.dy), (15, 16_370, 10_896));
    }
}
