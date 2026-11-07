//! Background work. A `Task` runs one closure on its own thread and hands the
//! result to the UI, which polls once per frame. Restarting drops any earlier
//! in-flight result, so stale answers can never overwrite newer ones.
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::{Duration, Instant};

use gitem_core::Repo;

pub struct Task<T> {
    rx: Option<Receiver<Result<T, String>>>,
    pub value: Option<T>,
    pub error: Option<String>,
    /// Stops the git commands of the work under way, for work started by `spawn_git`.
    cancel: Option<Arc<AtomicBool>>,
    started: Option<Instant>,
}

fn counter(ctx: &egui::Context) -> Arc<AtomicUsize> {
    ctx.data_mut(|d| d.get_temp_mut_or_default::<Arc<AtomicUsize>>(egui::Id::new("gitem-tasks-running")).clone())
}

/// How many tasks of this context are still working, including any whose result
/// nobody is waiting for any more. Lets tests wait for the work itself, not a time.
pub fn running(ctx: &egui::Context) -> usize {
    counter(ctx).load(Ordering::SeqCst)
}

/// Counts a task down when its thread ends, also if the closure panics.
struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl<T> Default for Task<T> {
    fn default() -> Self {
        Self { rx: None, value: None, error: None, cancel: None, started: None }
    }
}

impl<T> Task<T> {
    fn stop_git(&mut self) {
        if let Some(c) = self.cancel.take() {
            c.store(true, Ordering::Relaxed);
        }
    }
}

/// Closing a view stops what it was waiting for. A blame of a big file runs for minutes.
impl<T> Drop for Task<T> {
    fn drop(&mut self) {
        self.stop_git();
    }
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn<E: ToString>(&mut self, ctx: &egui::Context, f: impl FnOnce() -> Result<T, E> + Send + 'static) {
        self.stop_git();
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.rx = Some(rx);
        self.started = Some(Instant::now());
        let count = counter(&ctx);
        count.fetch_add(1, Ordering::SeqCst);
        let running = Running(count);
        std::thread::spawn(move || {
            let _running = running;
            let r = f().map_err(|e| e.to_string());
            let _ = tx.send(r);
            ctx.request_repaint();
        });
    }

    /// The same, with a copy of `repo` for `f` to use whose git commands stop if this work
    /// is replaced or stopped, or this task is dropped.
    pub fn spawn_git<E: ToString>(&mut self, ctx: &egui::Context, repo: &Repo, f: impl FnOnce(Repo) -> Result<T, E> + Send + 'static) {
        let mut repo = repo.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        repo.git.cancel = Some(cancel.clone());
        self.spawn(ctx, move || f(repo));
        self.cancel = Some(cancel);
    }

    /// Gives up on the work under way. Nothing it still does can reach this task.
    pub fn stop(&mut self) {
        self.stop_git();
        self.rx = None;
        self.started = None;
    }

    /// How long the work under way has been going.
    pub fn running_for(&self) -> Option<Duration> {
        self.started.filter(|_| self.rx.is_some()).map(|s| s.elapsed())
    }

    /// Returns true on the frame a result (or error) arrives.
    pub fn poll(&mut self) -> bool {
        let Some(rx) = &self.rx else { return false };
        match rx.try_recv() {
            Ok(Ok(v)) => {
                self.value = Some(v);
                self.error = None;
                self.rx = None;
                true
            }
            Ok(Err(e)) => {
                self.error = Some(e);
                self.rx = None;
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.error = Some("background task stopped unexpectedly".into());
                self.rx = None;
                true
            }
        }
    }

    pub fn busy(&self) -> bool {
        self.rx.is_some()
    }

    pub fn get(&self) -> Option<&T> {
        self.value.as_ref()
    }
}
