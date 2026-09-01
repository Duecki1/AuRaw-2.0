use anyhow::{Context, Result};
use std::{
    fs,
    io::ErrorKind,
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::gpu_errors::GpuErrorScopes;

const CALIBRAW_PIPELINE_CACHE_SCHEMA: &str = "wgpu29-v1";

#[derive(Clone)]
pub struct PersistentGpuPipelineCache {
    cache: wgpu::PipelineCache,
    path: Arc<PathBuf>,
}

impl PersistentGpuPipelineCache {
    pub fn load_or_create(
        device: &wgpu::Device,
        adapter_info: &wgpu::AdapterInfo,
        cache_root: &Path,
    ) -> Result<Option<(Arc<Self>, usize)>> {
        if !device.features().contains(wgpu::Features::PIPELINE_CACHE) {
            return Ok(None);
        }
        let Some(adapter_key) = wgpu::util::pipeline_cache_key(adapter_info) else {
            return Ok(None);
        };

        let cache_dir = cache_root.join(format!(
            "wgpu-pipeline-cache-{CALIBRAW_PIPELINE_CACHE_SCHEMA}"
        ));
        let path = cache_dir.join(adapter_key);
        let cache_data = match fs::read(&path) {
            Ok(data) => Some(data),
            Err(error) if error.kind() == ErrorKind::NotFound => None,
            Err(error) => {
                log::warn!(
                    "could not read persisted GPU pipeline cache {}: {error}",
                    path.display()
                );
                None
            }
        };
        let loaded_bytes = cache_data.as_ref().map(Vec::len).unwrap_or(0);

        let gpu_error_scopes = GpuErrorScopes::push(device);
        let cache = unsafe {
            device.create_pipeline_cache(&wgpu::PipelineCacheDescriptor {
                label: Some("CalibRaw persistent GPU pipeline cache"),
                data: cache_data.as_deref(),
                fallback: true,
            })
        };
        gpu_error_scopes.finish("create persistent GPU pipeline cache")?;

        Ok(Some((
            Arc::new(Self {
                cache,
                path: Arc::new(path),
            }),
            loaded_bytes,
        )))
    }

    pub fn raw(&self) -> &wgpu::PipelineCache {
        &self.cache
    }

    pub fn path(&self) -> &Path {
        self.path.as_path()
    }

    pub fn persist(&self) -> Result<usize> {
        let Some(data) = self.cache.get_data() else {
            return Ok(0);
        };
        crate::thumbnail_cache::write_bytes_atomic(self.path.as_path(), &data).with_context(
            || {
                format!(
                    "could not publish GPU pipeline cache {}",
                    self.path.display()
                )
            },
        )?;
        Ok(data.len())
    }
}
