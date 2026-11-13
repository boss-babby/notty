//! Background scheduler for flows and scripts that repeat every N seconds.
//!
//! Before v12 a flow set to "run every N seconds" was driven by the flow *page*: the moment you navigated away the timer stopped.
//! The engine lives in the application now and is ticked on every frame whatever page, view or dialog is on screen. The only
//! flow it leaves alone is the one whose editor is currently visible (that page drives its own run so the node status lights
//! and the report stay in sync).
//!
//! Limits of the platforms (honest version): the engine runs while the app process is alive and rendering. In a browser tab
//! that is hidden the browser pauses animation frames, on Android the process is frozen a few seconds after the app leaves the
//! screen — keep the screen on (the app keeps it awake while schedules exist) or use the Windows build for round-the-clock jobs.

use std::collections::HashMap;
use std::time::Duration;

use crate::automation::{self as au, Output};
use crate::flow::Run;
use crate::model::{PageKind, Workspace};
use crate::scripthost::{self, ScriptCtx};

#[derive(Default)]
pub struct Bg {
    runs: HashMap<u64, Run>,
    last_flow: HashMap<u64, f64>,
    last_script: HashMap<u64, f64>,
    /// How many schedules are alive (flows + scripts) — shown in the UI.
    pub active: usize,
    /// How many runs finished since the app started.
    pub completed: u64,
}

pub struct BgOut {
    pub output: Output,
    pub toasts: Vec<String>,
}

impl Bg {
    /// Call once per frame. `skip` = the flow page whose editor is on screen.
    pub fn tick(&mut self, ws: &mut Workspace, ctx: &egui::Context, now: f64, skip: Option<u64>) -> BgOut {
        let mut out = BgOut { output: Output::default(), toasts: vec![] };
        let mut fastest = u32::MAX;

        // ── flows ──
        let flows: Vec<(u64, u32)> = ws.pages.iter().filter(|p| p.kind == PageKind::Flow && !p.trashed).filter_map(|p| p.flow.as_ref().filter(|f| f.every > 0).map(|f| (p.id, f.every))).collect();
        self.runs.retain(|id, _| flows.iter().any(|f| f.0 == *id) && Some(*id) != skip);
        self.last_flow.retain(|id, _| flows.iter().any(|f| f.0 == *id));
        let mut active = 0;
        for (id, every) in &flows {
            if Some(*id) == skip {
                continue;
            }
            active += 1;
            fastest = fastest.min(*every);
            if let Some(run) = self.runs.get_mut(id) {
                if !run.finished {
                    run.step(ws, now);
                }
                if run.finished {
                    if let Some(mut run) = self.runs.remove(id) {
                        self.completed += 1;
                        let o = std::mem::take(&mut run.output);
                        out.output.changed |= o.changed;
                        out.output.notices.extend(o.notices);
                        if let Some(t) = run.copy.take() {
                            ctx.copy_text(t);
                        }
                    }
                }
                continue;
            }
            let last = self.last_flow.get(id).copied().unwrap_or(0.0);
            if now - last >= *every as f64 * 1000.0 {
                self.last_flow.insert(*id, now);
                let Some(f) = ws.page(*id).and_then(|p| p.flow.clone()) else { continue };
                if !f.problems().is_empty() {
                    continue;
                }
                let name = ws.page(*id).map(|p| p.title.clone()).filter(|t| !t.is_empty()).unwrap_or_else(|| "Flow".into());
                let run = Run::start(&f, &name, None, false, ws, now);
                self.runs.insert(*id, run);
            }
        }

        // ── scripts of the library ──
        let scripts: Vec<(u64, String, String, u32)> = ws.auto.scripts.iter().filter(|s| s.every > 0).map(|s| (s.id, s.name.clone(), s.code.clone(), s.every)).collect();
        self.last_script.retain(|id, _| scripts.iter().any(|s| s.0 == *id));
        for (id, name, code, every) in scripts {
            active += 1;
            fastest = fastest.min(every);
            let last = self.last_script.get(&id).copied().unwrap_or(0.0);
            if now - last < every as f64 * 1000.0 {
                continue;
            }
            self.last_script.insert(id, now);
            let sctx = ScriptCtx { row: None, page: None, db: 0, source: name.clone(), kind: au::NoticeKind::Button, now };
            let mut o = Output::default();
            let mut follow = vec![];
            let rep = scripthost::run_script(ws, &code, &sctx, &mut o, &mut follow);
            out.output.changed |= o.changed;
            out.output.notices.extend(o.notices);
            self.completed += 1;
            let mut stop: Option<String> = None;
            if let Some(s) = ws.auto.scripts.iter_mut().find(|s| s.id == id) {
                s.runs = s.runs.saturating_add(1);
                s.last_run = now;
                if let Some(e) = &rep.error {
                    // an endless stream of the same error helps nobody: stop the schedule and say why
                    s.every = 0;
                    stop = Some(format!("Script “{}” stopped repeating: {}", name, e));
                }
            }
            if let Some(msg) = stop {
                au::log(ws, &name, &msg, false, now);
                out.toasts.push(msg);
            }
        }

        self.active = active;
        if active > 0 {
            let busy = !self.runs.is_empty();
            let ms = if busy { 30 } else { (fastest.min(5) as u64 * 1000 / 4).clamp(100, 250) };
            ctx.request_repaint_after(Duration::from_millis(ms));
        }
        out
    }
}
