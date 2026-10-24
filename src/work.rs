//! A tiny prioritised job queue drained by a pool of worker threads.
//!
//! The UI thread re-submits everything it currently wants each frame (most
//! important first), so stale requests for things that scrolled off-screen
//! are simply dropped. Jobs already running are never queued twice.

use std::collections::{HashSet, VecDeque};
use std::hash::Hash;
use std::sync::{Arc, Condvar, Mutex};

use crossbeam_channel::Sender;

struct State<T> {
    pending: VecDeque<T>,
    running: HashSet<T>,
}

pub struct WorkQueue<T> {
    state: Mutex<State<T>>,
    ready: Condvar,
}

impl<T: Clone + Eq + Hash + Send + 'static> WorkQueue<T> {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(State { pending: VecDeque::new(), running: HashSet::new() }),
            ready: Condvar::new(),
        })
    }

    /// Replaces all pending (not yet started) jobs.
    pub fn replace(&self, jobs: impl IntoIterator<Item = T>) {
        let mut s = self.state.lock().unwrap();
        let State { pending, running } = &mut *s;
        pending.clear();
        pending.extend(jobs.into_iter().filter(|j| !running.contains(j)));
        if !pending.is_empty() {
            self.ready.notify_all();
        }
    }

    fn pop(&self) -> T {
        let mut s = self.state.lock().unwrap();
        loop {
            if let Some(job) = s.pending.pop_front() {
                s.running.insert(job.clone());
                return job;
            }
            s = self.ready.wait(s).unwrap();
        }
    }

    fn finish(&self, job: &T) {
        self.state.lock().unwrap().running.remove(job);
    }

    /// Starts `n` workers. `make` builds each worker's job handler, which lets
    /// a worker own per-thread state such as a database connection.
    pub fn spawn_workers<R, F>(
        self: &Arc<Self>,
        name: &str,
        n: usize,
        tx: Sender<R>,
        ctx: egui::Context,
        make: impl Fn() -> F + Send + Sync + 'static,
    ) where
        R: Send + 'static,
        F: FnMut(T) -> R,
    {
        let make = Arc::new(make);
        for i in 0..n {
            let (queue, tx, ctx, make) = (self.clone(), tx.clone(), ctx.clone(), make.clone());
            std::thread::Builder::new()
                .name(format!("{name}-{i}"))
                .spawn(move || {
                    let mut handle = make();
                    loop {
                        let job = queue.pop();
                        let result = handle(job.clone());
                        // Deliver before un-marking so the UI never re-requests
                        // a job whose result is already in flight.
                        let sent = tx.send(result).is_ok();
                        queue.finish(&job);
                        if !sent {
                            return;
                        }
                        ctx.request_repaint();
                    }
                })
                .expect("spawning worker thread");
        }
    }
}
