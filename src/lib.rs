//! Lumen — a Notion-style workspace written entirely in Rust with egui.
//! The same code runs as a native desktop app and as WebAssembly in the browser.

#[cfg(target_os = "android")]
mod android;
mod app;
mod canvas;
mod canvasdraw;
mod canvasui;
mod automation;
mod autoui;
mod uploader;
mod auth;
mod database;
mod dates;
mod editor;
mod emoji;
mod formula;
mod history;
mod print;
mod timeline;
mod icons;
mod io;
mod lock;
mod lucide;
mod media;
mod mediablocks;
mod multisel;
mod pdf;
mod pdfnotes;
mod pdftext;
mod pickers;
mod rich;
mod script;
mod scripthost;
mod scriptui;
mod clip;
mod model;
mod platform;
mod dashboard;
mod inlinew;
mod dashcfg;
mod dashcharts;
mod dashdata;
mod dashedit;
mod dashwidgets;
mod graph;
mod smooth;
mod views;
mod theme;
mod touch;
mod widgets;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod dashtests;

pub use app::LumenApp;

#[cfg(target_arch = "wasm32")]
mod web {
    use wasm_bindgen::prelude::*;

    /// Handle used by `index.html` to start/stop the app on a canvas.
    #[wasm_bindgen]
    pub struct LumenWeb {
        runner: eframe::WebRunner,
    }

    #[wasm_bindgen]
    impl LumenWeb {
        #[wasm_bindgen(constructor)]
        pub fn new() -> Self {
            eframe::WebLogger::init(log::LevelFilter::Warn).ok();
            Self { runner: eframe::WebRunner::new() }
        }

        #[wasm_bindgen]
        pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
            self.runner
                .start(canvas, eframe::WebOptions::default(), Box::new(|cc| Ok(Box::new(crate::LumenApp::new(cc)))))
                .await
        }

        #[wasm_bindgen]
        pub fn destroy(&self) {
            self.runner.destroy();
        }
    }

    impl Default for LumenWeb {
        fn default() -> Self {
            Self::new()
        }
    }
}

/// Android entry point (NativeActivity → android-activity → here). The Java half is `android/java/com/lumen/app/LumenActivity.java`.
#[cfg(target_os = "android")]
#[no_mangle]
pub fn android_main(app: android_activity::AndroidApp) {
    android::init(&app);
    let options = eframe::NativeOptions {
        android_app: Some(app),
        renderer: eframe::Renderer::Glow,
        vsync: true,
        // Plain 2D UI: no depth / stencil / MSAA buffers (saves GPU memory and battery).
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    if let Err(e) = eframe::run_native("Lumen", options, Box::new(|cc| Ok(Box::new(LumenApp::new(cc))))) {
        log::error!("Lumen stopped: {e}");
    }
}
