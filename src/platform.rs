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
