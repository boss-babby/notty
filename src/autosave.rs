//! Autosave off the UI thread.
//!
//! Serialising a large workspace (the dirty-check hash **and** the RON text) is the most expensive thing an active app does. While the
//! user is working, `LumenApp::save` hands a *snapshot* (a clone — memcpy-like, no formatting) to one long-lived worker thread; the
//! worker fingerprints and serialises it, and the UI thread only stores the finished string (`Storage::set_string` is a map insert).
//! Everything that could lose data keeps the old synchronous path (window closing, app in the background, idle app, the browser, which
//! has no threads): see `LumenApp::save`.
use crate::auth::Credential;
use crate::model::Workspace;

/// The state to persist, copied.
pub struct SaveJob {
    pub ws: Workspace,
    pub cred: Option<Credential>,
    pub skipped: bool,
    /// Fingerprint of what is already stored: an equal fingerprint means "nothing to write".
    pub known_fp: Option<u64>,
    pub gen: u64,
}

/// `text == None`: unchanged (or not serialisable — the caller then saves synchronously next time).
pub struct SaveDone {
    pub fp: Option<u64>,
    pub text: Option<String>,
    pub gen: u64,
}

pub type Runner = fn(SaveJob) -> SaveDone;

#[cfg(not(target_arch = "wasm32"))]
pub struct Autosave {
    tx: std::sync::mpsc::Sender<SaveJob>,
    rx: std::sync::mpsc::Receiver<SaveDone>,
    busy: bool,
    gen: u64,
}

#[cfg(not(target_arch = "wasm32"))]
impl Autosave {
    pub fn new(ctx: &egui::Context, run: Runner) -> Autosave {
        let (tx, jobs) = std::sync::mpsc::channel::<SaveJob>();
        let (done_tx, rx) = std::sync::mpsc::channel::<SaveDone>();
        let ctx = ctx.clone();
        let _ = std::thread::Builder::new()
            .name("lumen-autosave".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let gen = job.gen;
                    let mut done = run(job);
                    done.gen = gen;
                    if done_tx.send(done).is_err() {
                        return;
                    }
                    ctx.request_repaint(); // a repaint is the moment the UI thread stores the result
                }
            });
        Autosave {
            tx,
            rx,
            busy: false,
            gen: 1,
        }
    }

    pub fn busy(&self) -> bool {
        self.busy
    }

    /// Invalidates any in-flight job (e.g. after switching or resetting the workspace) so stale completions are discarded.
    pub fn invalidate(&mut self) {
        self.gen = self.gen.wrapping_add(1).max(1);
        while self.rx.try_recv().is_ok() {}
    }

    /// Queues a snapshot. `false` when the worker is still busy with the previous one (the caller just tries again next time).
    pub fn submit(&mut self, mut job: SaveJob) -> bool {
        if self.busy {
            return false;
        }
        job.gen = self.gen;
        self.busy = self.tx.send(job).is_ok();
        self.busy
    }

    /// A finished job, if there is one (never blocks). Discards completions from older generations.
    pub fn poll(&mut self) -> Option<SaveDone> {
        loop {
            let d = self.rx.try_recv().ok()?;
            self.busy = false;
            if d.gen == self.gen {
                return Some(d);
            }
        }
    }

    /// Waits for the job in flight (the synchronous save paths call this first so an older result can never overwrite a newer one).
    pub fn wait(&mut self) -> Option<SaveDone> {
        if !self.busy {
            return None;
        }
        let d = self
            .rx
            .recv_timeout(std::time::Duration::from_secs(20))
            .ok();
        if d.is_none() {
            // Timed out while worker is still running: invalidate its generation so its late result is ignored.
            self.gen = self.gen.wrapping_add(1).max(1);
        }
        self.busy = false;
        d.filter(|r| r.gen == self.gen)
    }
}

/// The browser has no threads: the "worker" runs the job inline and `poll` hands the result back at once.
#[cfg(target_arch = "wasm32")]
pub struct Autosave {
    run: Runner,
    done: Option<SaveDone>,
}

#[cfg(target_arch = "wasm32")]
impl Autosave {
    pub fn new(_ctx: &egui::Context, run: Runner) -> Autosave {
        Autosave { run, done: None }
    }
    pub fn busy(&self) -> bool {
        false
    }
    pub fn invalidate(&mut self) {
        self.done = None;
    }
    pub fn submit(&mut self, job: SaveJob) -> bool {
        self.done = Some((self.run)(job));
        true
    }
    pub fn poll(&mut self) -> Option<SaveDone> {
        self.done.take()
    }
    pub fn wait(&mut self) -> Option<SaveDone> {
        self.done.take()
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    fn run(job: SaveJob) -> SaveDone {
        let text = serde_json::to_string(&job.ws).ok();
        let fp = text.as_ref().map(|t| crate::media::fnv(t.as_bytes()));
        if fp.is_some() && fp == job.known_fp {
            return SaveDone {
                fp,
                text: None,
                gen: job.gen,
            };
        }
        SaveDone {
            fp,
            text,
            gen: job.gen,
        }
    }

    #[test]
    fn a_snapshot_is_serialised_on_the_worker_and_an_unchanged_one_is_skipped() {
        let ctx = egui::Context::default();
        let mut a = Autosave::new(&ctx, run);
        let ws = Workspace::seed();
        assert!(a.submit(SaveJob {
            ws: ws.clone(),
            cred: None,
            skipped: false,
            known_fp: None,
            gen: 0,
        }));
        assert!(
            !a.submit(SaveJob {
                ws: ws.clone(),
                cred: None,
                skipped: false,
                known_fp: None,
                gen: 0,
            }),
            "one job at a time"
        );
        let first = a.wait().expect("result");
        assert!(first.text.is_some() && first.fp.is_some());
        assert!(!a.busy());
        assert!(a.submit(SaveJob {
            ws,
            cred: None,
            skipped: false,
            known_fp: first.fp,
            gen: 0,
        }));
        let second = a.wait().expect("result");
        assert!(second.text.is_none(), "same state ⇒ nothing to write");
        assert_eq!(second.fp, first.fp);
    }
}
