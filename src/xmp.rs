//! XMP sidecar files (`IMG_1234.HEIC.xmp` from icloudpd, `IMG_1234.xmp` from
//! Apple Photos or Lightroom). They carry what iCloud knows but the file
//! itself may not: locations added or corrected in Photos, adjusted dates,
//! captions and favourites.

use std::path::Path;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Xmp {
    pub lat: Option<f64>,
    pub lon: Option<f64>,
    /// Local wall-clock capture time as seconds since 1970.
    pub taken: Option<i64>,
    pub rating: Option<i32>,
    pub caption: Option<String>,
}

pub fn read(path: &Path) -> Option<Xmp> {
    let text = std::fs::read_to_string(path).ok()?;
    Some(parse(&text))
}

pub fn parse(xml: &str) -> Xmp {
    let lat = field(xml, "exif:GPSLatitude").and_then(|v| coord(&v, 'S'));
    let lon = field(xml, "exif:GPSLongitude").and_then(|v| coord(&v, 'W'));
    // (0, 0) means "no location", as in EXIF.
    let (lat, lon) = match (lat, lon) {
        (Some(a), Some(o)) if (a != 0.0 || o != 0.0) && a.abs() <= 90.0 && o.abs() <= 180.0 => (Some(a), Some(o)),
        _ => (None, None),
    };
    let taken = ["photoshop:DateCreated", "xmp:CreateDate", "exif:DateTimeOriginal"]
        .iter()
        .find_map(|t| field(xml, t).and_then(|v| date(&v)));
    let caption = ["dc:title", "dc:description"].iter().find_map(|t| field(xml, t)).filter(|s| !s.is_empty());
    Xmp { lat, lon, taken, rating: field(xml, "xmp:Rating").and_then(|v| v.parse().ok()), caption }
}

/// The value of a property written either as an element
/// (`<exif:GPSLatitude>51.5</exif:GPSLatitude>`, icloudpd) or as an
/// attribute (`exif:GPSLatitude="51,30.45N"`, Adobe/Apple). Language
/// alternatives (`<rdf:Alt><rdf:li …>text</rdf:li>`) are unwrapped.
fn field(xml: &str, name: &str) -> Option<String> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(i) = xml[from..].find(&open) {
        let start = from + i + open.len();
        from = start;
        // Make sure we matched the whole tag name, not a prefix of a longer one.
        match xml[start..].chars().next() {
            Some('>') => {}
            Some(c) if c.is_whitespace() => {}
            _ => continue,
        }
        let body_start = start + xml[start..].find('>')? + 1;
        if xml[..body_start].ends_with("/>") {
            continue;
        }
        let close = format!("</{name}>");
        let body = &xml[body_start..body_start + xml[body_start..].find(&close)?];
        let inner = match body.find("<rdf:li") {
            Some(li) => {
                let s = li + body[li..].find('>')? + 1;
                &body[s..s + body[s..].find("</rdf:li>")?]
            }
            None => body,
        };
        return Some(unescape(inner.trim()));
    }
    let attr = format!("{name}=\"");
    let i = xml.find(&attr)? + attr.len();
    Some(unescape(&xml[i..i + xml[i..].find('"')?]))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<").replace("&gt;", ">").replace("&quot;", "\"").replace("&apos;", "'").replace("&amp;", "&")
}

/// Signed decimal degrees ("-33.86") or the XMP spec's "DDD,MM.mmmK" /
/// "DDD,MM,SSK" forms with a hemisphere letter.
fn coord(s: &str, negative: char) -> Option<f64> {
    let s = s.trim();
    if let Ok(v) = s.parse::<f64>() {
        return v.is_finite().then_some(v);
    }
    let hemisphere = s.chars().last()?.to_ascii_uppercase();
    let parts: Vec<f64> = s[..s.len() - 1].split(',').map(|p| p.trim().parse().ok()).collect::<Option<_>>()?;
    let deg = match parts.as_slice() {
        [d, m] => d + m / 60.0,
        [d, m, sec] => d + m / 60.0 + sec / 3600.0,
        _ => return None,
    };
    Some(if hemisphere == negative { -deg } else { deg })
}

/// Keeps the wall-clock time the photo was taken, like EXIF does.
fn date(s: &str) -> Option<i64> {
    for fmt in ["%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%dT%H:%M:%S%:z", "%Y-%m-%dT%H:%M:%S%.f%z", "%Y-%m-%dT%H:%M:%S%.f%:z"] {
        if let Ok(d) = chrono::DateTime::parse_from_str(s, fmt) {
            return Some(d.naive_local().and_utc().timestamp());
        }
    }
    for fmt in ["%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M:%S%.f", "%Y-%m-%dT%H:%M"] {
        if let Ok(d) = chrono::NaiveDateTime::parse_from_str(s.trim_end_matches('Z'), fmt) {
            return Some(d.and_utc().timestamp());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped exactly like icloudpd 1.32's `generate_xml` output.
    const ICLOUDPD: &str = r#"<?xml version='1.0' encoding='utf-8'?>
<x:xml_doc xmlns:x="adobe:ns:meta/" x:xmptk="icloudpd 1.32.3+879c561"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#"><rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"><dc:title>Fish &amp; chips</dc:title><dc:subject><rdf:Seq><rdf:li>food</rdf:li></rdf:Seq></dc:subject></rdf:Description><rdf:Description rdf:about="" xmlns:exif="http://ns.adobe.com/exif/1.0/"><exif:GPSAltitude>12.5</exif:GPSAltitude><exif:GPSLatitude>53.2707</exif:GPSLatitude><exif:GPSLongitude>-9.0568</exif:GPSLongitude><exif:GPSTimeStamp>2023-08-15T14:55:56+0000</exif:GPSTimeStamp></rdf:Description><rdf:Description rdf:about="" xmlns:photoshop="http://ns.adobe.com/photoshop/1.0/"><photoshop:DateCreated>2023-08-15T15:55:56+0100</photoshop:DateCreated></rdf:Description><rdf:Description rdf:about="" xmlns:xmp="http://ns.adobe.com/xap/1.0/"><xmp:CreateDate>2023-08-15T15:55:56+0100</xmp:CreateDate><xmp:Rating>5</xmp:Rating></rdf:Description></rdf:RDF></x:xml_doc>"#;

    #[test]
    fn icloudpd_sidecar() {
        let x = parse(ICLOUDPD);
        assert_eq!((x.lat, x.lon), (Some(53.2707), Some(-9.0568)));
        assert_eq!(x.rating, Some(5));
        assert_eq!(x.caption.as_deref(), Some("Fish & chips"));
        let t = chrono::DateTime::from_timestamp(x.taken.unwrap(), 0).unwrap().naive_utc();
        assert_eq!(t.to_string(), "2023-08-15 15:55:56");
    }

    #[test]
    fn adobe_style_attributes() {
        let xml = r#"<rdf:Description rdf:about="" exif:GPSLatitude="33,51.6S" exif:GPSLongitude="151,12.6E"
            xmp:Rating="3" photoshop:DateCreated="2021-04-02T09:00:00"><dc:title><rdf:Alt>
            <rdf:li xml:lang="x-default">Harbour</rdf:li></rdf:Alt></dc:title></rdf:Description>"#;
        let x = parse(xml);
        assert!((x.lat.unwrap() + 33.86).abs() < 1e-9 && (x.lon.unwrap() - 151.21).abs() < 1e-9);
        assert_eq!(x.rating, Some(3));
        assert_eq!(x.caption.as_deref(), Some("Harbour"));
        assert!(x.taken.is_some());
    }

    #[test]
    fn no_location() {
        assert_eq!(parse("<x:xml_doc/>"), Xmp::default());
    }
}
