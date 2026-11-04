// Hide the console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

#[cfg(not(target_arch = "wasm32"))]
fn main() -> eframe::Result {
    env_logger::init();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("Lumen")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([760.0, 480.0]),
        vsync: true,
        ..Default::default()
    };
    eframe::run_native("Lumen", options, Box::new(|cc| Ok(Box::new(lumen::LumenApp::new(cc)))))
}

#[cfg(target_arch = "wasm32")]
fn main() {}
