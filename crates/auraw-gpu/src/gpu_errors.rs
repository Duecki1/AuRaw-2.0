use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

static GPU_OUT_OF_MEMORY: AtomicBool = AtomicBool::new(false);

/// Replaces wgpu's default panic-on-error callback for an application device.
///
/// Fallible AuRaw allocations use error scopes and return their failures to the
/// caller. This handler is the last line of defence for allocations owned by
/// egui, a backend, or another path that cannot be scoped by AuRaw itself.
pub fn install_uncaptured_gpu_error_handler(device: &wgpu::Device) {
    device.on_uncaptured_error(Arc::new(|error| {
        record_gpu_error(&error, "uncaptured GPU operation");
    }));
}

/// Returns whether an unhandled or scoped GPU allocation has exhausted memory
/// since the previous call. The UI uses this to release optional resources and
/// tell the user why the requested operation was skipped.
pub fn take_gpu_out_of_memory() -> bool {
    GPU_OUT_OF_MEMORY.swap(false, Ordering::AcqRel)
}

fn record_gpu_error(error: &wgpu::Error, context: &str) {
    let message = match error {
        wgpu::Error::OutOfMemory { .. } => {
            GPU_OUT_OF_MEMORY.store(true, Ordering::Release);
            format!("{context} ran out of GPU memory; the operation was cancelled")
        }
        wgpu::Error::Validation { description, .. } => {
            format!("{context} failed GPU validation: {description}")
        }
        wgpu::Error::Internal { description, .. } => {
            format!("{context} hit an internal GPU error: {description}")
        }
    };
    log::error!("{message}");
    crate::diagnostics::record(message);
}

/// Captures all errors produced by one synchronous GPU allocation sequence.
/// wgpu resource constructors are intentionally infallible at the type level;
/// without these scopes a real driver allocation failure reaches the global
/// handler only after the constructor has returned an unusable resource.
pub(crate) struct GpuErrorScopes {
    validation: wgpu::ErrorScopeGuard,
    internal: wgpu::ErrorScopeGuard,
    out_of_memory: wgpu::ErrorScopeGuard,
}

impl GpuErrorScopes {
    pub(crate) fn push(device: &wgpu::Device) -> Self {
        let out_of_memory = device.push_error_scope(wgpu::ErrorFilter::OutOfMemory);
        let internal = device.push_error_scope(wgpu::ErrorFilter::Internal);
        let validation = device.push_error_scope(wgpu::ErrorFilter::Validation);
        Self {
            validation,
            internal,
            out_of_memory,
        }
    }

    pub(crate) fn finish(self, context: &'static str) -> Result<()> {
        // Error scopes are a stack and therefore must be popped in reverse
        // order. wgpu-core resolves these futures immediately; WebGPU resolves
        // them through its normal promise machinery.
        let validation = pollster::block_on(self.validation.pop());
        let internal = pollster::block_on(self.internal.pop());
        let out_of_memory = pollster::block_on(self.out_of_memory.pop());
        let error = out_of_memory.or(internal).or(validation);
        if let Some(error) = error {
            record_gpu_error(&error, context);
            return Err(anyhow!("{context} failed: {error}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_memory_signal_is_edge_triggered() {
        GPU_OUT_OF_MEMORY.store(true, Ordering::Release);
        assert!(take_gpu_out_of_memory());
        assert!(!take_gpu_out_of_memory());
    }
}
