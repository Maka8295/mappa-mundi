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

    fn run(self) -> (u64, Option<ColorImage>) {
        let src = imageio::Source { path: &self.path, kind: self.kind, orientation: self.orientation, key: self.key };
        let result = if self.max_dim == 0 { imageio::thumbnail(&src) } else { imageio::full(&src, self.max_dim) };
        match result {
            Ok(img) => {
                let size = [img.width() as usize, img.height() as usize];
                (self.key, Some(ColorImage::from_rgba_unmultiplied(size, img.as_raw())))
            }
            Err(e) => {
                eprintln!("mappa-mundi: {}: {e:#}", self.path.display());
                (self.key, None)
            }
        }
    }
}

pub struct Media {
    thumbs: TextureCache<u64, Job>,
    full: TextureCache<u64, Job>,
    /// Images at their own size, for zooming in on the one being viewed.
    original: TextureCache<u64, Job>,
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
            full: start("full", 2, 6),
            // A 12 MP photo is ~50 MB of GPU memory.
            original: start("original", 1, 2),
        }
    }

    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.thumbs.begin_frame(ctx);
        self.full.begin_frame(ctx);
        self.original.begin_frame(ctx);
    }

    pub fn end_frame(&mut self) {
        self.thumbs.end_frame();
        self.full.end_frame();
        self.original.end_frame();
    }

    pub fn thumb(&mut self, item: &Item, priority: f32) -> Option<TextureHandle> {
        if let Some(t) = self.thumbs.get(&item.key) {
            return Some(t.clone());
        }
        self.thumbs.request(item.key, priority, || Job::new(item, 0));
        None
    }

    pub fn thumb_failed(&self, item: &Item) -> bool {
        self.thumbs.failed(&item.key)
    }

    pub fn full(&mut self, item: &Item, priority: f32, max_dim: u32) -> Option<TextureHandle> {
        if let Some(t) = self.full.get(&item.key) {
            return Some(t.clone());
        }
        self.full.request(item.key, priority, || Job::new(item, max_dim));
        None
    }

    pub fn full_failed(&self, item: &Item) -> bool {
        self.full.failed(&item.key)
    }

    /// The image without downscaling (up to `max_side`, the GPU's limit).
    pub fn original(&mut self, item: &Item, max_side: u32) -> Option<TextureHandle> {
        if let Some(t) = self.original.get(&item.key) {
            return Some(t.clone());
        }
        self.original.request(item.key, 0.0, || Job::new(item, max_side));
        None
    }

    pub fn original_failed(&self, item: &Item) -> bool {
        self.original.failed(&item.key)
    }
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
