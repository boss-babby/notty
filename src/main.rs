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
        persist_window: true,
        // Plain 2D UI: no depth/stencil/MSAA buffers saves GPU memory and bandwidth.
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    #[cfg(windows)]
    trim_memory_in_background();
    eframe::run_native("Lumen", options, Box::new(|cc| Ok(Box::new(lumen::LumenApp::new(cc)))))
}

/// Windows: the OpenGL driver, the font atlas and start-up allocations leave ~100 MB in the *working set* that is never touched again.
/// Trimming it a few seconds after start (and again once the app settles) makes the resident size drop to what is really in use;
/// pages that are needed again fault back in on demand. This is what Task Manager's "Memory" column shows.
#[cfg(windows)]
fn trim_memory_in_background() {
    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> isize;
        fn SetProcessWorkingSetSize(process: isize, min: usize, max: usize) -> i32;
    }
    std::thread::spawn(|| {
        for secs in [4u64, 20, 90] {
            std::thread::sleep(std::time::Duration::from_secs(secs));
            // (usize::MAX, usize::MAX) = "empty the working set"
            unsafe {
                SetProcessWorkingSetSize(GetCurrentProcess(), usize::MAX, usize::MAX);
            }
        }
    });
}

#[cfg(target_arch = "wasm32")]
fn main() {}
