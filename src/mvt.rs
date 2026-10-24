//! Minimal Mapbox Vector Tile (protobuf) decoder: layers, features, the
//! handful of properties the street style uses, and geometry.

use anyhow::{bail, ensure};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GeomType {
    Point,
    Line,
    Polygon,
}

pub struct Feature {
    pub geom: GeomType,
    /// Points, line parts or polygon rings, in tile units (0..extent).
    pub parts: Vec<Vec<[f32; 2]>>,
    pub kind: String,
    pub kind_detail: String,
    pub name: Option<String>,
    pub min_zoom: f32,
    pub is_tunnel: bool,
    pub is_bridge: bool,
}

pub struct Layer {
    pub name: String,
    pub extent: f32,
    pub features: Vec<Feature>,
}

#[derive(Clone)]
enum Value {
    Str(String),
    Num(f64),
    Bool(bool),
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, pos: 0 }
    }

    fn done(&self) -> bool {
        self.pos >= self.b.len()
    }

    fn varint(&mut self) -> anyhow::Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let Some(&byte) = self.b.get(self.pos) else { bail!("truncated varint") };
            self.pos += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte < 0x80 {
                return Ok(v);
            }
        }
        bail!("varint too long")
    }

    fn bytes(&mut self, n: usize) -> anyhow::Result<&'a [u8]> {
        ensure!(self.pos + n <= self.b.len(), "truncated field");
        self.pos += n;
        Ok(&self.b[self.pos - n..self.pos])
    }

    /// Next (field number, payload). Varints come back as their value.
    fn field(&mut self) -> anyhow::Result<(u64, Field<'a>)> {
        let key = self.varint()?;
        let f = match key & 7 {
            0 => Field::Varint(self.varint()?),
            1 => Field::Bytes(self.bytes(8)?),
            2 => {
                let n = self.varint()? as usize;
                Field::Bytes(self.bytes(n)?)
            }
            5 => Field::Bytes(self.bytes(4)?),
            w => bail!("unsupported wire type {w}"),
        };
        Ok((key >> 3, f))
    }
}

enum Field<'a> {
    Varint(u64),
    Bytes(&'a [u8]),
}

fn packed(b: &[u8]) -> anyhow::Result<Vec<u32>> {
    let mut r = Reader::new(b);
    let mut out = Vec::with_capacity(b.len());
    while !r.done() {
        out.push(r.varint()? as u32);
    }
    Ok(out)
}

/// Decodes the layers whose names `want` accepts.
pub fn decode(tile: &[u8], want: impl Fn(&str) -> bool) -> anyhow::Result<Vec<Layer>> {
    let mut layers = Vec::new();
    let mut r = Reader::new(tile);
    while !r.done() {
        if let (3, Field::Bytes(b)) = r.field()?
            && let Some(l) = decode_layer(b, &want)?
        {
            layers.push(l);
        }
    }
    Ok(layers)
}

fn decode_layer(b: &[u8], want: &impl Fn(&str) -> bool) -> anyhow::Result<Option<Layer>> {
    let (mut name, mut extent) = (String::new(), 4096.0);
    let (mut keys, mut values, mut raw_features) = (Vec::new(), Vec::new(), Vec::new());
    let mut r = Reader::new(b);
    while !r.done() {
        match r.field()? {
            (1, Field::Bytes(s)) => {
                name = String::from_utf8_lossy(s).into_owned();
                if !want(&name) {
                    return Ok(None);
                }
            }
            (2, Field::Bytes(f)) => raw_features.push(f),
            (3, Field::Bytes(s)) => keys.push(String::from_utf8_lossy(s).into_owned()),
            (4, Field::Bytes(v)) => values.push(decode_value(v)?),
            (5, Field::Varint(e)) => extent = e as f32,
            _ => {}
        }
    }
    let key_index = |k: &str| keys.iter().position(|x| x == k).map(|i| i as u32);
    let k = [
        key_index("kind"),
        key_index("kind_detail"),
        key_index("name:en"),
        key_index("name"),
        key_index("min_zoom"),
        key_index("is_tunnel"),
        key_index("is_bridge"),
    ];
    let mut features = Vec::with_capacity(raw_features.len());
    for f in raw_features {
        features.push(decode_feature(f, &k, &values)?);
    }
    Ok(Some(Layer { name, extent, features }))
}

fn decode_value(b: &[u8]) -> anyhow::Result<Value> {
    let mut r = Reader::new(b);
    let mut v = Value::Bool(false);
    while !r.done() {
        v = match r.field()? {
            (1, Field::Bytes(s)) => Value::Str(String::from_utf8_lossy(s).into_owned()),
            (2, Field::Bytes(f)) => Value::Num(f32::from_le_bytes(f.try_into()?) as f64),
            (3, Field::Bytes(d)) => Value::Num(f64::from_le_bytes(d.try_into()?)),
            (4 | 5, Field::Varint(i)) => Value::Num(i as i64 as f64),
            (6, Field::Varint(i)) => Value::Num((((i >> 1) as i64) ^ -((i & 1) as i64)) as f64),
            (7, Field::Varint(i)) => Value::Bool(i != 0),
            _ => continue,
        };
    }
    Ok(v)
}

fn decode_feature(b: &[u8], k: &[Option<u32>; 7], values: &[Value]) -> anyhow::Result<Feature> {
    let (mut tags, mut geom_type, mut geometry) = (Vec::new(), 0, Vec::new());
    let mut r = Reader::new(b);
    while !r.done() {
        match r.field()? {
            (2, Field::Bytes(t)) => tags = packed(t)?,
            (3, Field::Varint(t)) => geom_type = t,
            (4, Field::Bytes(g)) => geometry = packed(g)?,
            _ => {}
        }
    }
    let prop = |key: Option<u32>| -> Option<&Value> {
        let key = key?;
        tags.as_chunks::<2>().0.iter().find(|kv| kv[0] == key).and_then(|kv| values.get(kv[1] as usize))
    };
    let string = |key| match prop(key) {
        Some(Value::Str(s)) => Some(s.clone()),
        _ => None,
    };
    let flag = |key| matches!(prop(key), Some(Value::Bool(true)));
    let geom = match geom_type {
        1 => GeomType::Point,
        2 => GeomType::Line,
        _ => GeomType::Polygon,
    };
    Ok(Feature {
        geom,
        parts: decode_geometry(&geometry, geom)?,
        kind: string(k[0]).unwrap_or_default(),
        kind_detail: string(k[1]).unwrap_or_default(),
        name: string(k[2]).or_else(|| string(k[3])),
        min_zoom: match prop(k[4]) {
            Some(Value::Num(z)) => *z as f32,
            _ => 0.0,
        },
        is_tunnel: flag(k[5]),
        is_bridge: flag(k[6]),
    })
}

fn decode_geometry(cmds: &[u32], geom: GeomType) -> anyhow::Result<Vec<Vec<[f32; 2]>>> {
    let (mut x, mut y) = (0i32, 0i32);
    let mut parts: Vec<Vec<[f32; 2]>> = Vec::new();
    let mut i = 0;
    let zigzag = |v: u32| ((v >> 1) as i32) ^ -((v & 1) as i32);
    while i < cmds.len() {
        let (id, count) = (cmds[i] & 7, (cmds[i] >> 3) as usize);
        i += 1;
        match id {
            1 | 2 => {
                ensure!(i + 2 * count <= cmds.len(), "truncated geometry");
                for c in 0..count {
                    x += zigzag(cmds[i + 2 * c]);
                    y += zigzag(cmds[i + 2 * c + 1]);
                    // Each MoveTo starts a new part (for points, each point is one).
                    if id == 1 || parts.is_empty() {
                        parts.push(Vec::new());
                    }
                    parts.last_mut().unwrap().push([x as f32, y as f32]);
                }
                i += 2 * count;
            }
            7 => {
                if let Some(p) = parts.last_mut()
                    && let Some(&first) = p.first()
                {
                    p.push(first);
                }
            }
            _ => bail!("unknown geometry command {id}"),
        }
    }
    if geom == GeomType::Point {
        parts = parts.into_iter().flatten().map(|p| vec![p]).collect();
    }
    Ok(parts)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_commands() {
        // From the MVT spec: a line MoveTo(2,2) LineTo(2,10) LineTo(10,10).
        let line = decode_geometry(&[9, 4, 4, 18, 0, 16, 16, 0], GeomType::Line).unwrap();
        assert_eq!(line, vec![vec![[2.0, 2.0], [2.0, 10.0], [10.0, 10.0]]]);
        // A polygon ring is closed by ClosePath.
        let poly = decode_geometry(&[9, 6, 12, 18, 10, 12, 24, 44, 15], GeomType::Polygon).unwrap();
        assert_eq!(poly, vec![vec![[3.0, 6.0], [8.0, 12.0], [20.0, 34.0], [3.0, 6.0]]]);
    }
}
