mod ai_denoise;
mod ai_masks;
#[cfg(target_os = "android")]
mod android;
mod app;
mod diagnostics;
mod file_ops;
mod inpainting;
mod performance_settings;
pub mod pipeline;
pub mod regression;
pub mod sidecar;
mod thumbnail_cache;
mod ui;

#[cfg(test)]
#[path = "../build_support/shader_preprocessor.rs"]
mod shader_preprocessor;

pub use app::AurawApp;

/// Git revision embedded by `build.rs` for traceable binaries.
#[used]
pub static SOURCE_REVISION: &str = env!("AURAW_SOURCE_REVISION");

fn native_options() -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    // Keep only one frame queued so Android presents the newest gesture state
    // on the next vsync instead of building up several frames of input latency.
    #[cfg(target_os = "android")]
    {
        options.wgpu_options.surface = eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY;
    }

    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        setup.device_descriptor = std::sync::Arc::new(|adapter| {
            let info = adapter.get_info();
            let adapter_limits = adapter.limits();
            crate::diagnostics::set_gpu_info(format!(
                "name={}\nbackend={:?}\ndevice_type={:?}\nvendor=0x{:04x}\ndevice=0x{:04x}\ndriver={}\ndriver_info={}\nmax_texture_dimension_2d={}\nfeatures={:?}",
                info.name,
                info.backend,
                info.device_type,
                info.vendor,
                info.device,
                info.driver,
                info.driver_info,
                adapter_limits.max_texture_dimension_2d,
                adapter.features(),
            ));
            let mut required_limits = if info.backend == eframe::wgpu::Backend::Gl {
                eframe::wgpu::Limits::downlevel_webgl2_defaults()
            } else {
                eframe::wgpu::Limits::default()
            };
            required_limits.max_texture_dimension_2d = adapter_limits.max_texture_dimension_2d;
            #[cfg(target_os = "android")]
            let required_features = {
                let mut features = eframe::wgpu::Features::empty();
                if adapter
                    .features()
                    .contains(eframe::wgpu::Features::PIPELINE_CACHE)
                {
                    features |= eframe::wgpu::Features::PIPELINE_CACHE;
                }
                features
            };
            #[cfg(not(target_os = "android"))]
            let required_features = eframe::wgpu::Features::empty();
            eframe::wgpu::DeviceDescriptor {
                label: Some("AuRaw wgpu device"),
                required_features,
                required_limits,
                ..Default::default()
            }
        });
    }

    options
}

#[cfg(not(target_os = "android"))]
pub fn run_onnx_runtime_probe_cli(args: &[String]) -> Option<i32> {
    if args.len() != 4 || args.get(1).map(String::as_str) != Some("--auraw-onnx-runtime-probe") {
        return None;
    }
    let path = std::path::Path::new(&args[2]);
    let sha256 = &args[3];
    match crate::ai_masks::run_runtime_probe_process(path, sha256) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("AuRaw ONNX Runtime probe failed: {error:#}");
            Some(2)
        }
    }
}

#[cfg(not(target_os = "android"))]
fn desktop_icon() -> std::sync::Arc<eframe::egui::IconData> {
    static ICON: std::sync::OnceLock<std::sync::Arc<eframe::egui::IconData>> =
        std::sync::OnceLock::new();
    ICON.get_or_init(|| {
        let image = image::load_from_memory(include_bytes!("../packaging/icons/auraw-256.png"))
            .expect("embedded desktop icon must be a valid PNG")
            .into_rgba8();
        let (width, height) = image.dimensions();
        std::sync::Arc::new(eframe::egui::IconData {
            rgba: image.into_raw(),
            width,
            height,
        })
    })
    .clone()
}

#[cfg(not(target_os = "android"))]
pub fn run_desktop() -> eframe::Result {
    env_logger::init();

    let mut options = native_options();
    options.viewport = eframe::egui::ViewportBuilder::default()
        .with_app_id("de.duecki.auraw")
        .with_inner_size([1280.0, 720.0])
        .with_min_inner_size([480.0, 480.0])
        .with_icon(desktop_icon());

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
    // Keep Android system status/navigation bars visible.
    options.viewport = eframe::egui::ViewportBuilder::default();

    let result = eframe::run_native(
        "AuRaw",
        options,
        Box::new(move |cc| Ok(Box::new(AurawApp::new_android(cc, android_app.clone())))),
    );
    if let Err(error) = result {
        log::error!("AuRaw terminated: {error:#}");
    }
}
