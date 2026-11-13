//! Thin platform layer: file downloads and text file import that behave the same on desktop and in the browser.

/// Saves `data` as a file the user can reach. Desktop: writes to the Downloads folder.
/// Web: triggers a browser download. Returns a human readable status message.
pub fn save_download(name: &str, data: &[u8], mime: &str) -> String {
    #[cfg(target_os = "android")]
    {
        crate::android::save_file(name, data, mime)
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        let _ = mime;
        let home = std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).unwrap_or_else(|_| ".".into());
        let mut dir = std::path::PathBuf::from(home).join("Downloads");
        if !dir.is_dir() {
            dir = std::env::current_dir().unwrap_or_else(|_| ".".into());
        }
        let mut path = dir.join(name);
        let mut n = 1;
        while path.exists() {
            let stem = std::path::Path::new(name).file_stem().and_then(|s| s.to_str()).unwrap_or("file");
            let ext = std::path::Path::new(name).extension().and_then(|s| s.to_str()).unwrap_or("");
            path = dir.join(format!("{stem} ({n}).{ext}"));
            n += 1;
        }
        match std::fs::write(&path, data) {
            Ok(_) => format!("Saved to {}", path.display()),
            Err(e) => format!("Could not save file: {e}"),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let run = || -> Result<(), wasm_bindgen::JsValue> {
            let arr = js_sys::Uint8Array::from(data);
            let parts = js_sys::Array::of1(&arr);
            let opts = web_sys::BlobPropertyBag::new();
            opts.set_type(mime);
            let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts)?;
            let url = web_sys::Url::create_object_url_with_blob(&blob)?;
            let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
            let a: web_sys::HtmlAnchorElement = doc.create_element("a")?.dyn_into()?;
            a.set_href(&url);
            a.set_download(name);
            a.click();
            web_sys::Url::revoke_object_url(&url)?;
            Ok(())
        };
        match run() {
            Ok(_) => format!("Downloaded {name}"),
            Err(_) => "Download failed".to_owned(),
        }
    }
}

/// Reads bytes of a dropped file (native gives a path, web gives bytes directly).
pub fn dropped_bytes(f: &egui::DroppedFile) -> Option<Vec<u8>> {
    if let Some(b) = &f.bytes {
        return Some(b.to_vec());
    }
    #[cfg(not(target_arch = "wasm32"))]
    if let Some(p) = &f.path {
        return std::fs::read(p).ok();
    }
    None
}

pub fn file_name(f: &egui::DroppedFile) -> String {
    if !f.name.is_empty() {
        return f.name.clone();
    }
    f.path.as_ref().and_then(|p| p.file_name()).and_then(|n| n.to_str()).unwrap_or("").to_owned()
}

thread_local! {
    static PASTE: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// Starts reading the system clipboard. The text arrives via [`take_paste`] (immediately on desktop, a moment later
/// in the browser because the Clipboard API is asynchronous).
pub fn request_paste(ctx: &egui::Context) {
    #[cfg(target_os = "android")]
    {
        let text = crate::android::clipboard_get();
        PASTE.with(|p| *p.borrow_mut() = Some(text));
        ctx.request_repaint();
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        let text = arboard::Clipboard::new().and_then(|mut c| c.get_text()).unwrap_or_default();
        PASTE.with(|p| *p.borrow_mut() = Some(text));
        ctx.request_repaint();
    }
    #[cfg(target_arch = "wasm32")]
    {
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut text = String::new();
            if let Some(w) = web_sys::window() {
                if let Ok(v) = wasm_bindgen_futures::JsFuture::from(w.navigator().clipboard().read_text()).await {
                    text = v.as_string().unwrap_or_default();
                }
            }
            PASTE.with(|p| *p.borrow_mut() = Some(text));
            ctx.request_repaint();
        });
    }
}

pub fn take_paste() -> Option<String> {
    PASTE.with(|p| p.borrow_mut().take())
}

// ───────────────────────────── opening links ─────────────────────────────

/// Opens a URL in the system browser (desktop) or a new tab (web).
pub fn open_url(ctx: &egui::Context, url: &str) {
    #[cfg(target_os = "android")]
    {
        let _ = ctx;
        crate::android::open_url(url);
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        let _ = ctx;
        let _ = webbrowser::open(url);
    }
    #[cfg(target_arch = "wasm32")]
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

/// A short vibration (phones, web build only) — confirms a press-and-hold. Silently does nothing elsewhere.
pub fn haptic(ctx: &egui::Context, ms: u32) {
    let _ = ctx;
    #[cfg(target_arch = "wasm32")]
    if let Some(w) = web_sys::window() {
        let _ = w.navigator().vibrate_with_duration(ms);
    }
    #[cfg(target_os = "android")]
    crate::android::haptic(ms);
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let _ = ms;
}

// ───────────────────────────── file picking ─────────────────────────────

/// Where a picked file goes once the (asynchronous) dialog returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickTarget {
    /// Import dialog: a Markdown / text file (`mode` 0 → page), a CSV / TSV file (1 → database) or a JSON backup (2).
    ImportFile { mode: u8 },
    BlockImage { page: u64, block: u64 },
    BlockFile { page: u64, block: u64 },
    Cover { page: u64 },
    Icon { page: u64 },
    /// An image card of a canvas.
    CanvasImage { page: u64, node: u64 },
}

pub struct Picked {
    pub target: PickTarget,
    pub name: String,
    pub bytes: Vec<u8>,
}

static PICKED: std::sync::Mutex<Vec<Picked>> = std::sync::Mutex::new(Vec::new());

pub fn take_picked() -> Vec<Picked> {
    PICKED.lock().map(|mut v| std::mem::take(&mut *v)).unwrap_or_default()
}

pub fn push_picked(p: Picked) {
    if let Ok(mut v) = PICKED.lock() {
        v.push(p);
    }
}

/// True when a native "open file" dialog exists on this platform (Windows, macOS, web).
pub fn can_pick_files() -> bool {
    cfg!(any(windows, target_os = "macos", target_os = "android", target_arch = "wasm32"))
}

/// Opens the system file dialog; the result shows up in [`take_picked`].
pub fn pick_file(ctx: &egui::Context, target: PickTarget, images_only: bool) {
    #[cfg(any(windows, target_os = "macos"))]
    {
        let ctx = ctx.clone();
        std::thread::spawn(move || {
            let mut d = rfd::FileDialog::new();
            if images_only {
                d = d.add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"]);
            }
            if let PickTarget::ImportFile { mode } = target {
                d = match mode {
                    0 => d.add_filter("Markdown / text", &["md", "markdown", "txt"]),
                    1 => d.add_filter("CSV / TSV", &["csv", "tsv", "txt"]),
                    _ => d.add_filter("Lumen backup", &["json"]),
                };
            }
            if let Some(path) = d.pick_file() {
                if let Ok(bytes) = std::fs::read(&path) {
                    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file").to_owned();
                    push_picked(Picked { target, name, bytes });
                }
            }
            ctx.request_repaint();
        });
    }
    #[cfg(target_arch = "wasm32")]
    {
        let ctx = ctx.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let mut d = rfd::AsyncFileDialog::new();
            if images_only {
                d = d.add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp", "bmp"]);
            }
            if let PickTarget::ImportFile { mode } = target {
                d = match mode {
                    0 => d.add_filter("Markdown / text", &["md", "markdown", "txt"]),
                    1 => d.add_filter("CSV / TSV", &["csv", "tsv", "txt"]),
                    _ => d.add_filter("Lumen backup", &["json"]),
                };
            }
            if let Some(h) = d.pick_file().await {
                let bytes = h.read().await;
                push_picked(Picked { target, name: h.file_name(), bytes });
            }
            ctx.request_repaint();
        });
    }
    #[cfg(target_os = "android")]
    crate::android::pick_file(ctx, target, images_only);
    #[cfg(not(any(windows, target_os = "macos", target_os = "android", target_arch = "wasm32")))]
    {
        let _ = (ctx, target, images_only);
    }
}

// ───────────────────────────── live embeds (web) ─────────────────────────────

/// An embed block that wants a live web page drawn over its area.
#[derive(Clone, Debug)]
pub struct EmbedRect {
    pub id: u64,
    pub url: String,
    pub rect: egui::Rect,
    /// Play with a native `<video>` element instead of an iframe.
    pub video: bool,
    /// Video playback options (1 = autoplay, 2 = muted, 4 = loop).
    pub opts: u8,
    /// Visible part (the scroll area's clip rectangle).
    pub clip: egui::Rect,
}

/// Web: creates / moves / removes real `<iframe>` elements over the canvas so websites, videos and maps are embedded live.
/// Desktop: no-op (blocks show a preview card and open the system browser).
pub fn sync_iframes(items: &[EmbedRect]) {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        let run = || -> Option<()> {
            let doc = web_sys::window()?.document()?;
            let container = match doc.get_element_by_id("lumen-embeds") {
                Some(c) => c,
                None => {
                    let c = doc.create_element("div").ok()?;
                    c.set_id("lumen-embeds");
                    c.set_attribute("style", "position:fixed;left:0;top:0;width:0;height:0;z-index:5;").ok()?;
                    doc.body()?.append_child(&c).ok()?;
                    c
                }
            };
            let want: std::collections::HashSet<String> = items.iter().map(|i| format!("lumen-embed-{}", i.id)).collect();
            let kids = container.children();
            let mut dead = vec![];
            for n in 0..kids.length() {
                if let Some(k) = kids.item(n) {
                    if !want.contains(&k.id()) {
                        dead.push(k);
                    }
                }
            }
            for k in dead {
                k.remove();
            }
            for it in items {
                let eid = format!("lumen-embed-{}", it.id);
                let frame: web_sys::HtmlElement = match doc.get_element_by_id(&eid) {
                    Some(e) => e.dyn_into().ok()?,
                    None => {
                        let f: web_sys::HtmlElement = doc.create_element(if it.video { "video" } else { "iframe" }).ok()?.dyn_into().ok()?;
                        f.set_id(&eid);
                        if it.video {
                            f.set_attribute("controls", "").ok()?;
                            f.set_attribute("playsinline", "").ok()?;
                            f.set_attribute("preload", "metadata").ok()?;
                            f.set_attribute("controlslist", "nodownload").ok()?;
                            if it.opts & 1 != 0 {
                                f.set_attribute("autoplay", "").ok()?;
                            }
                            if it.opts & 2 != 0 {
                                f.set_attribute("muted", "").ok()?;
                                let _ = js_sys::Reflect::set(&f, &"muted".into(), &true.into());
                            }
                            if it.opts & 4 != 0 {
                                f.set_attribute("loop", "").ok()?;
                            }
                        } else {
                            f.set_attribute("allow", "autoplay; fullscreen; clipboard-write; encrypted-media; picture-in-picture").ok()?;
                            f.set_attribute("allowfullscreen", "true").ok()?;
                            f.set_attribute("loading", "lazy").ok()?;
                            f.set_attribute("referrerpolicy", "no-referrer-when-downgrade").ok()?;
                        }
                        container.append_child(&f).ok()?;
                        f
                    }
                };
                if frame.get_attribute("src").as_deref() != Some(it.url.as_str()) {
                    frame.set_attribute("src", &it.url).ok()?;
                }
                let vis = it.rect.intersect(it.clip);
                let st = frame.style();
                if vis.width() < 2.0 || vis.height() < 2.0 {
                    st.set_property("display", "none").ok()?;
                    continue;
                }
                let (t, l) = ((vis.top() - it.rect.top()).max(0.0), (vis.left() - it.rect.left()).max(0.0));
                let (b, r) = ((it.rect.bottom() - vis.bottom()).max(0.0), (it.rect.right() - vis.right()).max(0.0));
                st.set_property("display", "block").ok()?;
                st.set_property("position", "absolute").ok()?;
                st.set_property("border", "0").ok()?;
                st.set_property("background", if it.video { "#000" } else { "#fff" }).ok()?;
                st.set_property("object-fit", "contain").ok()?;
                st.set_property("left", &format!("{}px", it.rect.left())).ok()?;
                st.set_property("top", &format!("{}px", it.rect.top())).ok()?;
                st.set_property("width", &format!("{}px", it.rect.width())).ok()?;
                st.set_property("height", &format!("{}px", it.rect.height())).ok()?;
                st.set_property("clip-path", &format!("inset({t}px {r}px {b}px {l}px round 8px)")).ok()?;
            }
            Some(())
        };
        run();
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = items;
    }
}

/// True when websites can be previewed inside the app: iframes on the web, the native WebView2 child window on Windows.
pub fn live_preview_supported() -> bool {
    cfg!(any(windows, target_arch = "wasm32"))
}

/// Desktop counterpart of [`sync_iframes`]: the system web view (WebView2 on Windows) is created when a preview starts, moved over the
/// block while it is shown, and **destroyed as soon as the preview stops**, which gives its memory back to the system.
pub fn sync_native_previews(frame: &eframe::Frame, items: &[EmbedRect]) {
    #[cfg(windows)]
    native::sync(frame, items);
    #[cfg(not(windows))]
    {
        let _ = (frame, items);
    }
}

#[cfg(windows)]
mod native {
    use super::EmbedRect;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use wry::dpi::{LogicalPosition, LogicalSize};
    use wry::{Rect, WebView, WebViewBuilder};

    /// At most this many native views exist at once (each one is a whole browser process tree).
    const MAX_VIEWS: usize = 2;

    thread_local! {
        static VIEWS: RefCell<HashMap<u64, (String, WebView)>> = RefCell::new(HashMap::new());
    }

    pub fn sync(frame: &eframe::Frame, items: &[EmbedRect]) {
        VIEWS.with(|v| {
            let mut v = v.borrow_mut();
            // dropping a WebView closes it and frees its memory
            v.retain(|id, _| items.iter().any(|i| i.id == *id));
            for it in items.iter().take(MAX_VIEWS) {
                let count = v.len();
                let vis = it.rect.intersect(it.clip);
                let fully_visible = vis.width() >= it.rect.width() - 1.0 && vis.height() >= it.rect.height() - 1.0;
                let bounds = Rect {
                    position: LogicalPosition::new(it.rect.left() as f64, it.rect.top() as f64).into(),
                    size: LogicalSize::new(it.rect.width().max(1.0) as f64, it.rect.height().max(1.0) as f64).into(),
                };
                match v.get_mut(&it.id) {
                    Some((url, wv)) => {
                        if *url != it.url {
                            let _ = wv.load_url(&it.url);
                            *url = it.url.clone();
                        }
                        let _ = wv.set_bounds(bounds);
                        let _ = wv.set_visible(fully_visible);
                    }
                    None if fully_visible && count < MAX_VIEWS => {
                        if let Ok(wv) = WebViewBuilder::new().with_url(&it.url).with_bounds(bounds).build_as_child(frame) {
                            v.insert(it.id, (it.url.clone(), wv));
                        }
                    }
                    None => {}
                }
            }
        });
    }
}

// ───────────────────────────── printing ─────────────────────────────

/// Shows a self-contained HTML document in the browser (new tab on the web, system browser on desktop) so the user can print
/// it or save it as PDF. Returns a status message.
pub fn open_html(name: &str, html: &str) -> String {
    #[cfg(target_os = "android")]
    {
        crate::android::print_html(name, html)
    }
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    {
        let mut path = std::env::temp_dir();
        path.push(name);
        match std::fs::write(&path, html.as_bytes()) {
            Ok(_) => match webbrowser::open(&path.to_string_lossy()) {
                Ok(_) => "Opened the print preview in your browser".to_owned(),
                Err(_) => format!("Saved the print version to {}", path.display()),
            },
            Err(e) => format!("Could not write the print file: {e}"),
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let run = || -> Result<bool, wasm_bindgen::JsValue> {
            let arr = js_sys::Uint8Array::from(html.as_bytes());
            let parts = js_sys::Array::of1(&arr);
            let opts = web_sys::BlobPropertyBag::new();
            opts.set_type("text/html;charset=utf-8");
            let blob = web_sys::Blob::new_with_u8_array_sequence_and_options(&parts, &opts)?;
            let url = web_sys::Url::create_object_url_with_blob(&blob)?;
            let win = web_sys::window().ok_or("no window")?;
            Ok(win.open_with_url_and_target(&url, "_blank")?.is_some())
        };
        match run() {
            Ok(true) => "Print preview opened in a new tab".to_owned(),
            Ok(false) => {
                // pop-up blocked → fall back to a download
                save_download(name, html.as_bytes(), "text/html")
            }
            Err(_) => "Could not open the print preview".to_owned(),
        }
    }
}

/// `true` when the primary input is a finger (phones / tablets): decided once at start-up on the web build via the
/// `(pointer: coarse)` media query, so tap targets are big *before* the first touch.
#[cfg(target_arch = "wasm32")]
pub fn coarse_pointer() -> bool {
    use wasm_bindgen::JsCast;
    let Some(w) = web_sys::window() else { return false };
    let Ok(f) = js_sys::Reflect::get(&w, &"matchMedia".into()) else { return false };
    let Ok(f) = f.dyn_into::<js_sys::Function>() else { return false };
    let Ok(mq) = f.call1(&w, &"(pointer: coarse)".into()) else { return false };
    js_sys::Reflect::get(&mq, &"matches".into()).ok().and_then(|v| v.as_bool()).unwrap_or(false)
}

/// Web: asks the browser not to evict this site's storage under disk pressure (`navigator.storage.persist()`), so the workspace in
/// localStorage / IndexedDB survives. Other targets store in files and need nothing.
#[cfg(target_arch = "wasm32")]
pub fn request_persistent_storage() {
    let Some(w) = web_sys::window() else { return };
    let nav = w.navigator();
    if let Ok(st) = js_sys::Reflect::get(&nav, &"storage".into()) {
        use wasm_bindgen::JsCast;
        if let Some(f) = js_sys::Reflect::get(&st, &"persist".into()).ok().and_then(|f| f.dyn_into::<js_sys::Function>().ok()) {
            let _ = f.call0(&st);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
pub fn request_persistent_storage() {}

#[cfg(not(target_arch = "wasm32"))]
pub fn coarse_pointer() -> bool {
    cfg!(target_os = "android")
}

// ───────────────────────────── time zone ─────────────────────────────

/// Minutes to add to UTC to get local time (native builds). Windows asks the OS; elsewhere `LUMEN_TZ_MIN` or UTC.
#[cfg(not(target_arch = "wasm32"))]
pub fn tz_offset_minutes() -> i64 {
    #[cfg(windows)]
    {
        #[repr(C)]
        struct SystemTime([u16; 8]);
        #[repr(C)]
        struct Tzi {
            bias: i32,
            std_name: [u16; 32],
            std_date: SystemTime,
            std_bias: i32,
            dst_name: [u16; 32],
            dst_date: SystemTime,
            dst_bias: i32,
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn GetTimeZoneInformation(info: *mut Tzi) -> u32;
        }
        // SAFETY: plain C struct filled by the OS; zeroed is a valid initial value.
        unsafe {
            let mut t: Tzi = std::mem::zeroed();
            let r = GetTimeZoneInformation(&mut t);
            let bias = t.bias + match r {
                1 => t.std_bias,
                2 => t.dst_bias,
                _ => 0,
            };
            -(bias as i64)
        }
    }
    #[cfg(target_os = "android")]
    {
        crate::android::tz_minutes()
    }
    #[cfg(not(any(windows, target_os = "android")))]
    {
        std::env::var("LUMEN_TZ_MIN").ok().and_then(|v| v.trim().parse().ok()).unwrap_or(0)
    }
}

// ───────────────────────────── notifications ─────────────────────────────
//
// Windows: a native toast (PowerShell → Windows.UI.Notifications, no console window, no extra dependency).
// Web: the page's `lumenNotify` bridge (web/index.html) → service-worker `showNotification` (works for installed PWAs and on phones).
// Other desktops: `notify-send` / `osascript` when present.

/// Permission state of web notifications: "granted", "denied", "default" or "unsupported". Desktop builds are always "granted".
pub fn notify_permission() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        use wasm_bindgen::JsCast;
        if let Some(w) = web_sys::window() {
            if let Ok(f) = js_sys::Reflect::get(&w, &"lumenNotifyState".into()) {
                if let Some(f) = f.dyn_ref::<js_sys::Function>() {
                    if let Ok(v) = f.call0(&w) {
                        return v.as_string().unwrap_or_else(|| "unsupported".into());
                    }
                }
            }
        }
        "unsupported".into()
    }
    #[cfg(not(target_arch = "wasm32"))]
    "granted".into()
}

/// Asks the browser for notification permission (must run inside a click handler). No-op on desktop.
pub fn notify_request() {
    #[cfg(target_arch = "wasm32")]
    call_bridge("lumenNotifyRequest", &[]);
    #[cfg(target_os = "android")]
    crate::android::notify_request();
}

/// Shows a system notification right now.
pub fn notify_os(title: &str, body: &str, tag: &str) {
    #[cfg(target_arch = "wasm32")]
    call_bridge("lumenNotify", &[title.into(), body.into(), tag.into()]);
    #[cfg(windows)]
    windows_toast(title, body);
    #[cfg(target_os = "android")]
    crate::android::notify(title, body, tag);
    #[cfg(all(not(windows), not(target_arch = "wasm32"), not(target_os = "android")))]
    {
        let _ = tag;
        #[cfg(target_os = "macos")]
        {
            let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
            let _ = std::process::Command::new("osascript").arg("-e").arg(format!("display notification \"{}\" with title \"{}\"", esc(body), esc(title))).spawn();
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = std::process::Command::new("notify-send").arg("--app-name=Lumen").arg(title).arg(body).spawn();
        }
    }
}

/// Hands the upcoming reminders (JSON: `[{"at": epoch_ms, "title", "body", "tag"}]`) to the page so the browser can fire them
/// with its own timers even while the tab is in the background and the canvas is not repainting. No-op on desktop.
pub fn schedule_push(json: &str) {
    #[cfg(target_arch = "wasm32")]
    call_bridge("lumenSchedule", &[json.into()]);
    #[cfg(target_os = "android")]
    crate::android::schedule(json);
    #[cfg(not(any(target_arch = "wasm32", target_os = "android")))]
    let _ = json;
}

#[cfg(target_arch = "wasm32")]
fn call_bridge(name: &str, args: &[wasm_bindgen::JsValue]) {
    use wasm_bindgen::JsCast;
    let Some(w) = web_sys::window() else { return };
    let Ok(f) = js_sys::Reflect::get(&w, &name.into()) else { return };
    let Some(f) = f.dyn_ref::<js_sys::Function>() else { return };
    let arr = js_sys::Array::new();
    for a in args {
        arr.push(a);
    }
    let _ = f.apply(&w, &arr);
}

#[cfg(windows)]
fn windows_toast(title: &str, body: &str) {
    use std::os::windows::process::CommandExt;
    fn xml(s: &str) -> String {
        s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&apos;")
    }
    let script = format!(
        "$ErrorActionPreference='Stop';\
         [Windows.UI.Notifications.ToastNotificationManager, Windows.UI.Notifications, ContentType = WindowsRuntime] | Out-Null;\
         [Windows.Data.Xml.Dom.XmlDocument, Windows.Data.Xml.Dom.XmlDocument, ContentType = WindowsRuntime] | Out-Null;\
         $x = New-Object Windows.Data.Xml.Dom.XmlDocument;\
         $x.LoadXml('<toast><visual><binding template=\"ToastGeneric\"><text>{}</text><text>{}</text></binding></visual></toast>');\
         $t = [Windows.UI.Notifications.ToastNotification]::new($x);\
         [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('{{1AC14E77-02E7-4E5D-B744-2EB1AE5198B7}}\\WindowsPowerShell\\v1.0\\powershell.exe').Show($t)",
        xml(title),
        xml(body)
    );
    // -EncodedCommand = base64 of the UTF-16LE script: no quoting problems whatever the text contains.
    let mut bytes = Vec::with_capacity(script.len() * 2);
    for u in script.encode_utf16() {
        bytes.extend_from_slice(&u.to_le_bytes());
    }
    let enc = base64(&bytes);
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let _ = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-WindowStyle", "Hidden", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &enc])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn();
}

#[cfg(windows)]
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}


/// The address of the website the app runs on (web build) — used in the MCP connection snippets.
pub fn site_origin() -> String {
    #[cfg(target_arch = "wasm32")]
    {
        return web_sys::window().and_then(|w| w.location().origin().ok()).unwrap_or_default();
    }
    #[allow(unreachable_code)]
    String::new()
}
