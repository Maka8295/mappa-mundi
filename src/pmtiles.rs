//! Reader for PMTiles v3 archives (https://github.com/protomaps/PMTiles):
//! a single file of map tiles indexed by a Hilbert-curve tile id.

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::os::unix::fs::FileExt;
use std::path::Path;
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, ensure};

#[derive(Clone, Copy)]
struct Entry {
    tile_id: u64,
    offset: u64,
    length: u32,
    /// 0 means the entry points at a leaf directory instead of a tile.
    run_length: u32,
}

pub struct Pmtiles {
    file: File,
    root: Vec<Entry>,
    leaves: Mutex<HashMap<u64, Arc<Vec<Entry>>>>,
    leaf_offset: u64,
    data_offset: u64,
    gzip_tiles: bool,
    pub min_zoom: u8,
    pub max_zoom: u8,
    pub attribution: String,
}

impl Pmtiles {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
        let mut h = [0u8; 127];
        file.read_exact_at(&mut h, 0)?;
        ensure!(&h[..7] == b"PMTiles" && h[7] == 3, "{} is not a PMTiles v3 archive", path.display());
        let u64_at = |i: usize| u64::from_le_bytes(h[i..i + 8].try_into().unwrap());
        let (root_off, root_len, meta_off, meta_len) = (u64_at(8), u64_at(16), u64_at(24), u64_at(32));
        let internal_gzip = h[97] == 2;
        ensure!(h[97] <= 2, "unsupported directory compression in {}", path.display());
        ensure!(h[99] == 1, "{} doesn't contain vector (MVT) tiles", path.display());

        let read = |off: u64, len: u64| -> anyhow::Result<Vec<u8>> {
            let mut buf = vec![0; len as usize];
            file.read_exact_at(&mut buf, off)?;
            if internal_gzip { gunzip(&buf) } else { Ok(buf) }
        };
        let root = decode_directory(&read(root_off, root_len)?)?;
        let meta: serde_json::Value = serde_json::from_slice(&read(meta_off, meta_len)?).unwrap_or_default();
        let attribution = meta["attribution"].as_str().unwrap_or("© OpenStreetMap").to_string();
        Ok(Self {
            root,
            leaves: Mutex::new(HashMap::new()),
            leaf_offset: u64_at(40),
            data_offset: u64_at(56),
            gzip_tiles: h[98] == 2,
            min_zoom: h[100],
            max_zoom: h[101],
            attribution,
            file,
        })
    }

    /// The decompressed tile, or None if the archive doesn't have it.
    pub fn tile(&self, z: u8, x: u32, y: u32) -> anyhow::Result<Option<Vec<u8>>> {
        if z < self.min_zoom || z > self.max_zoom {
            return Ok(None);
        }
        let id = tile_id(z, x, y);
        let mut dir = None::<Arc<Vec<Entry>>>;
        for _ in 0..4 {
            let entries: &[Entry] = dir.as_deref().map_or(&self.root, |d| d);
            let Some(e) = find(entries, id) else { return Ok(None) };
            if e.run_length > 0 {
                let mut buf = vec![0; e.length as usize];
                self.file.read_exact_at(&mut buf, self.data_offset + e.offset)?;
                return Ok(Some(if self.gzip_tiles { gunzip(&buf)? } else { buf }));
            }
            dir = Some(self.leaf(e.offset, e.length)?);
        }
        Ok(None)
    }

    fn leaf(&self, offset: u64, length: u32) -> anyhow::Result<Arc<Vec<Entry>>> {
        if let Some(d) = self.leaves.lock().unwrap().get(&offset) {
            return Ok(d.clone());
        }
        let mut buf = vec![0; length as usize];
        self.file.read_exact_at(&mut buf, self.leaf_offset + offset)?;
        let dir = Arc::new(decode_directory(&gunzip(&buf).unwrap_or(buf))?);
        self.leaves.lock().unwrap().insert(offset, dir.clone());
        Ok(dir)
    }
}

fn gunzip(buf: &[u8]) -> anyhow::Result<Vec<u8>> {
    let mut out = Vec::with_capacity(buf.len() * 3);
    flate2::read::GzDecoder::new(buf).read_to_end(&mut out)?;
    Ok(out)
}

fn find(entries: &[Entry], id: u64) -> Option<Entry> {
    let i = entries.partition_point(|e| e.tile_id <= id).checked_sub(1)?;
    let e = entries[i];
    (e.run_length == 0 || id < e.tile_id + e.run_length as u64).then_some(e)
}

fn decode_directory(buf: &[u8]) -> anyhow::Result<Vec<Entry>> {
    let mut pos = 0;
    let mut next = || -> anyhow::Result<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let b = *buf.get(pos).context("truncated directory")?;
            pos += 1;
            v |= ((b & 0x7f) as u64) << shift;
            if b < 0x80 {
                return Ok(v);
            }
        }
        anyhow::bail!("bad varint in directory")
    };
    let n = next()? as usize;
    let mut entries = vec![Entry { tile_id: 0, offset: 0, length: 0, run_length: 0 }; n];
    let mut last = 0;
    for e in entries.iter_mut() {
        last += next()?;
        e.tile_id = last;
    }
    for e in entries.iter_mut() {
        e.run_length = next()? as u32;
    }
    for e in entries.iter_mut() {
        e.length = next()? as u32;
    }
    for i in 0..n {
        let v = next()?;
        entries[i].offset = if v == 0 && i > 0 { entries[i - 1].offset + entries[i - 1].length as u64 } else { v - 1 };
    }
    Ok(entries)
}

/// Tile id: all tiles of lower zoom levels, then the Hilbert-curve position.
pub fn tile_id(z: u8, mut x: u32, mut y: u32) -> u64 {
    let mut id = ((1u64 << (2 * z as u64)) - 1) / 3;
    let mut s = 1u32 << z >> 1;
    while s > 0 {
        let rx = (x & s > 0) as u64;
        let ry = (y & s > 0) as u64;
        id += s as u64 * s as u64 * ((3 * rx) ^ ry);
        if ry == 0 {
            if rx == 1 {
                x = s.wrapping_sub(1).wrapping_sub(x);
                y = s.wrapping_sub(1).wrapping_sub(y);
            }
            std::mem::swap(&mut x, &mut y);
        }
        s >>= 1;
    }
    id
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tile_ids_match_the_spec() {
        let cases = [((0, 0, 0), 0), ((1, 0, 0), 1), ((1, 0, 1), 2), ((1, 1, 1), 3), ((1, 1, 0), 4), ((2, 0, 0), 5)];
        for ((z, x, y), want) in cases {
            assert_eq!(tile_id(z, x, y), want, "{z}/{x}/{y}");
        }
        // Same ids as tools/street-maps computes (Trafalgar Square at z15, and a deep tile).
        assert_eq!(tile_id(15, 16372, 10896), 518_974_351);
        assert_eq!(tile_id(20, 523_904, 348_672), 531_429_736_447);
    }
}
