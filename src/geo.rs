//! Web Mercator helpers. World coordinates are normalised to [0,1] x [0,1]
//! with y pointing south, matching the slippy-map tile scheme.

use std::f64::consts::PI;

pub const MAX_LAT: f64 = 85.051_128_78;

pub fn to_merc(lat: f64, lon: f64) -> (f64, f64) {
    let lat = lat.clamp(-MAX_LAT, MAX_LAT);
    let x = (lon + 180.0) / 360.0;
    let s = lat.to_radians().sin();
    let y = 0.5 - ((1.0 + s) / (1.0 - s)).ln() / (4.0 * PI);
    (x.rem_euclid(1.0), y.clamp(0.0, 1.0))
}

pub fn from_merc(x: f64, y: f64) -> (f64, f64) {
    let lon = x * 360.0 - 180.0;
    let n = PI - 2.0 * PI * y;
    let lat = n.sinh().atan().to_degrees();
    (lat, lon)
}

/// Great-circle distance in kilometres.
pub fn haversine_km(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let (p1, p2) = (lat1.to_radians(), lat2.to_radians());
    let dp = p2 - p1;
    let dl = (lon2 - lon1).to_radians();
    let a = (dp / 2.0).sin().powi(2) + p1.cos() * p2.cos() * (dl / 2.0).sin().powi(2);
    6371.0 * 2.0 * a.sqrt().asin()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mercator_round_trip() {
        for &(lat, lon) in &[(0.0, 0.0), (51.5072, -0.1276), (-33.86, 151.21), (64.1, -21.9)] {
            let (x, y) = to_merc(lat, lon);
            let (la, lo) = from_merc(x, y);
            assert!((la - lat).abs() < 1e-9 && (lo - lon).abs() < 1e-9, "{lat},{lon}");
        }
    }
}
