//! Thin platform layer: file downloads and text file import that behave the same on desktop and in the browser.

/// Saves `data` as a file the user can reach. Desktop: writes to the Downloads folder.
/// Web: triggers a browser download. Returns a human readable status message.
pub fn save_download(name: &str, data: &[u8], mime: &str) -> String {
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
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
    #[cfg(not(target_arch = "wasm32"))]
    {
        let _ = ctx;
        let _ = webbrowser::open(url);
    }
    #[cfg(target_arch = "wasm32")]
    ctx.open_url(egui::OpenUrl::new_tab(url));
}

// ───────────────────────────── file picking ─────────────────────────────

/// Where a picked file goes once the (asynchronous) dialog returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PickTarget {
    BlockImage { page: u64, block: u64 },
    BlockFile { page: u64, block: u64 },
    Cover { page: u64 },
    Icon { page: u64 },
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
    cfg!(any(windows, target_os = "macos", target_arch = "wasm32"))
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
            if let Some(h) = d.pick_file().await {
                let bytes = h.read().await;
                push_picked(Picked { target, name: h.file_name(), bytes });
            }
            ctx.request_repaint();
        });
    }
    #[cfg(not(any(windows, target_os = "macos", target_arch = "wasm32")))]
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

// ───────────────────────────── printing ─────────────────────────────

/// Shows a self-contained HTML document in the browser (new tab on the web, system browser on desktop) so the user can print
/// it or save it as PDF. Returns a status message.
pub fn open_html(name: &str, html: &str) -> String {
    #[cfg(not(target_arch = "wasm32"))]
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
