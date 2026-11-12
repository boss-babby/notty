//! Android shell: the Rust half of the bridge to `android/java/com/lumen/app/LumenActivity.java`.
//!
//! The app runs as a `NativeActivity` (android-activity + winit + glow/EGL). Everything that needs the Android framework goes through
//! the Java activity, which is reachable from any thread via JNI:
//!
//! * **Density** — the Java side reports `DisplayMetrics.density`; [`on_raw_input`] forces egui's `pixels_per_point` to exactly that
//!   value (through the zoom factor), so text is rasterised at the real device resolution — the same crisp text as WebAssembly in
//!   Chrome (`devicePixelRatio`) — and the phone layout breakpoints see the same logical width as in the browser.
//! * **System bars** — the window is edge-to-edge; the activity reports the status bar / navigation bar / cutout insets and the
//!   keyboard height. `screen_rect` handed to egui is shrunk by them, so nothing is ever drawn below the status bar, behind the
//!   navigation bar or under the keyboard (the editor scrolls the caret into view when the height changes).
//! * **Soft keyboard** — a dedicated 1×1 `View` in the activity owns the `InputConnection`; committed text and editing keys come back
//!   here as events ([`Input`]) and are injected into egui's raw input. [`after_frame`] shows / hides the keyboard.
//! * **Files** — Storage-Access-Framework dialogs (`ACTION_OPEN_DOCUMENT`, `ACTION_CREATE_DOCUMENT`); results arrive in
//!   `nativePicked` and flow into `platform::push_picked`.
//! * Clipboard, links, haptics, notifications, printing, the Back button.
#![allow(non_snake_case)]

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicI64, AtomicU32, AtomicUsize, Ordering};
use std::sync::{Mutex, OnceLock};

use egui::{Context, Event, Key, Modifiers, Pos2, RawInput, Rect};
use jni::objects::{JByteArray, JClass, JObject, JString, JValue};
use jni::sys::{jboolean, jfloat, jint};
use jni::{JNIEnv, JavaVM};

use crate::platform::{PickTarget, Picked};

// ───────────────────────────── shared state ─────────────────────────────

static VM: AtomicUsize = AtomicUsize::new(0);
static ACT: AtomicUsize = AtomicUsize::new(0);
static CTX: OnceLock<Context> = OnceLock::new();

/// `DisplayMetrics.density` as f32 bits (0 = not reported yet).
static DENSITY: AtomicU32 = AtomicU32::new(0);
/// System insets in physical pixels: left, top, right, bottom (bottom already includes the keyboard).
static INSETS: [AtomicI32; 4] = [AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0), AtomicI32::new(0)];
static IME_VISIBLE: AtomicBool = AtomicBool::new(false);
static TZ_MIN: AtomicI64 = AtomicI64::new(0);
static BACK: AtomicBool = AtomicBool::new(false);
static NEXT_REQ: AtomicI32 = AtomicI32::new(1);
static PENDING_PICKS: Mutex<Option<HashMap<i32, PickTarget>>> = Mutex::new(None);
static INPUT: Mutex<Vec<Input>> = Mutex::new(Vec::new());

/// Something the soft keyboard produced.
enum Input {
    Text(String),
    Key(Key, bool),
}

fn wake() {
    if let Some(c) = CTX.get() {
        c.request_repaint();
    }
}

// ───────────────────────────── start-up ─────────────────────────────

/// Called first thing in `android_main`.
pub fn init(app: &android_activity::AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info).with_tag("Lumen"));
    std::panic::set_hook(Box::new(|info| log::error!("panic: {info}")));
    VM.store(app.vm_as_ptr() as usize, Ordering::SeqCst);
    ACT.store(app.activity_as_ptr() as usize, Ordering::SeqCst);
}

// ───────────────────────────── JNI plumbing ─────────────────────────────

fn with_activity<R>(f: impl FnOnce(&mut JNIEnv, &JObject) -> jni::errors::Result<R>) -> Option<R> {
    let (vm, act) = (VM.load(Ordering::SeqCst), ACT.load(Ordering::SeqCst));
    if vm == 0 || act == 0 {
        return None;
    }
    // SAFETY: both pointers come from android-activity and stay valid for the life of the process (global references).
    let vm = unsafe { JavaVM::from_raw(vm as *mut jni::sys::JavaVM) }.ok()?;
    let mut env = vm.attach_current_thread().ok()?;
    let obj = unsafe { JObject::from_raw(act as jni::sys::jobject) };
    let r = f(&mut env, &obj);
    if env.exception_check().unwrap_or(false) {
        let _ = env.exception_describe();
        let _ = env.exception_clear();
    }
    match r {
        Ok(v) => Some(v),
        Err(e) => {
            log::warn!("JNI call failed: {e}");
            None
        }
    }
}

fn call_void(name: &str, sig: &str, args: &[JValue]) {
    with_activity(|env, act| env.call_method(act, name, sig, args).map(|_| ()));
}

// ───────────────────────────── Java → Rust ─────────────────────────────

#[no_mangle]
pub extern "system" fn Java_com_lumen_app_LumenActivity_nativeMetrics(_env: JNIEnv, _c: JClass, density: jfloat, tz_min: jint) {
    DENSITY.store(density.to_bits(), Ordering::SeqCst);
    TZ_MIN.store(tz_min as i64, Ordering::SeqCst);
    wake();
}

#[no_mangle]
pub extern "system" fn Java_com_lumen_app_LumenActivity_nativeInsets(_env: JNIEnv, _c: JClass, l: jint, t: jint, r: jint, b: jint, ime: jboolean) {
    for (slot, v) in INSETS.iter().zip([l, t, r, b]) {
        slot.store(v.max(0), Ordering::SeqCst);
    }
    IME_VISIBLE.store(ime != 0, Ordering::SeqCst);
    wake();
}

#[no_mangle]
pub extern "system" fn Java_com_lumen_app_LumenActivity_nativeText(mut env: JNIEnv, _c: JClass, text: JString) {
    if let Ok(s) = env.get_string(&text) {
        let s: String = s.into();
        if !s.is_empty() {
            if let Ok(mut q) = INPUT.lock() {
                q.push(Input::Text(s));
            }
            wake();
        }
    }
}

/// An editing key pressed on the soft keyboard (Android key codes).
#[no_mangle]
pub extern "system" fn Java_com_lumen_app_LumenActivity_nativeKey(_env: JNIEnv, _c: JClass, code: jint, down: jboolean) {
    let key = match code {
        67 => Key::Backspace,
        112 => Key::Delete,
        66 | 160 => Key::Enter,
        61 => Key::Tab,
        111 => Key::Escape,
        19 => Key::ArrowUp,
        20 => Key::ArrowDown,
        21 => Key::ArrowLeft,
        22 => Key::ArrowRight,
        122 => Key::Home,
        123 => Key::End,
        92 => Key::PageUp,
        93 => Key::PageDown,
        _ => return,
    };
    if let Ok(mut q) = INPUT.lock() {
        q.push(Input::Key(key, down != 0));
    }
    wake();
}

#[no_mangle]
pub extern "system" fn Java_com_lumen_app_LumenActivity_nativeBack(_env: JNIEnv, _c: JClass) {
    BACK.store(true, Ordering::SeqCst);
    wake();
}

#[no_mangle]
pub extern "system" fn Java_com_lumen_app_LumenActivity_nativePicked(mut env: JNIEnv, _c: JClass, req: jint, name: JString, _mime: JString, data: JByteArray) {
    let target = PENDING_PICKS.lock().ok().and_then(|mut m| m.as_mut().and_then(|m| m.remove(&req)));
    let Some(target) = target else { return };
    let name: String = env.get_string(&name).map(|s| s.into()).unwrap_or_else(|_| "file".into());
    let bytes = env.convert_byte_array(&data).unwrap_or_default();
    if !bytes.is_empty() {
        crate::platform::push_picked(Picked { target, name, bytes });
    }
    wake();
}

#[no_mangle]
pub extern "system" fn Java_com_lumen_app_LumenActivity_nativePickCancelled(_env: JNIEnv, _c: JClass, req: jint) {
    if let Ok(mut m) = PENDING_PICKS.lock() {
        if let Some(m) = m.as_mut() {
            m.remove(&req);
        }
    }
}

// ───────────────────────────── Rust → Java ─────────────────────────────

pub fn show_ime() {
    call_void("showIme", "()V", &[]);
}

pub fn hide_ime() {
    call_void("hideIme", "()V", &[]);
}

/// Light icons on dark app chrome and vice versa (status bar + navigation bar).
pub fn set_dark_bars(dark: bool) {
    call_void("setDarkBars", "(Z)V", &[JValue::Bool(dark as jboolean)]);
}

/// Sends the app to the background (Back on the home screen of the app) instead of destroying it.
pub fn go_home() {
    call_void("goHome", "()V", &[]);
}

pub fn open_url(url: &str) {
    with_activity(|env, act| {
        let s = env.new_string(url)?;
        env.call_method(act, "openUrl", "(Ljava/lang/String;)V", &[JValue::Object(&s)]).map(|_| ())
    });
}

pub fn haptic(ms: u32) {
    call_void("haptic", "(I)V", &[JValue::Int(ms as jint)]);
}

pub fn clipboard_get() -> String {
    with_activity(|env, act| {
        let v = env.call_method(act, "getClipboardText", "()Ljava/lang/String;", &[])?.l()?;
        if v.is_null() {
            return Ok(String::new());
        }
        let js = JString::from(v);
        let s: String = env.get_string(&js)?.into();
        Ok(s)
    })
    .unwrap_or_default()
}

pub fn clipboard_set(text: &str) {
    with_activity(|env, act| {
        let s = env.new_string(text)?;
        env.call_method(act, "setClipboardText", "(Ljava/lang/String;)V", &[JValue::Object(&s)]).map(|_| ())
    });
}

pub fn notify_request() {
    call_void("requestNotificationPermission", "()V", &[]);
    call_void("requestExactAlarms", "()V", &[]);
}

/// Hands the upcoming reminders (JSON) to the Java side, which arms system alarms: they fire while the app is closed.
pub fn schedule(json: &str) {
    with_activity(|env, act| {
        let j = env.new_string(json)?;
        env.call_method(act, "scheduleReminders", "(Ljava/lang/String;)V", &[JValue::Object(&j)]).map(|_| ())
    });
}

pub fn notify(title: &str, body: &str, tag: &str) {
    with_activity(|env, act| {
        let (t, b, g) = (env.new_string(title)?, env.new_string(body)?, env.new_string(tag)?);
        env.call_method(act, "notifyUser", "(Ljava/lang/String;Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&t), JValue::Object(&b), JValue::Object(&g)])
            .map(|_| ())
    });
}

/// Android print dialog (print / "Save as PDF") for an HTML document.
pub fn print_html(name: &str, html: &str) -> String {
    let ok = with_activity(|env, act| {
        let (n, h) = (env.new_string(name)?, env.new_string(html)?);
        env.call_method(act, "printHtml", "(Ljava/lang/String;Ljava/lang/String;)V", &[JValue::Object(&n), JValue::Object(&h)]).map(|_| ())
    })
    .is_some();
    if ok { "Opening the Android print dialog (choose “Save as PDF” to export)".to_owned() } else { "Printing is not available".to_owned() }
}

/// Opens the system "create document" dialog and writes `data` to the chosen location.
pub fn save_file(name: &str, data: &[u8], mime: &str) -> String {
    let ok = with_activity(|env, act| {
        let (n, m) = (env.new_string(name)?, env.new_string(mime)?);
        let arr = env.byte_array_from_slice(data)?;
        env.call_method(act, "saveFile", "(Ljava/lang/String;Ljava/lang/String;[B)V", &[JValue::Object(&n), JValue::Object(&m), JValue::Object(&arr)]).map(|_| ())
    })
    .is_some();
    if ok { format!("Choose where to save {name}") } else { "Could not open the file dialog".to_owned() }
}

/// Opens the system file picker (Storage Access Framework). The result arrives through `platform::take_picked`.
pub fn pick_file(ctx: &Context, target: PickTarget, images_only: bool) {
    let _ = CTX.set(ctx.clone());
    let req = NEXT_REQ.fetch_add(1, Ordering::SeqCst);
    if let Ok(mut m) = PENDING_PICKS.lock() {
        m.get_or_insert_with(HashMap::new).insert(req, target);
    }
    let started = with_activity(|env, act| {
        let mime = env.new_string(if images_only { "image/*" } else { "*/*" })?;
        env.call_method(act, "pickFile", "(ILjava/lang/String;)V", &[JValue::Int(req), JValue::Object(&mime)]).map(|_| ())
    });
    if started.is_none() {
        if let Ok(mut m) = PENDING_PICKS.lock() {
            if let Some(m) = m.as_mut() {
                m.remove(&req);
            }
        }
    }
}

pub fn tz_minutes() -> i64 {
    TZ_MIN.load(Ordering::SeqCst)
}

// ───────────────────────────── per-frame glue ─────────────────────────────

pub fn density() -> f32 {
    f32::from_bits(DENSITY.load(Ordering::SeqCst))
}

pub fn ime_visible() -> bool {
    IME_VISIBLE.load(Ordering::SeqCst)
}

/// The Back button / gesture was used since the last call (consumes it).
pub fn take_back() -> bool {
    // API < 33: winit reports the Back key (see the "Lumen patch" in vendor/egui-winit); API >= 33: the Java back callback.
    let hit = BACK.swap(false, Ordering::SeqCst) | egui_winit::ANDROID_BACK.swap(false, Ordering::SeqCst);
    if !hit {
        return false;
    }
    // both paths may fire for one press: accept one Back per 250 ms
    static LAST: Mutex<Option<std::time::Instant>> = Mutex::new(None);
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    let now = std::time::Instant::now();
    if last.is_some_and(|t| now.duration_since(t).as_millis() < 250) {
        return false;
    }
    *last = Some(now);
    true
}

/// Queues a key press + release for the next frame (used to turn Back into Escape).
pub fn push_key(key: Key) {
    if let Ok(mut q) = INPUT.lock() {
        q.push(Input::Key(key, true));
        q.push(Input::Key(key, false));
    }
    wake();
}

thread_local! {
    static LAST_TAP: std::cell::Cell<Option<(f64, Pos2)>> = const { std::cell::Cell::new(None) };
    static WANTED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    static LAST_WANT_T: std::cell::Cell<f64> = const { std::cell::Cell::new(-100.0) };
    static LAST_SHOW_T: std::cell::Cell<f64> = const { std::cell::Cell::new(-100.0) };
    static DARK: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}

/// `App::raw_input_hook` on Android: exact pixel density, safe area, soft-keyboard events.
pub fn on_raw_input(ctx: &Context, raw: &mut RawInput) {
    let _ = CTX.get_or_init(|| ctx.clone());

    // 0. glyph atlas / texture size: 4096 px is plenty for a phone UI and halves the worst-case GPU memory of the 8192 px default.
    raw.max_texture_side = Some(raw.max_texture_side.unwrap_or(4096).min(4096));

    // 1. density — the logical size must equal the device's real dp size
    let d = density();
    if d > 0.5 {
        let native = raw.viewports.get(&raw.viewport_id).and_then(|v| v.native_pixels_per_point).unwrap_or(d).max(0.1);
        let zoom = (d / native).clamp(0.25, 8.0);
        if (ctx.zoom_factor() - zoom).abs() > 0.002 {
            ctx.set_zoom_factor(zoom);
        }
        // 2. safe area (points) — status bar, cutout, navigation bar, keyboard
        if let Some(sr) = raw.screen_rect {
            let p = |i: usize| INSETS[i].load(Ordering::SeqCst) as f32 / d;
            let min = sr.min + egui::vec2(p(0), p(1));
            let max = sr.max - egui::vec2(p(2), p(3));
            if max.x - min.x > 64.0 && max.y - min.y > 64.0 {
                raw.screen_rect = Some(Rect::from_min_max(min, max));
            }
        }
    }

    // 3. taps (a finger released without touch.rs having turned it into a scroll) — for the keyboard policy
    let now = raw.time.unwrap_or(0.0);
    for e in &raw.events {
        match e {
            Event::PointerButton { pos, pressed: false, button: egui::PointerButton::Primary, .. } => {
                LAST_TAP.with(|t| t.set(Some((now, *pos))));
            }
            _ => {}
        }
    }

    // 4. soft keyboard → egui events
    let drained: Vec<Input> = INPUT.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default();
    for i in drained {
        match i {
            Input::Text(s) => {
                let mut buf = String::new();
                for ch in s.chars() {
                    if ch == '\n' || ch == '\r' {
                        if !buf.is_empty() {
                            raw.events.push(Event::Text(std::mem::take(&mut buf)));
                        }
                        for pressed in [true, false] {
                            raw.events.push(Event::Key { key: Key::Enter, physical_key: Some(Key::Enter), pressed, repeat: false, modifiers: Modifiers::NONE });
                        }
                    } else if !ch.is_control() {
                        buf.push(ch);
                    }
                }
                if !buf.is_empty() {
                    raw.events.push(Event::Text(buf));
                }
            }
            Input::Key(key, pressed) => {
                raw.events.push(Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers: Modifiers::NONE });
            }
        }
    }
}

/// Called at the end of every frame: soft keyboard policy, system-bar icon colours, clipboard writes.
///
/// Keyboard policy (same spirit as the web build): the keyboard opens when the user *taps* a text field (or when a field gets focus
/// right after a tap, e.g. a search box that opens focused). A caret that appears by itself (new block, Enter) keeps the keyboard in
/// its current state. It closes once no text field has had focus for half a second.
pub fn after_frame(ctx: &Context, dark: bool) {
    let now = ctx.input(|i| i.time);
    frame_stats(now);

    if DARK.with(|d| d.get()) != Some(dark) {
        DARK.with(|d| d.set(Some(dark)));
        set_dark_bars(dark);
    }

    // clipboard: egui records Copy / Cut commands; hand them to the system clipboard
    let copied: Option<String> = ctx.output(|o| {
        o.commands.iter().rev().find_map(|c| match c {
            egui::OutputCommand::CopyText(t) => Some(t.clone()),
            _ => None,
        })
    });
    if let Some(t) = copied {
        clipboard_set(&t);
    }

    let ime = ctx.output(|o| o.ime.as_ref().map(|i| i.rect));
    let tap = LAST_TAP.with(|t| t.get());
    match ime {
        Some(rect) => {
            let was = WANTED.with(|w| w.replace(true));
            LAST_WANT_T.with(|t| t.set(now));
            let visible = ime_visible();
            let recently_shown = now - LAST_SHOW_T.with(|t| t.get()) < 0.8;
            if !visible && !recently_shown {
                let tapped_here = tap.is_some_and(|(t, p)| now - t < 0.45 && rect.expand(10.0).contains(p));
                let tapped_just_before = tap.is_some_and(|(t, _)| now - t < 0.6);
                if tapped_here || (!was && tapped_just_before) {
                    LAST_SHOW_T.with(|t| t.set(now));
                    show_ime();
                }
            }
        }
        None => {
            WANTED.with(|w| w.set(false));
            if ime_visible() {
                let since = now - LAST_WANT_T.with(|t| t.get());
                if since > 0.5 {
                    hide_ime();
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(550));
                }
            }
        }
    }
}

/// Power diagnostics: `adb logcat -s Lumen` prints how many frames were drawn per 10 s window. egui is reactive — an idle
/// workspace draws 0–2 frames per window (caret blink / reminder poll), a scroll or drag draws one per display refresh.
fn frame_stats(now: f64) {
    thread_local! {
        static WINDOW: std::cell::Cell<(f64, u32)> = const { std::cell::Cell::new((0.0, 0)) };
    }
    WINDOW.with(|w| {
        let (t0, n) = w.get();
        if t0 == 0.0 {
            w.set((now, 1));
        } else if now - t0 >= 10.0 {
            log::info!("frames drawn in the last {:.0}s: {} ({:.1} fps average)", now - t0, n, n as f64 / (now - t0));
            w.set((now, 1));
        } else {
            w.set((t0, n + 1));
        }
    });
}
