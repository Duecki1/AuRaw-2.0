impl AurawApp {
    pub(crate) fn mark_mask_adjustments_dirty(&mut self) {
        self.note_mask_edit_changed();
        if self.gpu_pipeline.is_none() {
            return;
        }
        self.queue_preview_processing(ProcessingStage::Output);
    }

    pub(crate) fn mark_mask_geometry_dirty(&mut self, layer: usize) {
        if layer < MAX_LOCAL_MASKS {
            self.dirty_mask_layers[layer] = true;
            self.detail_dirty_mask_layers[layer] = true;
            self.navigation_dirty_mask_layers[layer] = true;
        }
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mark_mask_adjustments_dirty();
    }

    /// Interactive brush and geometry edits can otherwise trigger a full
    /// high-resolution atlas rasterization every display frame. Refresh at a
    /// steady interactive cadence independent of monitor refresh rate, then
    /// always commit the exact newest geometry when the pointer is released.
    pub(crate) fn note_mask_geometry_interaction(&mut self, layer: usize) {
        const INTERACTIVE_MASK_INTERVAL: Duration = Duration::from_millis(45);

        if self.mask_interaction_dirty_layer != Some(layer) {
            self.finish_mask_geometry_interaction();
            self.mask_interaction_dirty_layer = Some(layer);
            self.mask_interaction_last_upload = None;
        }

        self.mask_interaction_has_uncommitted_change = true;
        let now = Instant::now();
        let upload_due = self
            .mask_interaction_last_upload
            .is_none_or(|last| now.duration_since(last) >= INTERACTIVE_MASK_INTERVAL);
        if upload_due {
            self.mark_mask_geometry_dirty(layer);
            self.mask_interaction_last_upload = Some(now);
            self.mask_interaction_has_uncommitted_change = false;
        }
    }

    pub(crate) fn finish_mask_geometry_interaction(&mut self) {
        let layer = self.mask_interaction_dirty_layer.take();
        let should_commit = self.mask_interaction_has_uncommitted_change;
        self.mask_interaction_last_upload = None;
        self.mask_interaction_has_uncommitted_change = false;
        if should_commit {
            if let Some(layer) = layer {
                self.mark_mask_geometry_dirty(layer);
            }
        }
    }

    pub(crate) fn begin_mask_touch_gesture(&mut self, mask_index: usize, component_index: usize) {
        if self.mask_touch_gesture_backup.is_some() {
            return;
        }
        let Some(geometry) = self
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .map(|component| component.geometry.clone())
        else {
            return;
        };
        self.mask_touch_gesture_backup = Some(MaskTouchGestureBackup {
            mask_index,
            component_index,
            geometry,
            object_cache: self.object_cache.clone(),
        });
    }

    pub(crate) fn commit_mask_touch_gesture(&mut self) {
        self.mask_touch_gesture_backup = None;
    }

    /// If a second finger joins a mask stroke, the gesture is viewport
    /// navigation. Restore the exact pre-touch geometry so the first finger's
    /// initial dab, color sample, or object-mask reset never leaks into the
    /// image while pinch zooming.
    pub(crate) fn cancel_mask_touch_gesture(&mut self) {
        let Some(backup) = self.mask_touch_gesture_backup.take() else {
            self.last_brush_point = None;
            self.mask_drag = None;
            return;
        };
        let restored = self
            .masks
            .masks
            .get_mut(backup.mask_index)
            .and_then(|mask| mask.components.get_mut(backup.component_index))
            .is_some_and(|component| {
                component.geometry = backup.geometry;
                true
            });
        self.object_cache = backup.object_cache;
        self.object_generation = self.object_generation.wrapping_add(1);
        self.last_brush_point = None;
        self.mask_drag = None;
        self.mask_interaction_dirty_layer = None;
        self.mask_interaction_last_upload = None;
        self.mask_interaction_has_uncommitted_change = false;
        if restored {
            self.mark_mask_geometry_dirty(backup.mask_index);
        }
    }

    pub(crate) fn mark_all_mask_layers_dirty(&mut self) {
        self.dirty_mask_layers.fill(true);
        self.detail_dirty_mask_layers.fill(true);
        self.navigation_dirty_mask_layers.fill(true);
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mark_mask_adjustments_dirty();
    }

    pub(crate) fn activate_mask_tool(&mut self, kind: MaskKind) {
        self.finish_mask_geometry_interaction();
        self.active_mask_tool = kind.is_available().then_some(kind);
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_touch_gesture_backup = None;
        if matches!(kind, MaskKind::Brush | MaskKind::Object) {
            self.brush_mode = BrushMode::Paint;
        }
    }

    pub(crate) fn select_mask_tool(&mut self, kind: MaskKind) {
        self.finish_mask_geometry_interaction();
        self.active_mask_tool = kind.is_available().then_some(kind);
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_touch_gesture_backup = None;
    }

    pub(crate) fn blink_selected_mask(&mut self) {
        self.mask_overlay_blink = Some((std::time::Instant::now(), MaskOverlayBlink::GroupTwice));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn blink_selected_component(&mut self) {
        self.mask_overlay_blink = Some((
            std::time::Instant::now(),
            MaskOverlayBlink::ComponentThenGroup,
        ));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn capture_mask_source(&mut self, frame: &eframe::Frame) -> Result<(), String> {
        if self.mask_source_cache.is_some() {
            return Ok(());
        }
        let render_state = frame
            .wgpu_render_state()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let program_template = self
            .gpu_pipeline
            .as_ref()
            .ok_or_else(|| "Open an image before creating this mask.".to_owned())?;
        let full_raw = self
            .loaded_raw
            .as_ref()
            .ok_or_else(|| "The original RAW is not available.".to_owned())?;
        let source_edge = if cfg!(target_os = "android") {
            1600
        } else {
            2048
        };
        let raw = if full_raw.width.max(full_raw.height) <= source_edge {
            Arc::clone(full_raw)
        } else {
            Arc::new(build_proxy(
                full_raw,
                ProxySpec {
                    max_edge: source_edge,
                },
            ))
        };

        // Subject, Object, and range classifiers must be stable when the user changes
        // Exposure, Color, Effects, curves, grading, or local masks. Render a
        // fresh canonical rendition from the unedited RAW proxy instead of
        // reading the live edited output texture. Camera white balance,
        // profile color, demosaic, and lens-corrected geometry are retained so
        // the model sees a natural image that remains pixel-aligned with the
        // preview and export. A dedicated 1600/2048-edge proxy keeps model
        // quality independent of the interactive Preview Quality setting.
        let reference_exposure = ExposureParams::scene_referred_default();
        let reference_masks = MaskStack::default();
        let params = GpuParams::new(&reference_exposure, &reference_masks, &raw);
        let reference_pipeline = RawGpuPipeline::new_headless_reusing_programs(
            &render_state.device,
            &render_state.queue,
            &raw,
            &params,
            ProcessingQuality::Preview,
            program_template,
        )
        .map_err(|error| format!("Could not prepare the original RAW for masking: {error:#}"))?;
        reference_pipeline.recompute(&render_state.queue, &render_state.device, &params);
        let rgba = reference_pipeline
            .read_output_region_blocking(
                &render_state.device,
                &render_state.queue,
                0,
                0,
                reference_pipeline.width,
                reference_pipeline.height,
            )
            .map_err(|error| format!("Could not read the original RAW for masking: {error:#}"))?;
        self.mask_source_cache =
            MaskRgbImage::new(reference_pipeline.width, reference_pipeline.height, rgba);
        Ok(())
    }

    pub(crate) fn request_subject_mask(&mut self, frame: &eframe::Frame) {
        if let Some(mask) = self.subject_mask_cache.clone() {
            self.apply_subject_mask(mask);
            return;
        }
        #[cfg(not(target_os = "android"))]
        if self.onnx_runtime_path.is_none() || self.onnx_runtime_sha256.is_none() {
            self.notice = Some(
                "Choose an ONNX Runtime library under Settings before using Subject or Not Subject masks."
                    .to_owned(),
            );
            return;
        }
        if let Err(error) = self.capture_mask_source(frame) {
            self.notice = Some(error);
            return;
        }
        let path = self.birefnet_model_path();
        if path.exists() {
            self.start_subject_worker(path);
        } else {
            self.subject_consent_open = true;
        }
    }

    fn start_subject_worker(&mut self, model_path: PathBuf) {
        if self.subject_receiver.is_some() {
            return;
        }
        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The preview could not be prepared for subject selection.".to_owned());
            return;
        };
        self.subject_download_progress = None;
        self.subject_inferencing = model_path.exists();
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        self.subject_receiver = Some(spawn_subject_mask(
            model_path,
            runtime_path,
            runtime_sha256,
            source.width,
            source.height,
            source.rgba.to_vec(),
        ));
        self.egui_ctx.request_repaint();
    }

    fn apply_subject_mask(&mut self, mask: MaskImage) {
        self.subject_mask_cache = Some(mask.clone());
        for local_mask in &mut self.masks.masks {
            for component in &mut local_mask.components {
                if matches!(component.kind, MaskKind::Subject | MaskKind::Background) {
                    if let crate::pipeline::MaskGeometry::Ai { mask: target, .. } =
                        &mut component.geometry
                    {
                        *target = Some(mask.clone());
                    }
                }
            }
        }
        self.mark_all_mask_layers_dirty();
        self.blink_selected_mask();
    }

    fn poll_subject_worker(&mut self) {
        let mut finished = None;
        if let Some(receiver) = &self.subject_receiver {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    SubjectMaskEvent::DownloadProgress {
                        label,
                        downloaded,
                        total,
                    } => {
                        self.subject_download_progress = Some((label, downloaded, total));
                        self.subject_inferencing = false;
                    }
                    SubjectMaskEvent::Inferencing => {
                        self.subject_download_progress = None;
                        self.subject_inferencing = true;
                    }
                    SubjectMaskEvent::Finished(result) => finished = Some(result),
                }
            }
        }
        if let Some(result) = finished {
            self.subject_receiver = None;
            self.subject_download_progress = None;
            self.subject_inferencing = false;
            match result {
                Ok(result) => {
                    if let Some(mask) = MaskImage::new(result.width, result.height, result.mask) {
                        self.apply_subject_mask(mask);
                    }
                }
                Err(error) => self.notice = Some(format!("Subject selection failed: {error}")),
            }
        }
    }

    /// A completed object mask is intentionally immutable from the canvas.
    /// Starting another stroke on the same component replaces it from scratch
    /// instead of treating the stroke as a correction to the previous SAM run.
    pub(crate) fn restart_refined_object_mask_for_stroke(
        &mut self,
        mask_index: usize,
        component_index: usize,
    ) -> bool {
        let target = (mask_index, component_index);
        let cleared = self
            .masks
            .masks
            .get_mut(mask_index)
            .and_then(|mask| mask.components.get_mut(component_index))
            .is_some_and(|component| {
                let crate::pipeline::MaskGeometry::Object { mask, strokes, .. } =
                    &mut component.geometry
                else {
                    return false;
                };
                if mask.is_none() {
                    return false;
                }
                *mask = None;
                strokes.clear();
                true
            });
        if !cleared {
            return false;
        }

        // Any in-flight result or cached logits belong to the replaced mask.
        // Incrementing the generation makes that result stale without trying
        // to interrupt ONNX Runtime halfway through a session run.
        self.object_generation = self.object_generation.wrapping_add(1);
        if self.object_pending_target == Some(target) {
            self.object_pending_target = None;
        }
        if self
            .object_cache
            .as_ref()
            .is_some_and(|(cached_target, _)| *cached_target == target)
        {
            self.object_cache = None;
        }
        self.mask_overlay_blink = None;
        self.brush_mode = BrushMode::Paint;
        true
    }

    pub(crate) fn request_object_mask(&mut self, mask_index: usize, component_index: usize) {
        #[cfg(not(target_os = "android"))]
        if self.onnx_runtime_path.is_none() || self.onnx_runtime_sha256.is_none() {
            self.notice = Some(
                "Choose an ONNX Runtime library under Settings before using Object masks."
                    .to_owned(),
            );
            return;
        }
        let Some(component) = self
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
        else {
            return;
        };
        let crate::pipeline::MaskGeometry::Object { strokes, .. } = &component.geometry else {
            return;
        };
        if !strokes
            .iter()
            .any(|stroke| stroke.positive && !stroke.points.is_empty())
        {
            self.notice = Some("Paint inside an object before generating its mask.".to_owned());
            return;
        }
        if self.mask_source_cache.is_none() {
            self.notice = Some(
                "The original image source is not ready for object selection. Re-open the Object mask or create it again."
                    .to_owned(),
            );
            return;
        }
        let (encoder, decoder) = self.sam21_model_paths();
        if encoder.exists() && decoder.exists() {
            self.start_object_worker(mask_index, component_index, encoder, decoder);
        } else {
            self.object_pending_target = Some((mask_index, component_index));
            self.object_consent_open = true;
        }
    }

    fn start_object_worker(
        &mut self,
        mask_index: usize,
        component_index: usize,
        encoder_path: PathBuf,
        decoder_path: PathBuf,
    ) {
        if self.object_receiver.is_some() {
            // Preserve the newest user intent; it will be submitted after the
            // current generation finishes rather than racing two ORT sessions.
            self.object_pending_target = Some((mask_index, component_index));
            self.object_generation = self.object_generation.wrapping_add(1);
            return;
        }
        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The original image source is unavailable for object selection.".to_owned());
            return;
        };
        let (strokes, brush_size, edge_refine, detailed_edges) = {
            let Some(component) = self
                .masks
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(component_index))
            else {
                return;
            };
            let crate::pipeline::MaskGeometry::Object {
                strokes,
                brush_size,
                edge_refine,
                detailed_edges,
                ..
            } = &component.geometry
            else {
                return;
            };
            (strokes.clone(), *brush_size, *edge_refine, *detailed_edges)
        };
        let cache = self
            .object_cache
            .as_ref()
            .filter(|(target, _)| *target == (mask_index, component_index))
            .map(|(_, cache)| cache.clone());
        let request = ObjectMaskRequest {
            source_width: source.width,
            source_height: source.height,
            source_rgba: source.rgba.to_vec(),
            strokes,
            brush_size,
            edge_refine,
            detailed_edges,
            cache,
        };
        self.object_generation = self.object_generation.wrapping_add(1);
        self.object_job_generation = self.object_generation;
        self.object_job_target = Some((mask_index, component_index));
        self.object_pending_target = None;
        self.object_download_progress = None;
        self.object_inferencing = encoder_path.exists() && decoder_path.exists();
        self.object_decoder_only = request.cache.is_some();
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        self.object_receiver = Some(spawn_object_mask(
            encoder_path,
            decoder_path,
            runtime_path,
            runtime_sha256,
            request,
        ));
        self.egui_ctx.request_repaint();
    }

    fn poll_object_worker(&mut self) {
        let mut finished = None;
        if let Some(receiver) = &self.object_receiver {
            while let Ok(event) = receiver.try_recv() {
                match event {
                    ObjectMaskEvent::DownloadProgress {
                        label,
                        downloaded,
                        total,
                    } => {
                        self.object_download_progress = Some((label, downloaded, total));
                        self.object_inferencing = false;
                    }
                    ObjectMaskEvent::Inferencing { decoder_only } => {
                        self.object_download_progress = None;
                        self.object_inferencing = true;
                        self.object_decoder_only = decoder_only;
                    }
                    ObjectMaskEvent::Finished(result) => finished = Some(result),
                }
            }
        }
        let Some(result) = finished else {
            return;
        };
        self.object_receiver = None;
        self.object_download_progress = None;
        self.object_inferencing = false;
        let target = self.object_job_target.take();
        let generation = self.object_job_generation;
        if generation == self.object_generation {
            match (target, result) {
                (Some((mask_index, component_index)), Ok(result)) => {
                    let crate::ai_masks::ObjectMaskResult {
                        width,
                        height,
                        mask: pixels,
                        cache,
                    } = result;
                    let mask = MaskImage::new(width, height, pixels);
                    let applied = if let (Some(mask), Some(component)) = (
                        mask,
                        self.masks
                            .masks
                            .get_mut(mask_index)
                            .and_then(|local| local.components.get_mut(component_index)),
                    ) {
                        if let crate::pipeline::MaskGeometry::Object {
                            mask: generated_mask,
                            ..
                        } = &mut component.geometry
                        {
                            *generated_mask = Some(mask);
                            true
                        } else {
                            false
                        }
                    } else {
                        false
                    };
                    if applied {
                        self.object_cache = Some(((mask_index, component_index), cache));
                        self.mark_mask_geometry_dirty(mask_index);
                        self.blink_selected_component();
                    }
                }
                (_, Ok(_)) => {}
                (_, Err(error)) => {
                    self.notice = Some(format!("Object selection failed: {error}"));
                }
            }
        }
        if let Some((mask_index, component_index)) = self.object_pending_target.take() {
            let (encoder, decoder) = self.sam21_model_paths();
            self.start_object_worker(mask_index, component_index, encoder, decoder);
        }
    }

    #[cfg(not(target_os = "android"))]
    fn sam21_model_paths(&self) -> (PathBuf, PathBuf) {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir)
            .join("auraw/models");
        (
            root.join("sam2.1-hiera-tiny.encoder.onnx"),
            root.join("sam2.1-hiera-tiny.decoder.onnx"),
        )
    }

    #[cfg(target_os = "android")]
    fn sam21_model_paths(&self) -> (PathBuf, PathBuf) {
        let root = self
            .android_app
            .internal_data_path()
            .unwrap_or_else(std::env::temp_dir)
            .join("models");
        (
            root.join("sam2.1-hiera-tiny.encoder.onnx"),
            root.join("sam2.1-hiera-tiny.decoder.onnx"),
        )
    }

    #[cfg(not(target_os = "android"))]
    fn birefnet_model_path(&self) -> PathBuf {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/models/birefnet-general-lite.onnx")
    }

    #[cfg(not(target_os = "android"))]
    fn onnx_runtime_config_path() -> PathBuf {
        let root = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/onnx-runtime-path")
    }

    #[cfg(not(target_os = "android"))]
    fn load_onnx_runtime_selection() -> Option<(PathBuf, String)> {
        let configured = std::fs::read_to_string(Self::onnx_runtime_config_path()).ok()?;
        let mut lines = configured.lines();
        let sha256 = lines.next()?.strip_prefix("sha256=")?.to_owned();
        let path = PathBuf::from(lines.next()?.strip_prefix("path=")?);
        if lines.next().is_some()
            || sha256.len() != 64
            || !sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            || !path.is_file()
        {
            return None;
        }
        Some((path, sha256))
    }

    #[cfg(not(target_os = "android"))]
    fn persist_onnx_runtime_selection(
        selection: Option<(&std::path::Path, &str)>,
    ) -> Result<(), String> {
        let config = Self::onnx_runtime_config_path();
        if let Some((path, sha256)) = selection {
            let parent = config
                .parent()
                .ok_or_else(|| "invalid AuRaw configuration path".to_owned())?;
            let path_text = path
                .to_str()
                .ok_or_else(|| "the ONNX Runtime path is not valid UTF-8".to_owned())?;
            if path_text.contains('\n') || path_text.contains('\r') {
                return Err("the ONNX Runtime path contains a line break".to_owned());
            }
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("could not create {}: {error}", parent.display()))?;
            let temporary = config.with_extension(format!("tmp.{}", std::process::id()));
            let payload = format!("sha256={sha256}\npath={path_text}\n");
            std::fs::write(&temporary, payload.as_bytes())
                .map_err(|error| format!("could not write {}: {error}", temporary.display()))?;
            #[cfg(windows)]
            if config.exists() {
                std::fs::remove_file(&config)
                    .map_err(|error| format!("could not replace {}: {error}", config.display()))?;
            }
            std::fs::rename(&temporary, &config)
                .map_err(|error| format!("could not publish {}: {error}", config.display()))?;
        } else if let Err(error) = std::fs::remove_file(&config) {
            if error.kind() != std::io::ErrorKind::NotFound {
                return Err(format!("could not remove {}: {error}", config.display()));
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn choose_onnx_runtime(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Select the ONNX Runtime shared library")
            .pick_file()
        else {
            return;
        };
        if !path.is_file() {
            self.notice = Some(format!("{} is not a file.", path.display()));
            return;
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let looks_like_runtime = file_name.contains("onnxruntime")
            && (file_name.ends_with(".dll")
                || file_name.ends_with(".dylib")
                || file_name.contains(".so"));
        if !looks_like_runtime {
            self.notice = Some(
                "Select the ONNX Runtime shared library (onnxruntime.dll, libonnxruntime.so, or libonnxruntime.dylib)."
                    .to_owned(),
            );
            return;
        }
        let sha256 = match crate::ai_masks::sha256_file_hex(&path) {
            Ok(sha256) => sha256,
            Err(error) => {
                self.notice = Some(format!("Could not hash selected ONNX Runtime: {error:#}"));
                return;
            }
        };
        match Self::persist_onnx_runtime_selection(Some((&path, &sha256))) {
            Ok(()) => {
                self.onnx_runtime_path = Some(path);
                self.onnx_runtime_sha256 = Some(sha256);
                self.notice = Some(
                    "ONNX Runtime selection and SHA-256 pin saved. Restart AuRaw before generating another subject mask."
                        .to_owned(),
                );
            }
            Err(error) => self.notice = Some(error),
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn clear_onnx_runtime(&mut self) {
        match Self::persist_onnx_runtime_selection(None) {
            Ok(()) => {
                self.onnx_runtime_path = None;
                self.onnx_runtime_sha256 = None;
                self.notice = Some(
                    "ONNX Runtime selection cleared. Restart AuRaw to apply the change.".to_owned(),
                );
            }
            Err(error) => self.notice = Some(error),
        }
    }

    #[cfg(target_os = "android")]
    fn birefnet_model_path(&self) -> PathBuf {
        self.android_app
            .internal_data_path()
            .unwrap_or_else(std::env::temp_dir)
            .join("models/birefnet-general-lite.onnx")
    }

    fn show_subject_dialogs(&mut self, ctx: &egui::Context) {
        if self.subject_consent_open {
            egui::Window::new("Download subject-selection model?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Subject masks use BiRefNet; Not Subject is the inverse of that probability map, not an independent background model.");
                    ui.label(format!(
                        "The first use downloads {:.0} MB from the rembg GitHub release and stores it in AuRaw's cache.",
                        BIREFNET_MODEL_BYTES as f64 / 1_000_000.0
                    ));
                    ui.label("Inference is local. No photograph is uploaded.");
                    #[cfg(not(target_os = "android"))]
                    if self.onnx_runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing. AuRaw never downloads native runtime code.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Download and continue").clicked() {
                            self.subject_consent_open = false;
                            self.start_subject_worker(self.birefnet_model_path());
                        }
                        if ui.button("Cancel").clicked() {
                            self.subject_consent_open = false;
                        }
                    });
                });
        }
        if self.subject_receiver.is_some() {
            egui::Window::new("Preparing subject mask")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if let Some((label, downloaded, total)) = self.subject_download_progress {
                        let fraction = downloaded as f32 / total.max(1) as f32;
                        ui.label(format!("Downloading {label}…"));
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .show_percentage()
                                .text(format!(
                                    "{:.1} / {:.1} MB",
                                    downloaded as f64 / 1_000_000.0,
                                    total as f64 / 1_000_000.0
                                )),
                        );
                    } else if self.subject_inferencing {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Running high-quality local subject selection…");
                        });
                    } else {
                        ui.spinner();
                    }
                });
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        if self.object_consent_open {
            egui::Window::new("Download object-selection model?")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Object masks use SAM 2.1 Hiera Tiny with separate image encoder and prompt decoder models.");
                    ui.label(format!(
                        "The first use downloads about {:.0} MB and stores both ONNX files in AuRaw's model cache.",
                        SAM21_MODEL_BYTES_ESTIMATE as f64 / 1_000_000.0
                    ));
                    ui.label("Inference is local. No photograph or prompt stroke is uploaded.");
                    #[cfg(not(target_os = "android"))]
                    if self.onnx_runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Download and continue").clicked() {
                            self.object_consent_open = false;
                            if let Some((mask_index, component_index)) = self.object_pending_target.take() {
                                let (encoder, decoder) = self.sam21_model_paths();
                                self.start_object_worker(mask_index, component_index, encoder, decoder);
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.object_consent_open = false;
                            self.object_pending_target = None;
                        }
                    });
                });
        }
        if self.object_receiver.is_some() {
            egui::Window::new("Preparing object mask")
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if let Some((label, downloaded, total)) = self.object_download_progress {
                        let fraction = downloaded as f32 / total.max(1) as f32;
                        ui.label(format!("Downloading {label}…"));
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .show_percentage()
                                .text(format!(
                                    "{:.1} / {:.1} MB",
                                    downloaded as f64 / 1_000_000.0,
                                    total as f64 / 1_000_000.0
                                )),
                        );
                    } else if self.object_inferencing {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label(if self.object_decoder_only {
                                "Updating the object mask…"
                            } else {
                                "Encoding the selected image region and generating the object mask…"
                            });
                        });
                    } else {
                        ui.spinner();
                    }
                });
            ctx.request_repaint_after(Duration::from_millis(50));
        }
    }
}
