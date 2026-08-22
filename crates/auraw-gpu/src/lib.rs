//! AuRaw's wgpu compute pipeline, shader compilation, and GPU export engine.

pub use egui;
pub use egui_wgpu;
pub use wgpu;

pub mod diagnostics {
    pub use auraw_core::diagnostics::*;
}

pub mod file_ops {
    pub use auraw_core::file_ops::*;
}

pub mod thumbnail_cache {
    pub use auraw_core::thumbnail_cache::*;
}

mod gpu_errors;
pub use gpu_errors::{install_uncaptured_gpu_error_handler, take_gpu_out_of_memory};

#[cfg(target_os = "android")]
pub mod android {
    pub use auraw_ffi::*;
}

pub mod pipeline;
