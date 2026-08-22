//! AuRaw's egui/eframe application shell and interactive Develop interface.

pub mod diagnostics {
    pub use auraw_core::diagnostics::*;
}

pub mod file_ops {
    pub use auraw_core::file_ops::*;
}

pub(crate) mod performance_settings;


pub mod thumbnail_cache {
    pub use auraw_core::thumbnail_cache::*;
}

pub mod pipeline {
    pub use auraw_gpu::pipeline::*;
}

pub mod sidecar {
    pub use auraw_core::sidecar::*;
    #[cfg(target_os = "android")]
    pub use auraw_ffi::{load_android, save_android};
}

pub mod ai_denoise {
    pub use auraw_ai::ai_denoise::*;
}

pub mod ai_masks {
    pub use auraw_ai::ai_masks::*;
}

pub mod remove {
    pub use auraw_ai::remove::*;
}


#[cfg(target_os = "android")]
pub mod android {
    pub use auraw_ffi::*;
}

mod app;
mod ui;

pub use app::AurawApp;
pub use auraw_core::SOURCE_REVISION;

fn native_options() -> eframe::NativeOptions {
    let mut options = eframe::NativeOptions {
        renderer: eframe::Renderer::Wgpu,
        ..Default::default()
    };

    #[cfg(target_os = "android")]
    {
        options.wgpu_options.surface = eframe::egui_wgpu::SurfaceConfig::LOW_LATENCY;
    }

    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        // Memory
        setup.instance_descriptor.memory_budget_thresholds = eframe::wgpu::MemoryBudgetThresholds {
            // ONNX execution providers allocate outside wgpu but compete for
            // the same VRAM. Leave substantial headroom so a rejected AI
            // allocation cannot strand egui without enough memory for its next
            // tiny staging upload.
            for_resource_creation: Some(70),
            for_device_loss: Some(97),
        };
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
    match auraw_ai::ai_masks::run_runtime_probe_process(path, sha256) {
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
        let image =
            image::load_from_memory(include_bytes!("../../../packaging/icons/auraw-256.png"))
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
pub fn android_main(android_app: auraw_ffi::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("AuRaw"),
    );

    let mut options = native_options();
    options.android_app = Some(android_app.clone());
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
