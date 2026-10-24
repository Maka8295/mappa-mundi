//! GPU texture cache fed by background workers, with LRU eviction.

use std::collections::{HashMap, HashSet};
use std::hash::Hash;
use std::sync::Arc;

use crossbeam_channel::Receiver;
use egui::{ColorImage, TextureHandle, TextureOptions};

use crate::work::WorkQueue;

pub struct TextureCache<K, J> {
    name: &'static str,
    textures: HashMap<K, (TextureHandle, u64)>,
    failed: HashSet<K>,
    wanted: Vec<(f32, K, J)>,
    wanted_keys: HashSet<K>,
    queue: Arc<WorkQueue<J>>,
    rx: Receiver<(K, Option<ColorImage>)>,
    frame: u64,
    capacity: usize,
}

impl<K, J> TextureCache<K, J>
where
    K: Clone + Eq + Hash + std::fmt::Debug,
    J: Clone + Eq + Hash + Send + 'static,
{
    pub fn new(
        name: &'static str,
        capacity: usize,
        queue: Arc<WorkQueue<J>>,
        rx: Receiver<(K, Option<ColorImage>)>,
    ) -> Self {
        Self {
            name,
            textures: HashMap::new(),
            failed: HashSet::new(),
            wanted: Vec::new(),
            wanted_keys: HashSet::new(),
            queue,
            rx,
            frame: 0,
            capacity,
        }
    }

    /// Uploads whatever the workers finished since the last frame.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        for (key, image) in self.rx.try_iter() {
            match image {
                Some(img) => {
                    let name = format!("{}-{key:?}", self.name);
                    let tex = ctx.load_texture(name, img, TextureOptions::LINEAR);
                    self.textures.insert(key, (tex, self.frame));
                }
                None => {
                    self.failed.insert(key);
                }
            }
        }
    }

    pub fn get(&mut self, key: &K) -> Option<&TextureHandle> {
        let frame = self.frame;
        self.textures.get_mut(key).map(|(t, used)| {
            *used = frame;
            &*t
        })
    }

    pub fn contains(&self, key: &K) -> bool {
        self.textures.contains_key(key)
    }

    pub fn failed(&self, key: &K) -> bool {
        self.failed.contains(key)
    }

    /// Asks for `key` to be loaded; lower `priority` values load first.
    pub fn request(&mut self, key: K, priority: f32, job: impl FnOnce() -> J) {
        if self.textures.contains_key(&key) || self.failed.contains(&key) || !self.wanted_keys.insert(key.clone()) {
            return;
        }
        self.wanted.push((priority, key, job()));
    }

    /// Hands this frame's requests to the workers and evicts old textures.
    pub fn end_frame(&mut self) {
        self.wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
        self.queue.replace(self.wanted.drain(..).map(|(_, _, job)| job));
        self.wanted_keys.clear();

        if self.textures.len() > self.capacity {
            let mut old: Vec<(u64, K)> = self
                .textures
                .iter()
                .filter(|(_, (_, used))| *used < self.frame)
                .map(|(k, (_, used))| (*used, k.clone()))
                .collect();
            old.sort_by_key(|(used, _)| *used);
            let excess = self.textures.len() - self.capacity;
            for (_, k) in old.into_iter().take(excess) {
                self.textures.remove(&k);
            }
        }
    }
}
