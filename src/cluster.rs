//! Hierarchical, zoom-dependent grouping of photos into map markers.
//!
//! Every integer zoom level gets its own set of clusters, built bottom-up by
//! greedily merging the previous level's clusters that would overlap on
//! screen. Panning or zooming then only needs to pick a level.

use std::collections::HashMap;

use crate::library::Item;

pub const MAX_LEVEL: usize = 21;

pub struct Cluster {
    /// Weighted centre in normalised Mercator coordinates.
    pub x: f64,
    pub y: f64,
    /// Item indices, newest first.
    pub members: Vec<u32>,
}

impl Cluster {
    /// The photo shown on the marker: the most recent one.
    pub fn cover(&self) -> u32 {
        self.members[0]
    }
}

pub struct Clusters {
    levels: Vec<Vec<Cluster>>,
}

impl Clusters {
    /// `radius` is the minimum on-screen distance (in points) between marker
    /// centres at each level's zoom. Items must be sorted newest first.
    pub fn build(items: &[Item], radius: f64) -> Self {
        let mut levels: Vec<Vec<Cluster>> = Vec::with_capacity(MAX_LEVEL + 1);
        let base: Vec<Cluster> =
            items.iter().enumerate().map(|(i, it)| Cluster { x: it.x, y: it.y, members: vec![i as u32] }).collect();
        for z in (0..=MAX_LEVEL).rev() {
            let prev = levels.last().unwrap_or(&base);
            let r = radius / (256.0 * (z as f64).exp2());
            let next = merge(prev, r);
            levels.push(next);
        }
        levels.reverse();
        Self { levels }
    }

    pub fn at_zoom(&self, zoom: f64) -> &[Cluster] {
        let level = (zoom.floor().max(0.0) as usize).min(MAX_LEVEL);
        &self.levels[level]
    }
}

fn merge(prev: &[Cluster], r: f64) -> Vec<Cluster> {
    let cell = |c: &Cluster| ((c.x / r).floor() as i64, (c.y / r).floor() as i64);
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, c) in prev.iter().enumerate() {
        grid.entry(cell(c)).or_default().push(i);
    }
    // Large clusters absorb their neighbours first, which keeps markers stable.
    let mut order: Vec<usize> = (0..prev.len()).collect();
    order.sort_by_key(|&i| std::cmp::Reverse(prev[i].members.len()));

    let mut used = vec![false; prev.len()];
    let mut out = Vec::new();
    for &i in &order {
        if used[i] {
            continue;
        }
        used[i] = true;
        let seed = &prev[i];
        let (cx, cy) = cell(seed);
        let mut members = seed.members.clone();
        let mut w = seed.members.len() as f64;
        let (mut sx, mut sy) = (seed.x * w, seed.y * w);
        let mut merged_any = false;
        for dx in -1..=1 {
            for dy in -1..=1 {
                let Some(bucket) = grid.get(&(cx + dx, cy + dy)) else { continue };
                for &j in bucket {
                    let c = &prev[j];
                    if used[j] || (c.x - seed.x).powi(2) + (c.y - seed.y).powi(2) > r * r {
                        continue;
                    }
                    used[j] = true;
                    merged_any = true;
                    let n = c.members.len() as f64;
                    sx += c.x * n;
                    sy += c.y * n;
                    w += n;
                    members.extend_from_slice(&c.members);
                }
            }
        }
        if merged_any {
            members.sort_unstable();
        }
        out.push(Cluster { x: sx / w, y: sy / w, members });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::library::Kind;

    pub(super) fn item(lat: f64, lon: f64) -> Item {
        let (x, y) = crate::geo::to_merc(lat, lon);
        Item {
            path: Default::default(),
            kind: Kind::Photo,
            edited: None,
            live: None,
            lat,
            lon,
            x,
            y,
            taken: None,
            model: None,
            orientation: 1,
            width: 0,
            height: 0,
            duration: None,
            caption: None,
            favorite: false,
            key: 0,
        }
    }

    #[test]
    fn merges_when_zoomed_out_and_splits_when_zoomed_in() {
        // Two spots ~3 km apart in London, one in Paris.
        let items = vec![item(51.50, -0.12), item(51.52, -0.08), item(48.85, 2.35)];
        let c = Clusters::build(&items, 40.0);
        assert_eq!(c.at_zoom(2.0).len(), 1);
        assert_eq!(c.at_zoom(7.0).len(), 2);
        assert_eq!(c.at_zoom(15.0).len(), 3);
        let total: usize = c.at_zoom(7.0).iter().map(|c| c.members.len()).sum();
        assert_eq!(total, 3);
        // Members stay sorted, so the cover is the newest item.
        assert_eq!(c.at_zoom(0.0)[0].members, vec![0, 1, 2]);
    }
}

#[cfg(test)]
mod perf_tests {
    use super::*;

    #[test]
    #[ignore]
    fn cluster_200k() {
        // Deterministic pseudo-random points around 50 "cities".
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rnd = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let centres: Vec<(f64, f64)> = (0..50).map(|_| (rnd() * 140.0 - 60.0, rnd() * 360.0 - 180.0)).collect();
        let items: Vec<Item> = (0..200_000)
            .map(|i| {
                let (lat, lon) = centres[i % 50];
                let mut it = tests::item(lat + (rnd() - 0.5) * 0.5, lon + (rnd() - 0.5) * 0.5);
                it.key = i as u64;
                it
            })
            .collect();
        let t = std::time::Instant::now();
        let c = Clusters::build(&items, 60.0);
        println!(
            "built in {:?}; level sizes z0={} z5={} z10={} z15={} z21={}",
            t.elapsed(),
            c.at_zoom(0.0).len(),
            c.at_zoom(5.0).len(),
            c.at_zoom(10.0).len(),
            c.at_zoom(15.0).len(),
            c.at_zoom(21.0).len()
        );
    }
}
