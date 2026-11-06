//! Background work. A `Task` runs one closure on its own thread and hands the
//! result to the UI, which polls once per frame. Restarting drops any earlier
//! in-flight result, so stale answers can never overwrite newer ones.
use std::sync::mpsc::{Receiver, TryRecvError, channel};

pub struct Task<T> {
    rx: Option<Receiver<Result<T, String>>>,
    pub value: Option<T>,
    pub error: Option<String>,
}

impl<T> Default for Task<T> {
    fn default() -> Self {
        Self { rx: None, value: None, error: None }
    }
}

impl<T: Send + 'static> Task<T> {
    pub fn spawn<E: ToString>(&mut self, ctx: &egui::Context, f: impl FnOnce() -> Result<T, E> + Send + 'static) {
        let (tx, rx) = channel();
        let ctx = ctx.clone();
        self.rx = Some(rx);
        std::thread::spawn(move || {
            let r = f().map_err(|e| e.to_string());
            let _ = tx.send(r);
            ctx.request_repaint();
        });
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
