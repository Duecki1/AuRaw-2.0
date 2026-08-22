use anyhow::{anyhow, Result};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

struct OomSignal(AtomicBool);

impl OomSignal {
    const fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    fn raise(&self) {
        self.0.store(true, Ordering::Release);
    }

    fn take(&self) -> bool {
        self.0.swap(false, Ordering::AcqRel)
    }
}

static GPU_OUT_OF_MEMORY: OomSignal = OomSignal::new();

pub fn install_uncaptured_gpu_error_handler(device: &wgpu::Device) {
    device.on_uncaptured_error(Arc::new(|error| {
        record_gpu_error(&error, "uncaptured GPU operation");
    }));
}

pub fn take_gpu_out_of_memory() -> bool {
    GPU_OUT_OF_MEMORY.take()
}

fn record_gpu_error(error: &wgpu::Error, context: &str) {
    let message = match error {
        wgpu::Error::OutOfMemory { .. } => {
            GPU_OUT_OF_MEMORY.raise();
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
        let signal = OomSignal::new();
        assert!(!signal.take());
        signal.raise();
        assert!(signal.take());
        assert!(!signal.take());
    }
}
