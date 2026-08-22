use super::*;

impl AurawApp {
    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_display_color_management(&mut self, enabled: bool) {
        if self.preferences.display_color_management == enabled {
            return;
        }
        self.preferences.display_color_management = enabled;
        self.preferences.display_profile_last_probe = None;
        self.preferences.display_profile_fingerprint = None;
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn choose_display_profile_override(&mut self) {
        if self.ui.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new().add_filter("ICC profiles", &["icc", "icm"]);
        if let Some(path) = self.preferences.display_profile_override.as_deref() {
            if let Some(parent) = path.parent() {
                dialog = dialog.set_directory(parent);
            }
        }
        self.ui.desktop_picker_receiver = Some(spawn_ui_worker(&self.egui_ctx, move || {
            let path =
                pollster::block_on(dialog.pick_file()).map(|handle| handle.path().to_path_buf());
            crate::app::DesktopPickerEvent::DisplayProfile(path)
        }));
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn apply_display_profile_override(&mut self, path: PathBuf) {
        self.preferences.display_profile_override = Some(path);
        self.preferences.display_profile_last_probe = None;
        self.preferences.display_profile_fingerprint = None;
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn clear_display_profile_override(&mut self) {
        if self.preferences.display_profile_override.take().is_none() {
            return;
        }
        self.preferences.display_profile_last_probe = None;
        self.preferences.display_profile_fingerprint = None;
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn sync_display_color_management(
        &mut self,
        ctx: &egui::Context,
        frame: &eframe::Frame,
    ) {
        use std::hash::{Hash, Hasher};

        let screen_point = ctx.input(|input| {
            let viewport = input.viewport();
            let native_pixels_per_point = viewport
                .native_pixels_per_point
                .unwrap_or_else(|| ctx.pixels_per_point());
            let coordinate_scale = if cfg!(target_os = "macos") {
                1.0
            } else {
                native_pixels_per_point
            };
            viewport.outer_rect.map(|rect| {
                let center = rect.center();
                [
                    (center.x * coordinate_scale).round() as i32,
                    (center.y * coordinate_scale).round() as i32,
                ]
            })
        });
        let screen_changed = match (screen_point, self.preferences.display_profile_last_screen_point) {
            (Some(current), Some(previous)) => current != previous,
            (Some(_), None) | (None, Some(_)) => true,
            (None, None) => false,
        };
        let elapsed = self.preferences.display_profile_last_probe
            .map(|instant| instant.elapsed())
            .unwrap_or(Duration::MAX);
        if elapsed < Duration::from_secs(1)
            || (!screen_changed && elapsed < Duration::from_secs(10))
        {
            return;
        }
        self.preferences.display_profile_last_probe = Some(Instant::now());
        self.preferences.display_profile_last_screen_point = screen_point;

        let resolved = if !self.preferences.display_color_management {
            Ok(None)
        } else if let Some(path) = self.preferences.display_profile_override.as_deref() {
            crate::pipeline::read_display_icc_profile(path).map(Some)
        } else {
            crate::pipeline::discover_display_icc_profile(screen_point)
        };

        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        let (transform, label, source, fingerprint) = match resolved {
            Ok(Some(profile)) => {
                profile.bytes.hash(&mut hasher);
                let fingerprint = hasher.finish();
                let source = Some(profile.source);
                if self.preferences.display_profile_fingerprint == Some(fingerprint)
                    && self.preferences.display_profile_label == profile.label
                    && self.preferences.display_profile_source == source
                {
                    return;
                }
                match crate::pipeline::IccOutputTransform::from_icc(
                    &profile.bytes,
                    crate::pipeline::RenderingIntent::RelativeColorimetric,
                ) {
                    Ok(transform) => (transform, profile.label, source, fingerprint),
                    Err(error) => {
                        log::warn!("display ICC profile could not be built; using sRGB: {error:#}");
                        (
                            crate::pipeline::IccOutputTransform::srgb(),
                            "sRGB fallback".to_owned(),
                            Some(format!("ICC error: {error:#}")),
                            0,
                        )
                    }
                }
            }
            Ok(None) => {
                let label = if self.preferences.display_color_management {
                    "sRGB fallback".to_owned()
                } else {
                    "sRGB (color management disabled)".to_owned()
                };
                if self.preferences.display_profile_fingerprint == Some(0)
                    && self.preferences.display_profile_label == label
                    && self.preferences.display_profile_source.is_none()
                {
                    return;
                }
                (crate::pipeline::IccOutputTransform::srgb(), label, None, 0)
            }
            Err(error) => {
                let source = Some(format!("Profile discovery error: {error:#}"));
                if self.preferences.display_profile_fingerprint == Some(0)
                    && self.preferences.display_profile_label == "sRGB fallback"
                    && self.preferences.display_profile_source == source
                {
                    return;
                }
                log::warn!("display ICC discovery failed; using sRGB: {error:#}");
                (
                    crate::pipeline::IccOutputTransform::srgb(),
                    "sRGB fallback".to_owned(),
                    source,
                    0,
                )
            }
        };

        if self.preferences.display_profile_fingerprint == Some(fingerprint)
            && self.preferences.display_profile_label == label
            && self.preferences.display_profile_source == source
        {
            return;
        }

        let Some(render_state) = frame.wgpu_render_state() else {
            self.preferences.display_output_transform = transform;
            self.preferences.display_profile_label = label;
            self.preferences.display_profile_source = source;
            self.preferences.display_profile_fingerprint = Some(fingerprint);
            return;
        };

        let previous_transform = self.preferences.display_output_transform.clone();
        let mut updates = Vec::new();
        if let Some(pipeline) = self.preview.gpu_pipeline.as_ref() {
            updates.push((
                "main preview",
                pipeline.write_output_transform(&render_state.queue, &transform),
            ));
        }
        if let Some(detail) = self.preview.detail.as_ref() {
            updates.push((
                "detail preview",
                detail
                    .pipeline
                    .write_output_transform(&render_state.queue, &transform),
            ));
        }
        if let Some(navigation) = self.preview.navigation.as_ref() {
            updates.push((
                "navigation preview",
                navigation
                    .pipeline
                    .write_output_transform(&render_state.queue, &transform),
            ));
        }
        if let Err(error) = collect_pipeline_update_results("install display ICC LUT", updates) {
            let mut rollbacks = Vec::new();
            if let Some(pipeline) = self.preview.gpu_pipeline.as_ref() {
                rollbacks.push((
                    "main preview",
                    pipeline.write_output_transform(&render_state.queue, &previous_transform),
                ));
            }
            if let Some(detail) = self.preview.detail.as_ref() {
                rollbacks.push((
                    "detail preview",
                    detail
                        .pipeline
                        .write_output_transform(&render_state.queue, &previous_transform),
                ));
            }
            if let Some(navigation) = self.preview.navigation.as_ref() {
                rollbacks.push((
                    "navigation preview",
                    navigation
                        .pipeline
                        .write_output_transform(&render_state.queue, &previous_transform),
                ));
            }
            let rollback = collect_pipeline_update_results("restore display ICC LUT", rollbacks);
            self.preview.pending_stage = Some(ProcessingStage::Output);
            self.ui.notice = Some(
                "Could not update every preview color profile. The previous display transform remains active."
                    .to_owned(),
            );
            crate::diagnostics::record(format!(
                "transactional display-profile update failed: {error:#}; rollback={rollback:#?}"
            ));
            return;
        }

        self.preferences.display_output_transform = transform;
        self.preferences.display_profile_label = label;
        self.preferences.display_profile_source = source;
        self.preferences.display_profile_fingerprint = Some(fingerprint);
        if self.preview.gpu_pipeline.is_some() {
            self.queue_preview_processing(ProcessingStage::Output);
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn apply_display_output_transform(
        &self,
        queue: &wgpu::Queue,
        pipeline: &RawGpuPipeline,
    ) -> anyhow::Result<()> {
        pipeline
            .write_output_transform(queue, &self.preferences.display_output_transform)
            .map_err(|error| {
                anyhow::anyhow!("preview pipeline: install display ICC LUT: {error:#}")
            })
    }
}
