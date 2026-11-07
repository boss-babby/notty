//! Which full-window view is showing: the open page, the home dashboard or the graph view.
use std::cell::Cell;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Page,
    Dashboard,
    Graph,
    Automations,
}

thread_local! {
    static M: Cell<(Mode, Option<u64>)> = const { Cell::new((Mode::Page, None)) };
}

pub fn mode() -> Mode {
    M.with(|m| m.get().0)
}

/// Switches view; `current` is the page that is open right now (opening any other page leaves the view).
pub fn set(mode: Mode, current: Option<u64>) {
    M.with(|m| m.set((mode, current)));
}

pub fn toggle(mode: Mode, current: Option<u64>) {
    if self::mode() == mode {
        set(Mode::Page, None);
    } else {
        set(mode, current);
    }
}

/// Call once per frame: navigating to another page (sidebar, palette, link) closes the view.
pub fn sync(current: Option<u64>) {
    M.with(|m| {
        let (mode, at) = m.get();
        if mode != Mode::Page && at != current {
            m.set((Mode::Page, None));
        }
    });
}
