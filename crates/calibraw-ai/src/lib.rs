pub mod diagnostics {
    pub use calibraw_core::diagnostics::*;
}

pub mod file_ops {
    pub use calibraw_core::file_ops::*;
}

pub mod pipeline {
    pub use calibraw_gpu::pipeline::*;
}

pub mod ai_denoise;
pub mod ai_masks;
pub mod execution_provider;
mod model_artifact;
mod model_install;
mod model_runtime;
#[cfg(not(target_os = "android"))]
mod onnx_runtime_artifact;
pub mod remove;
pub use model_install::ModelDownloadProgress;
pub use model_runtime::{set_active_ai_context, AiRuntimeContext};

#[cfg(not(target_os = "android"))]
pub use model_artifact::desktop_model_cache_root;
#[cfg(not(target_os = "android"))]
pub use onnx_runtime_artifact::{
    automatic_onnx_runtime_info, automatic_onnx_runtime_is_installed,
    ensure_automatic_onnx_runtime, AutomaticOnnxRuntimeInfo,
};

#[cfg(not(target_os = "android"))]
pub use execution_provider::set_ai_acceleration_enabled;
pub use execution_provider::{
    active_execution_providers, ai_acceleration_enabled, create_session_with_fallback,
    take_ai_gpu_memory_failure, CpuFallbackProfile, ExecutionProviderStatus, FallbackSession,
    ModelSource, SessionOptions,
};
