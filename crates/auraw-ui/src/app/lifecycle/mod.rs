use super::*;

pub(super) fn remove_temporary_raw(path: &std::path::Path) {
    if let Err(error) = std::fs::remove_file(path) {
        log::warn!(
            "could not remove imported Android RAW cache file {}: {error}",
            path.display()
        );
    }
}

pub(super) fn load_sidecar_for_target(
    target: &crate::sidecar::SidecarTarget,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> Result<Option<crate::sidecar::LoadedSidecar>, crate::sidecar::SidecarError> {
    match target {
        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
            crate::sidecar::load_desktop(raw_path)
        }
        #[cfg(target_os = "android")]
        crate::sidecar::SidecarTarget::Android {
            raw_uri,
            display_name,
        } => crate::sidecar::load_android(android_app, raw_uri, display_name),
    }
}

pub(super) fn raw_cache_key_for_target(target: &crate::sidecar::SidecarTarget) -> String {
    match target {
        crate::sidecar::SidecarTarget::Desktop { raw_path } => {
            let metadata = std::fs::metadata(raw_path).ok();
            let bytes = metadata
                .as_ref()
                .map(std::fs::Metadata::len)
                .unwrap_or_default();
            let modified = metadata
                .and_then(|metadata| metadata.modified().ok())
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|duration| duration.as_nanos())
                .unwrap_or_default();
            format!("desktop:{}:{bytes}:{modified}", raw_path.display())
        }
        #[cfg(target_os = "android")]
        crate::sidecar::SidecarTarget::Android { raw_uri, .. } => {
            format!("android:{raw_uri}")
        }
    }
}

pub(super) fn prewarm_dcp_profile_folder(folder: Option<std::path::PathBuf>) {
    let Some(folder) = folder.filter(|folder| folder.is_dir()) else {
        return;
    };
    let _ = std::thread::Builder::new()
        .name("auraw-dcp-prewarm".to_owned())
        .spawn(move || {
            let started = Instant::now();
            crate::pipeline::prewarm_dcp_profile_index(&folder);
            crate::diagnostics::record(format!(
                "DCP profile index prewarmed in {:.3}s",
                started.elapsed().as_secs_f64()
            ));
        });
}

#[cfg(not(target_os = "android"))]
pub(super) fn selected_picker_directory(path: &std::path::Path) -> Option<std::path::PathBuf> {
    if path.is_dir() {
        Some(path.to_path_buf())
    } else {
        path.parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(std::path::Path::to_path_buf)
    }
}

pub(super) fn gpu_preview_prewarm_cfa_kind() -> crate::pipeline::CfaKind {
    crate::pipeline::CfaKind::Bayer
}

pub(super) fn spawn_gpu_preview_prewarm(
    cc: &eframe::CreationContext<'_>,
    cache_root: Option<std::path::PathBuf>,
    export_prewarm: Arc<crate::pipeline::GpuProgramPrewarm>,
) -> Option<mpsc::Receiver<Result<RawGpuPipeline, String>>> {
    let Some(render_state) = cc.wgpu_render_state.as_ref() else {
        export_prewarm.publish(Err("eframe is not running with the wgpu backend".to_owned()));
        return None;
    };
    let device = render_state.device.clone();
    let queue = render_state.queue.clone();
    let adapter_info = render_state.adapter.get_info();
    let repaint = cc.egui_ctx.clone();
    let (sender, receiver) = mpsc::channel();
    let export_prewarm_for_thread = Arc::clone(&export_prewarm);
    let spawn_result = std::thread::Builder::new()
        .name("auraw-gpu-preview-prewarm".to_owned())
        .spawn(move || {
            let started = Instant::now();
            crate::diagnostics::record("GPU preview prewarm started at app initialization");

            let persistent_cache = match cache_root.as_deref() {
                Some(cache_root) => {
                    match crate::pipeline::PersistentGpuPipelineCache::load_or_create(
                        &device,
                        &adapter_info,
                        cache_root,
                    ) {
                        Ok(Some((cache, loaded_bytes))) => {
                            if loaded_bytes == 0 {
                                crate::diagnostics::record(format!(
                                    "GPU pipeline cache cold start: {}",
                                    cache.path().display()
                                ));
                            } else {
                                crate::diagnostics::record(format!(
                                    "GPU pipeline cache loaded: {} bytes from {}",
                                    loaded_bytes,
                                    cache.path().display()
                                ));
                            }
                            Some(cache)
                        }
                        Ok(None) => {
                            crate::diagnostics::record(
                                "GPU pipeline cache unavailable on this wgpu device/backend",
                            );
                            None
                        }
                        Err(error) => {
                            crate::diagnostics::record(format!(
                                "GPU pipeline cache could not be initialized: {error:#}"
                            ));
                            None
                        }
                    }
                }
                None => {
                    crate::diagnostics::record(
                        "GPU pipeline cache path unavailable; using in-process prewarm only",
                    );
                    None
                }
            };

            let cache_to_persist = persistent_cache.clone();
            let result = RawGpuPipeline::prewarm_preview_template_with_cache(
                &device,
                &queue,
                gpu_preview_prewarm_cfa_kind(),
                persistent_cache.clone(),
            )
            .map_err(|error| format!("GPU preview prewarm failed: {error:#}"));
            match &result {
                Ok(_) => crate::diagnostics::record(format!(
                    "GPU preview prewarm finished in {:.3}s",
                    started.elapsed().as_secs_f64()
                )),
                Err(error) => crate::diagnostics::record(error),
            }

            // Deliver the compiled template immediately. Cache serialization is
            // intentionally done afterwards so a RAW open waiting on prewarm
            // never pays filesystem write latency.
            let _ = sender.send(result);
            repaint.request_repaint();

            let export_started = Instant::now();
            let export_result = RawGpuPipeline::prewarm_export_program_template_with_cache(
                &device,
                &queue,
                gpu_preview_prewarm_cfa_kind(),
                persistent_cache,
            )
            .map_err(|error| format!("GPU export program prewarm failed: {error:#}"));
            match &export_result {
                Ok(_) => crate::diagnostics::record(format!(
                    "GPU export program prewarm finished in {:.3}s",
                    export_started.elapsed().as_secs_f64()
                )),
                Err(error) => crate::diagnostics::record(error),
            }
            export_prewarm_for_thread.publish(export_result);
            repaint.request_repaint();

            if let Some(cache) = cache_to_persist {
                let cache_save_started = Instant::now();
                match cache.persist() {
                    Ok(bytes) if bytes > 0 => crate::diagnostics::record(format!(
                        "GPU pipeline cache saved: {} bytes in {:.3}s to {}",
                        bytes,
                        cache_save_started.elapsed().as_secs_f64(),
                        cache.path().display()
                    )),
                    Ok(_) => crate::diagnostics::record(format!(
                        "GPU pipeline cache returned no persistent data for {}",
                        cache.path().display()
                    )),
                    Err(error) => crate::diagnostics::record(format!(
                        "GPU pipeline cache could not be saved: {error:#}"
                    )),
                }
            }
        });
    match spawn_result {
        Ok(_) => Some(receiver),
        Err(error) => {
            export_prewarm.publish(Err(format!("GPU prewarm thread could not start: {error}")));
            crate::diagnostics::record(format!(
                "GPU preview prewarm thread could not start: {error}"
            ));
            None
        }
    }
}

pub(super) fn append_notice(notice: &mut Option<String>, message: &str) {
    match notice {
        Some(existing) => {
            existing.push(' ');
            existing.push_str(message);
        }
        None => *notice = Some(message.to_owned()),
    }
}

pub(super) fn needs_canonical_mask_source(masks: &MaskStack) -> bool {
    masks.masks.iter().any(|mask| {
        mask.components.iter().any(|component| {
            matches!(
                &component.geometry,
                MaskGeometry::LuminanceRange { source: None, .. }
                    | MaskGeometry::ColorRange { source: None, .. }
                    | MaskGeometry::Object { .. }
                    | MaskGeometry::Landscape { .. }
            )
        })
    })
}

pub(super) fn install_missing_range_sources(masks: &mut MaskStack, source: &MaskRgbImage) {
    for mask in &mut masks.masks {
        for component in &mut mask.components {
            match &mut component.geometry {
                MaskGeometry::LuminanceRange { source: target, .. }
                | MaskGeometry::ColorRange { source: target, .. }
                    if target.is_none() =>
                {
                    *target = Some(source.clone());
                }
                _ => {}
            }
        }
    }
}

mod cache;
mod display;
mod documents;
mod pickers;
mod profiles;
mod settings;
mod startup;

#[cfg(test)]
mod tests;
