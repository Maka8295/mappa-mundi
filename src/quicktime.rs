//! Minimal QuickTime / MP4 metadata reader: just enough to place iPhone
//! videos on the map (ISO 6709 location, creation date, camera model).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct QtMeta {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// Local wall-clock capture time as seconds since 1970.
    pub created: Option<i64>,
    pub model: Option<String>,
    pub duration: Option<f32>,
}

/// Seconds between 1904-01-01 (QuickTime epoch) and 1970-01-01.
const QT_EPOCH_OFFSET: i64 = 2_082_844_800;

pub fn read(path: &Path) -> std::io::Result<QtMeta> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    let mut pos = 0u64;
    while pos + 8 <= len {
        f.seek(SeekFrom::Start(pos))?;
        let mut hdr = [0u8; 16];
        f.read_exact(&mut hdr[..8])?;
        let mut size = u32::from_be_bytes(hdr[0..4].try_into().unwrap()) as u64;
        let mut header_len = 8;
        if size == 1 {
            f.read_exact(&mut hdr[8..16])?;
            size = u64::from_be_bytes(hdr[8..16].try_into().unwrap());
            header_len = 16;
        } else if size == 0 {
            size = len - pos;
        }
        if size < header_len {
            break;
        }
        if &hdr[4..8] == b"moov" {
            let body_len = size - header_len;
            if body_len > 256 << 20 {
                break;
            }
            let mut body = vec![0u8; body_len as usize];
            f.read_exact(&mut body)?;
            return Ok(parse_moov(&body));
        }
        pos += size;
    }
    Ok(QtMeta::default())
}

/// Splits a buffer into its child boxes.
fn boxes(mut data: &[u8]) -> Vec<([u8; 4], &[u8])> {
    let mut out = Vec::new();
    while data.len() >= 8 {
        let mut size = u32::from_be_bytes(data[0..4].try_into().unwrap()) as usize;
        let typ: [u8; 4] = data[4..8].try_into().unwrap();
        let mut header = 8;
        if size == 1 && data.len() >= 16 {
            size = u64::from_be_bytes(data[8..16].try_into().unwrap()) as usize;
            header = 16;
        } else if size == 0 {
            size = data.len();
        }
        if size < header || size > data.len() {
            break;
        }
        out.push((typ, &data[header..size]));
        data = &data[size..];
    }
    out
}

fn be_u32(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?))
}

fn be_u64(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn parse_moov(body: &[u8]) -> QtMeta {
    let mut m = QtMeta::default();
    let mut utc_created = None;
    for (typ, b) in boxes(body) {
        match &typ {
            b"mvhd" => {
                let (created, timescale, duration) = if b.first() == Some(&1) {
                    (be_u64(b, 4), be_u32(b, 20), be_u64(b, 24))
                } else {
                    (be_u32(b, 4).map(u64::from), be_u32(b, 12), be_u32(b, 16).map(u64::from))
                };
                utc_created = created.filter(|&c| c > 0).map(|c| c as i64 - QT_EPOCH_OFFSET);
                if let (Some(ts), Some(d)) = (timescale, duration)
                    && ts > 0
                {
                    m.duration = Some(d as f32 / ts as f32);
                }
            }
            b"meta" => parse_meta(b, &mut m),
            b"udta" => {
                for (t, b) in boxes(b) {
                    match &t {
                        b"\xa9xyz" if b.len() > 4 => {
                            set_location(&mut m, &String::from_utf8_lossy(&b[4..]));
                        }
                        b"loci" if m.lat.is_none() => {
                            if let Some((lat, lon)) = parse_loci(b) {
                                m.lat = Some(lat);
                                m.lon = Some(lon);
                            }
                        }
                        b"meta" => parse_meta(b, &mut m),
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    if m.created.is_none() {
        m.created = utc_created;
    }
    m
}

fn parse_meta(b: &[u8], m: &mut QtMeta) {
    // QuickTime 'meta' boxes start straight with children; ISO ones are
    // full boxes with a 4-byte version/flags prefix.
    let b = if b.get(4..8) == Some(b"hdlr") { b } else { b.get(4..).unwrap_or_default() };
    let children = boxes(b);
    let mut keys: Vec<String> = Vec::new();
    for (t, kb) in &children {
        if t == b"keys" {
            let mut rest = kb.get(8..).unwrap_or_default();
            while rest.len() >= 8 {
                let size = u32::from_be_bytes(rest[0..4].try_into().unwrap()) as usize;
                if size < 8 || size > rest.len() {
                    break;
                }
                keys.push(String::from_utf8_lossy(&rest[8..size]).into_owned());
                rest = &rest[size..];
            }
        }
    }
    for (t, ib) in &children {
        if t != b"ilst" {
            continue;
        }
        for (item_type, item) in boxes(ib) {
            let key = match &item_type {
                b"\xa9xyz" => "com.apple.quicktime.location.ISO6709".to_string(),
                b"\xa9day" => "com.apple.quicktime.creationdate".to_string(),
                _ => {
                    let idx = u32::from_be_bytes(item_type) as usize;
                    match keys.get(idx.wrapping_sub(1)) {
                        Some(k) => k.clone(),
                        None => continue,
                    }
                }
            };
            let Some((_, data)) = boxes(item).into_iter().find(|(t, _)| t == b"data") else {
                continue;
            };
            let value = String::from_utf8_lossy(data.get(8..).unwrap_or_default());
            let value = value.trim_end_matches('\0').trim();
            match key.as_str() {
                "com.apple.quicktime.location.ISO6709" => set_location(m, value),
                "com.apple.quicktime.creationdate" => m.created = m.created.or(parse_date(value)),
                "com.apple.quicktime.model" if !value.is_empty() => m.model = Some(value.to_string()),
                _ => {}
            }
        }
    }
}

fn set_location(m: &mut QtMeta, s: &str) {
    if m.lat.is_some() {
        return;
    }
    if let Some((lat, lon)) = parse_iso6709(s) {
        m.lat = Some(lat);
        m.lon = Some(lon);
    }
}

/// Parses decimal-degree ISO 6709 strings such as `+51.5072-000.1276+011.000/`.
pub fn parse_iso6709(s: &str) -> Option<(f64, f64)> {
    let mut nums = Vec::new();
    let mut cur = String::new();
    for c in s.chars() {
        if (c == '+' || c == '-') && !cur.is_empty() {
            nums.push(std::mem::take(&mut cur));
        }
        if c == '/' {
            break;
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        nums.push(cur);
    }
    let lat: f64 = nums.first()?.parse().ok()?;
    let lon: f64 = nums.get(1)?.parse().ok()?;
    ((-90.0..=90.0).contains(&lat) && (-180.0..=180.0).contains(&lon)).then_some((lat, lon))
}

/// 3GPP location box: version/flags, language, NUL-terminated name, role,
/// then longitude, latitude and altitude as signed 16.16 fixed point.
fn parse_loci(b: &[u8]) -> Option<(f64, f64)> {
    let name_end = 6 + b.get(6..)?.iter().position(|&c| c == 0)?;
    let at = name_end + 2;
    let fixed = |i: usize| Some(i32::from_be_bytes(b.get(i..i + 4)?.try_into().ok()?) as f64 / 65536.0);
    let (lon, lat) = (fixed(at)?, fixed(at + 4)?);
    let valid = lat.abs() <= 90.0 && lon.abs() <= 180.0 && (lat != 0.0 || lon != 0.0);
    valid.then_some((lat, lon))
}

/// Parses an ISO 8601 creation date, keeping the local wall-clock time.
fn parse_date(s: &str) -> Option<i64> {
    for fmt in ["%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M:%S%:z"] {
        if let Ok(d) = chrono::DateTime::parse_from_str(s, fmt) {
            return Some(d.naive_local().and_utc().timestamp());
        }
    }
    chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S").ok().map(|d| d.and_utc().timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso6709() {
        assert_eq!(parse_iso6709("+51.5072-000.1276+011.000/"), Some((51.5072, -0.1276)));
        assert_eq!(parse_iso6709("-33.8688+151.2093/"), Some((-33.8688, 151.2093)));
        assert_eq!(parse_iso6709("garbage"), None);
    }

    #[test]
    fn loci_box() {
        let mut b = vec![0, 0, 0, 0, 0x15, 0xc7, 0];
        b.push(0); // role
        b.extend_from_slice(&((139.7454 * 65536.0) as i32).to_be_bytes());
        b.extend_from_slice(&((-35.6586 * 65536.0) as i32).to_be_bytes());
        b.extend_from_slice(&0i32.to_be_bytes());
        let (lat, lon) = parse_loci(&b).unwrap();
        assert!((lat + 35.6586).abs() < 1e-4 && (lon - 139.7454).abs() < 1e-4);
    }

    #[test]
    fn creation_date_keeps_wall_clock() {
        let t = parse_date("2023-06-01T14:22:33+0100").unwrap();
        let d = chrono::DateTime::from_timestamp(t, 0).unwrap().naive_utc();
        assert_eq!(d.to_string(), "2023-06-01 14:22:33");
    }
}
