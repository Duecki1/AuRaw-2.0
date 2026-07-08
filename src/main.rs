mod app;
mod pipeline;
mod ui;

use app::AurawApp;

fn main() -> eframe::Result {
    // Initialize env_logger to output debug logs to standard output
    env_logger::init();

    let options = eframe::NativeOptions {
        viewport: eframe::egui::ViewportBuilder::default()
            .with_inner_size([1280.0, 720.0])
            .with_min_inner_size([800.0, 600.0]),
        ..Default::default()
    };

    eframe::run_native(
        "AuRaw",
        options,
        Box::new(|cc| Ok(Box::new(AurawApp::new(cc)))),
    )
}
