//! Thumbnails and full-size images, decoded on worker threads.

use std::path::PathBuf;

use egui::{ColorImage, TextureHandle};

use crate::imageio;
use crate::library::{Item, Kind};
use crate::texcache::TextureCache;
use crate::work::WorkQueue;

#[derive(Clone, PartialEq, Eq, Hash)]
pub struct Job {
    key: u64,
    path: PathBuf,
    kind: Kind,
    orientation: u8,
    /// 0 for a thumbnail, otherwise the long-edge limit of a full image.
    max_dim: u32,
}

impl Job {
    fn new(item: &Item, max_dim: u32) -> Self {
        let path = item.image_path().to_path_buf();
        Self { key: item.key, path, kind: item.kind, orientation: item.orientation, max_dim }
    }

    /// Results are keyed by item and size, so one photo can be loaded at
    /// several sizes at once.
    fn run(self) -> (Key, Option<ColorImage>) {
        let src = imageio::Source { path: &self.path, kind: self.kind, orientation: self.orientation, key: self.key };
        let result = if self.max_dim == 0 { imageio::thumbnail(&src) } else { imageio::full(&src, self.max_dim) };
        match result {
            Ok(img) => {
                let size = [img.width() as usize, img.height() as usize];
                ((self.key, self.max_dim), Some(ColorImage::from_rgba_unmultiplied(size, img.as_raw())))
            }
            Err(e) => {
                eprintln!("mappa-mundi: {}: {e:#}", self.path.display());
                ((self.key, self.max_dim), None)
            }
        }
    }
}

/// Item key and long-edge limit (0 for a thumbnail).
type Key = (u64, u32);

pub struct Media {
    thumbs: TextureCache<Key, Job>,
    /// Images decoded at the size the viewer shows them.
    full: TextureCache<Key, Job>,
    /// Larger versions, up to the original size, for zooming in.
    zoom: TextureCache<Key, Job>,
}

impl Media {
    pub fn new(ctx: &egui::Context) -> Self {
        let cores = std::thread::available_parallelism().map_or(4, |n| n.get());
        let start = |name: &'static str, threads: usize, capacity: usize| {
            let queue = WorkQueue::new();
            let (tx, rx) = crossbeam_channel::unbounded();
            queue.spawn_workers(name, threads, tx, ctx.clone(), || Job::run);
            TextureCache::new(name, capacity, queue, rx)
        };
        Self {
            // ~800 thumbnails of 320x240 is roughly 250 MB of GPU memory.
            thumbs: start("thumb", (cores / 2).clamp(2, 8), 800),
            full: start("full", 2, 8),
            // A 12 MP photo is ~50 MB of GPU memory; this holds two photos'
            // original and half-size versions.
            zoom: start("zoom", 2, 4),
        }
    }

    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.thumbs.begin_frame(ctx);
        self.full.begin_frame(ctx);
        self.zoom.begin_frame(ctx);
    }

    pub fn end_frame(&mut self) {
        self.thumbs.end_frame();
        self.full.end_frame();
        self.zoom.end_frame();
    }

    pub fn thumb(&mut self, item: &Item, priority: f32) -> Option<TextureHandle> {
        load(&mut self.thumbs, item, priority, 0)
    }

    pub fn thumb_failed(&self, item: &Item) -> bool {
        self.thumbs.failed(&(item.key, 0))
    }

    /// The image with its long edge scaled down to `max_dim` pixels.
    pub fn full(&mut self, item: &Item, priority: f32, max_dim: u32) -> Option<TextureHandle> {
        load(&mut self.full, item, priority, max_dim)
    }

    pub fn full_failed(&self, item: &Item, max_dim: u32) -> bool {
        self.full.failed(&(item.key, max_dim))
    }

    /// Like `full`, for the bigger versions zooming in needs.
    pub fn zoomed(&mut self, item: &Item, priority: f32, max_dim: u32) -> Option<TextureHandle> {
        load(&mut self.zoom, item, priority, max_dim)
    }

    pub fn zoomed_failed(&self, item: &Item, max_dim: u32) -> bool {
        self.zoom.failed(&(item.key, max_dim))
    }
}

fn load(cache: &mut TextureCache<Key, Job>, item: &Item, priority: f32, max_dim: u32) -> Option<TextureHandle> {
    let key = (item.key, max_dim);
    if let Some(t) = cache.get(&key) {
        return Some(t.clone());
    }
    cache.request(key, priority, || Job::new(item, max_dim));
    None
}

/// UV rectangle that centre-crops a texture to a square.
pub fn square_uv(size: [usize; 2]) -> egui::Rect {
    let (w, h) = (size[0] as f32, size[1] as f32);
    if w > h {
        let m = (1.0 - h / w) / 2.0;
        egui::Rect::from_min_max(egui::pos2(m, 0.0), egui::pos2(1.0 - m, 1.0))
    } else {
        let m = (1.0 - w / h) / 2.0;
        egui::Rect::from_min_max(egui::pos2(0.0, m), egui::pos2(1.0, 1.0 - m))
    }
}
