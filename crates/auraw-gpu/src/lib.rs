//! AuRaw's wgpu compute pipeline, shader compilation, and GPU export engine.

pub use egui;
pub use egui_wgpu;
pub use wgpu;

/// Fixed LaMa model texture extent used by GPU staging resources.
pub const LAMA_EDGE: u32 = 512;

pub mod diagnostics {
    pub use auraw_core::diagnostics::*;
}

pub mod file_ops {
    pub use auraw_core::file_ops::*;
}

pub mod thumbnail_cache {
    pub use auraw_core::thumbnail_cache::*;
}

#[cfg(target_os = "android")]
pub mod android {
    pub use auraw_ffi::*;
}

pub mod pipeline;
