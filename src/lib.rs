mod ai_masks;
#[cfg(target_os = "android")]
mod android;
mod app;
pub mod pipeline;
pub mod regression;
pub mod sidecar;
mod ui;

pub use app::AurawApp;

/// Git revision embedded by `build.rs` for traceable binaries.
#[used]
pub static SOURCE_REVISION: &str = env!("AURAW_SOURCE_REVISION");

fn native_options() -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.device_descriptor = std::sync::Arc::new(|adapter| {
            let adapter_limits = adapter.limits();
            let mut required_limits = if adapter.get_info().backend == eframe::wgpu::Backend::Gl {
                eframe::wgpu::Limits::downlevel_webgl2_defaults()
            } else {
                eframe::wgpu::Limits::default()
            };
            required_limits.max_texture_dimension_2d = adapter_limits.max_texture_dimension_2d;
            eframe::wgpu::DeviceDescriptor {
                label: Some("AuRaw wgpu device"),
                required_limits,
                ..Default::default()
            }
        });
    }

    options
}

#[cfg(not(target_os = "android"))]
pub fn run_desktop() -> eframe::Result {
    env_logger::init();

    let mut options = native_options();
    options.viewport = eframe::egui::ViewportBuilder::default()
        .with_inner_size([1280.0, 720.0])
        .with_min_inner_size([480.0, 480.0]);

    eframe::run_native(
        "AuRaw",
        options,
        Box::new(|cc| Ok(Box::new(AurawApp::new(cc)))),
    )
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(android_app: android_activity::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("AuRaw"),
    );

    let mut options = native_options();
    options.android_app = Some(android_app.clone());
    options.viewport = eframe::egui::ViewportBuilder::default().with_fullscreen(true);

    let result = eframe::run_native(
        "AuRaw",
        options,
        Box::new(move |cc| Ok(Box::new(AurawApp::new_android(cc, android_app.clone())))),
    );
    if let Err(error) = result {
        log::error!("AuRaw terminated: {error:#}");
    }
}
