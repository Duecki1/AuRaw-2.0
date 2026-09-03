pub mod diagnostics {
    pub use calibraw_core::diagnostics::*;
}

pub mod file_ops {
    pub use calibraw_core::file_ops::*;
}

pub(crate) mod export_naming;
pub(crate) mod performance_settings;

pub mod thumbnail_cache {
    pub use calibraw_core::thumbnail_cache::*;
}

pub mod pipeline {
    pub use calibraw_gpu::pipeline::*;
}

pub mod sidecar {
    pub use calibraw_core::sidecar::*;
    #[cfg(target_os = "android")]
    pub use calibraw_ffi::{load_android, save_android};
}

pub mod ai_denoise {
    pub use calibraw_ai::ai_denoise::*;
}

pub mod ai_masks {
    pub use calibraw_ai::ai_masks::*;
}

pub mod remove {
    pub use calibraw_ai::remove::*;
}

#[cfg(target_os = "android")]
pub mod android {
    pub use calibraw_ffi::*;
}

mod app;
mod ui;

pub use app::CalibRawApp;
pub use calibraw_core::SOURCE_REVISION;

/// Memory-budget policy for wgpu's `VK_EXT_memory_budget` resource-creation gate.
///
/// The gate compares the *driver-reported* heap usage/budget against a percentage
/// before every texture/buffer allocation. On desktop drivers (notably
/// NVIDIA/Vulkan on Linux) the reported numbers are unreliable: usage includes all
/// processes plus driver reservations, budgets understate physical VRAM, and wgpu
/// must check every device-local heap because the allocator cannot pin a target
/// heap. That makes the gate fail resource creation while gigabytes of VRAM are
/// physically free, so it is disabled on desktop; genuine physical OOM is still
/// surfaced by the driver itself.
///
/// Android/unified-memory devices benefit from the guard (memory-pressure kills
/// there are expensive), so the default thresholds stay enabled.
///
/// `CALIBRAW_GPU_MEMORY_BUDGET_THRESHOLD=<1-100>` overrides the desktop default for
/// testing; an invalid or empty value falls back to the platform default.
fn memory_budget_thresholds() -> eframe::wgpu::MemoryBudgetThresholds {
    if let Ok(raw) = std::env::var("CALIBRAW_GPU_MEMORY_BUDGET_THRESHOLD") {
        if let Ok(percent) = raw.trim().parse::<u8>() {
            let percent = percent.clamp(1, 100);
            return eframe::wgpu::MemoryBudgetThresholds {
                for_resource_creation: Some(percent),
                for_device_loss: Some(percent.clamp(1, 97)),
            };
        }
    }
    if cfg!(target_os = "android") {
        eframe::wgpu::MemoryBudgetThresholds {
            for_resource_creation: Some(70),
            for_device_loss: Some(97),
        }
    } else {
        eframe::wgpu::MemoryBudgetThresholds {
            for_resource_creation: None,
            for_device_loss: None,
        }
    }
}

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
        setup.instance_descriptor.memory_budget_thresholds = memory_budget_thresholds();
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
                label: Some("CalibRaw wgpu device"),
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
    if args.len() != 4 || args.get(1).map(String::as_str) != Some("--calibraw-onnx-runtime-probe") {
        return None;
    }
    let path = std::path::Path::new(&args[2]);
    let sha256 = &args[3];
    match calibraw_ai::ai_masks::run_runtime_probe_process(path, sha256) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("CalibRaw ONNX Runtime probe failed: {error:#}");
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
            image::load_from_memory(include_bytes!("../../../packaging/icons/calibraw-256.png"))
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
        .with_app_id("de.duecki.calibraw")
        .with_inner_size([1280.0, 720.0])
        .with_min_inner_size([480.0, 480.0])
        .with_icon(desktop_icon());

    eframe::run_native(
        "CalibRaw",
        options,
        Box::new(|cc| Ok(Box::new(CalibRawApp::new(cc)))),
    )
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub fn android_main(android_app: calibraw_ffi::AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag("CalibRaw"),
    );

    let mut options = native_options();
    options.android_app = Some(android_app.clone());
    options.viewport = eframe::egui::ViewportBuilder::default();

    let result = eframe::run_native(
        "CalibRaw",
        options,
        Box::new(move |cc| Ok(Box::new(CalibRawApp::new_android(cc, android_app.clone())))),
    );
    if let Err(error) = result {
        log::error!("CalibRaw terminated: {error:#}");
    }
}
