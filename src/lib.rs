//! Lumen — a Notion-style workspace written entirely in Rust with egui.
//! The same code runs as a native desktop app and as WebAssembly in the browser.

mod app;
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
mod clip;
mod model;
mod platform;
mod theme;
mod widgets;

#[cfg(test)]
mod tests;

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
