//! AuRaw ONNX Runtime bindings and model-specific inference pipelines.

pub mod diagnostics {
    pub use auraw_core::diagnostics::*;
}

pub mod file_ops {
    pub use auraw_core::file_ops::*;
}

pub mod pipeline {
    pub use auraw_gpu::pipeline::*;
}

pub mod ai_denoise;
pub mod ai_masks;
pub mod execution_provider;
mod model_artifact;

#[cfg(not(target_os = "android"))]
pub use model_artifact::desktop_model_cache_root;

#[cfg(not(target_os = "android"))]
pub use execution_provider::set_ai_acceleration_enabled;
pub use execution_provider::{
    active_execution_providers, ai_acceleration_enabled, create_session_with_fallback,
    CpuFallbackProfile, ExecutionProviderStatus, FallbackSession, ModelSource, SessionOptions,
};
