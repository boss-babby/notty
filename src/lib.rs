//! Lumen — a Notion-style workspace written entirely in Rust with egui.
//! The same code runs as a native desktop app and as WebAssembly in the browser.

mod agent_build;
mod agent_tools;
mod ai;
mod aiui;
#[cfg(target_os = "android")]
mod android;
mod app;
mod auth;
mod automation;
mod autosave;
mod autoui;
mod bigdata;
mod bigtable;
mod bigui;
mod canvas;
mod canvasdash;
mod canvasdraw;
mod canvasexport;
mod canvasfmt;
mod canvasui;
mod canvasx;
mod clip;
mod collab;
mod crisp;
mod dashboard;
mod dashcfg;
mod dashcharts;
mod dashdata;
mod dashedit;
mod dashwidgets;
mod database;
mod dates;
mod demo;
mod editor;
mod emoji;
mod flow;
mod flowbg;
mod flowui;
mod formula;
mod graph;
mod history;
mod icons;
mod inlinew;
mod io;
mod lock;
mod lucide;
mod math;
mod mcp;
mod mdtable;
mod media;
mod mediablocks;
mod memgov;
mod model;
mod multisel;
mod outline;
mod pdf;
mod pdfbar;
mod pdfnotes;
mod pdftext;
mod pickers;
mod platform;
mod print;
mod rich;
mod script;
mod scripthost;
mod scriptui;
mod smooth;
mod tableblock;
mod theme;
mod timeline;
mod touch;
mod uploader;
mod views;
mod whiteboard;
mod widgets;
mod workspaces;

#[cfg(test)]
mod agent_tests;
#[cfg(test)]
mod dashtests;
#[cfg(test)]
mod tests;

pub use app::LumenApp;

/// Entry point of the headless `lumen-mcp` binary (stdio MCP server over a workspace backup file).
#[cfg(not(target_arch = "wasm32"))]
pub fn mcp_cli(args: Vec<String>) -> i32 {
    mcp::run_cli(args)
}

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
            Self {
                runner: eframe::WebRunner::new(),
            }
        }

        #[wasm_bindgen]
        pub async fn start(&self, canvas: web_sys::HtmlCanvasElement) -> Result<(), JsValue> {
            self.runner
                .start(
                    canvas,
                    eframe::WebOptions::default(),
                    Box::new(|cc| Ok(Box::new(crate::LumenApp::new(cc)))),
                )
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
    if let Err(e) = eframe::run_native(
        "Lumen",
        options,
        Box::new(|cc| Ok(Box::new(LumenApp::new(cc)))),
    ) {
        log::error!("Lumen stopped: {e}");
    }
}
