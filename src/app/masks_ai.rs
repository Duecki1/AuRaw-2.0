impl AurawApp {
    fn capture_ai_mask_target(
        &self,
        mask_index: usize,
        component_index: usize,
    ) -> Option<AiMaskTarget> {
        let component = self
            .masks
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))?;
        Some(AiMaskTarget {
            mask_index,
            component_index,
            kind: component.kind,
            geometry: component.geometry.clone(),
        })
    }

    /// Resolve against the captured component content instead of trusting a
    /// stale vector index. A unique match survives a reorder; no match and
    /// duplicate matches are both unsafe to apply.
    fn resolve_ai_mask_target(
        &self,
        target: &AiMaskTarget,
    ) -> std::result::Result<(usize, usize), String> {
        Self::resolve_ai_mask_target_in_stack(&self.masks, target)
    }

    fn resolve_ai_mask_target_in_stack(
        stack: &MaskStack,
        target: &AiMaskTarget,
    ) -> std::result::Result<(usize, usize), String> {
        let matches = stack
            .masks
            .iter()
            .enumerate()
            .flat_map(|(mask_index, mask)| {
                mask.components
                    .iter()
                    .enumerate()
                    .map(move |(component_index, component)| (mask_index, component_index, component))
            })
            .filter(|(_, _, component)| {
                component.kind == target.kind && component.geometry == target.geometry
            })
            .map(|(mask_index, component_index, _)| (mask_index, component_index))
            .collect::<Vec<_>>();
        match matches.as_slice() {
            [location] => Ok(*location),
            [] => {
                let message = match stack.masks.get(target.mask_index) {
                    None => "The target mask was deleted before inference completed.",
                    Some(mask) if mask.components.get(target.component_index).is_none() => {
                        "The target mask component was deleted before inference completed."
                    }
                    Some(mask)
                        if mask.components[target.component_index].kind != target.kind =>
                    {
                        "The target component changed type before inference completed."
                    }
                    Some(_) => "The target component changed before inference completed.",
                };
                Err(message.to_owned())
            }
            _ => Err("The target component is ambiguous after editing; the stale result was discarded.".to_owned()),
        }
    }

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

    pub(crate) fn ai_mask_update_busy(&self) -> bool {
        self.ai_mask_update_active
            || self.subject_task_id.is_some()
            || self.object_task_id.is_some()
            || self.subject_receiver.is_some()
            || self.object_receiver.is_some()
            || self.subject_consent_open
            || self.object_consent_open
    }

    /// A failed task remains in the background-task list until the user
    /// dismisses its details, but it must no longer own the AI-mask request
    /// slot. Likewise, an ID whose task was already removed cannot be allowed
    /// to block all future mask requests.
    fn recover_terminal_ai_mask_task_owners(&mut self) {
        let stale_subject = self.subject_receiver.is_none()
            && self.subject_task_id.is_some_and(|id| {
                self.background_tasks
                    .snapshot(id)
                    .is_none_or(|task| task.status == TaskStatus::Failed)
            });
        if stale_subject {
            if let Some(id) = self.subject_task_id {
                self.clear_ai_mask_task_owner(id);
            }
        }

        let stale_object = self.object_receiver.is_none()
            && self.object_task_id.is_some_and(|id| {
                self.background_tasks
                    .snapshot(id)
                    .is_none_or(|task| task.status == TaskStatus::Failed)
            });
        if stale_object {
            if let Some(id) = self.object_task_id {
                self.clear_ai_mask_task_owner(id);
            }
        }

                self.background_tasks
                    .snapshot(id)
                    .is_none_or(|task| task.status == TaskStatus::Failed)
            });
                self.clear_ai_mask_task_owner(id);
            }
        }
    }

    pub(crate) fn ai_masks_need_update(&self) -> bool {
        self.ai_masks_need_update && !self.masks.masks.is_empty()
    }

    /// Number of content-aware mask components still awaiting the active
    /// batch refresh. Subject/background components share one worker, but
    /// each component is counted so Library can report meaningful progress.
    pub(crate) fn ai_mask_update_remaining_target_count(&self) -> usize {
        if !self.ai_mask_update_active {
            return 0;
        }

        let subject_targets = self
            .masks
            .masks
            .iter()
            .flat_map(|mask| &mask.components)
            .filter(|component| {
                matches!(
                    (component.kind, &component.geometry),
                    (
                        MaskKind::Subject | MaskKind::Background,
                        MaskGeometry::Ai { .. },
                    )
                )
            })
            .count();
        let current_object = usize::from(
            self.object_receiver.is_some() || self.object_pending_target.is_some(),
        );
        );
        let subject_remaining = usize::from(self.ai_mask_update_subject_pending) * subject_targets;
        subject_remaining
            + self.ai_mask_update_object_queue.len()
            + current_object
    }

    /// Find masks that can be regenerated from their semantic component or
    /// saved prompt data. A pasted/stale mask may intentionally have no cached
    /// generated bitmap, so `mask: Some(_)` must not be required here.
    fn generated_ai_mask_targets(
        &self,
    ) -> (
        bool,
        VecDeque<(usize, usize)>,
        VecDeque<(usize, usize)>,
    ) {
        let mut subject = false;
        let mut objects = VecDeque::new();
        for (mask_index, local_mask) in self.masks.masks.iter().enumerate() {
            for (component_index, component) in local_mask.components.iter().enumerate() {
                match (component.kind, &component.geometry) {
                    (
                        MaskKind::Subject | MaskKind::Background,
                        MaskGeometry::Ai { .. },
                    ) => subject = true,
                    (
                        MaskKind::Object,
                        MaskGeometry::Object { strokes, .. },
                    ) if strokes
                        .iter()
                        .any(|stroke| stroke.positive && !stroke.points.is_empty()) =>
                    {
                        objects.push_back((mask_index, component_index));
                    }
                    _ => {}
                }
            }
        }
    }

    fn has_range_mask_targets(&self) -> bool {
        self.masks.masks.iter().any(|mask| {
            mask.components.iter().any(|component| {
                matches!(
                    &component.geometry,
                    MaskGeometry::LuminanceRange { .. } | MaskGeometry::ColorRange { .. }
                )
            })
        })
    }

    fn invalidate_generated_mask_sources(&mut self) {
        self.mask_source_cache = None;
        self.subject_mask_cache = None;
        self.object_cache = None;
        self.subject_generation = self.subject_generation.wrapping_add(1);
        self.object_generation = self.object_generation.wrapping_add(1);
        self.object_pending_target = None;
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        self.ai_mask_update_failed = false;
    }

    pub(crate) fn note_inpainting_changed_for_ai_masks(&mut self) {
        let has_ranges = self.has_range_mask_targets();
        self.invalidate_generated_mask_sources();
        self.ai_masks_need_update = has_subject
            || !object_targets.is_empty()
            || has_ranges;
    }

    pub(crate) fn note_lens_correction_changed_for_masks(&mut self) {
        let has_ranges = self.has_range_mask_targets();
        self.invalidate_generated_mask_sources();
        // Manual/geometric masks remain intact and are immediately reused.
        // Only source-dependent masks need regeneration against the newly
        // corrected (or uncorrected) image geometry.
        self.ai_masks_need_update = has_subject
            || !object_targets.is_empty()
            || has_ranges;
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn validate_onnx_runtime_for_ai(&mut self) -> bool {
        let (Some(runtime_path), Some(runtime_sha256)) = (
            self.onnx_runtime_path.clone(),
            self.onnx_runtime_sha256.clone(),
        ) else {
            self.notice = Some(
                "Choose an ONNX Runtime library under Settings before using desktop AI tools."
                    .to_owned(),
            );
            return false;
        };
        match crate::ai_masks::probe_runtime_subprocess(&runtime_path, &runtime_sha256) {
            Ok(()) => true,
            Err(error) => {
                self.notice = Some(format!(
                    "ONNX Runtime validation failed: {error:#}. Select a different onnxruntime.dll in Settings."
                ));
                false
            }
        }
    }

    fn ai_runtime_ready(&mut self) -> bool {
        #[cfg(target_os = "android")]
        {
            true
        }
        #[cfg(not(target_os = "android"))]
        {
            self.validate_onnx_runtime_for_ai()
        }
    }

    pub(crate) fn request_update_all_ai_masks(&mut self, frame: &eframe::Frame) {
        self.recover_terminal_ai_mask_task_owners();
        if self.ai_mask_update_busy() {
            self.notice = Some("Wait for the current AI mask operation to finish.".to_owned());
            return;
        }
            self.generated_ai_mask_targets();
        let update_ranges = self.has_range_mask_targets();
        if self.masks.masks.is_empty() {
            self.ai_masks_need_update = false;
            return;
        }
        #[cfg(not(target_os = "android"))]
            && !self.validate_onnx_runtime_for_ai()
        {
            return;
        }

        if update_subject
            || !object_targets.is_empty()
            || update_ranges
        {
            // Force a new canonical source because lens correction or
            // inpainting changed the image under content-aware masks.
            self.mask_source_cache = None;
            self.subject_mask_cache = None;
            self.object_cache = None;
            if let Err(error) = self.capture_mask_source(frame) {
                self.notice = Some(error);
                return;
            }

            if update_ranges {
                let source = self.mask_source_cache.clone();
                let mut range_layers_changed = Vec::new();
                for (mask_index, mask) in self.masks.masks.iter_mut().enumerate() {
                    let mut changed = false;
                    for component in &mut mask.components {
                        match &mut component.geometry {
                            MaskGeometry::LuminanceRange { source: target, .. }
                            | MaskGeometry::ColorRange { source: target, .. } => {
                                *target = source.clone();
                                changed = true;
                            }
                            _ => {}
                        }
                    }
                    if changed {
                        range_layers_changed.push(mask_index);
                    }
                }
                for mask_index in range_layers_changed {
                    self.mark_mask_geometry_dirty(mask_index);
                }
            }
        }

            self.ai_masks_need_update = false;
            self.notice = Some("Masks were refreshed for the current image geometry.".to_owned());
            self.egui_ctx.request_repaint();
            return;
        }

        self.ai_mask_update_active = true;
        self.ai_mask_update_subject_pending = update_subject;
        self.ai_mask_update_object_queue = object_targets;
        self.ai_mask_update_failed = false;

        if update_subject {
            let path = self.birefnet_model_path();
            if path.is_file() && self.vitmatte_model_path().is_file() {
                self.start_subject_worker(path);
            } else {
                self.subject_consent_open = true;
                self.egui_ctx.request_repaint();
            }
        } else {
            self.continue_ai_mask_update();
        }
    }

    fn continue_ai_mask_update(&mut self) {
        if !self.ai_mask_update_active
            || self.ai_mask_update_subject_pending
            || self.subject_receiver.is_some()
            || self.object_receiver.is_some()
            || self.subject_consent_open
            || self.object_consent_open
        {
            return;
        }

        while let Some((mask_index, component_index)) =
            self.ai_mask_update_object_queue.pop_front()
        {
            let valid = self
                .masks
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(component_index))
                .is_some_and(|component| {
                    matches!(
                        &component.geometry,
                        MaskGeometry::Object { strokes, .. } if strokes
                            .iter()
                            .any(|stroke| stroke.positive && !stroke.points.is_empty())
                    )
                });
            if !valid {
                continue;
            }

            let (encoder, decoder) = self.sam21_model_paths();
            if encoder.is_file() && decoder.is_file() && self.vitmatte_model_path().is_file() {
                self.start_object_worker(mask_index, component_index, encoder, decoder);
            } else {
                self.object_pending_target = Some((mask_index, component_index));
                self.object_consent_open = true;
                self.egui_ctx.request_repaint();
            }
            return;
        }

        while let Some((mask_index, component_index)) =
        {
            let valid = self
                .masks
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(component_index))
                .is_some_and(|component| {
                    matches!(
                        (component.kind, &component.geometry),
                    )
                });
            if !valid {
                continue;
            }
            } else {
                self.egui_ctx.request_repaint();
            }
            return;
        }

        self.finish_ai_mask_update();
    }

    fn finish_ai_mask_update(&mut self) {
        if !self.ai_mask_update_active {
            return;
        }
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        if self.ai_mask_update_failed {
            self.ai_masks_need_update = true;
            self.notice = Some(
                "Some AI masks could not be updated. The update button will remain available."
                    .to_owned(),
            );
        } else {
            self.ai_masks_need_update = false;
            self.notice = Some("Masks were refreshed for the current image geometry.".to_owned());
        }
        self.egui_ctx.request_repaint();
    }

    fn cancel_ai_mask_update(&mut self) {
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        self.ai_mask_update_failed = false;
        self.object_pending_target = None;
        self.ai_masks_need_update = true;
        self.notice = Some("AI-mask update canceled.".to_owned());
        self.egui_ctx.request_repaint();
    }

    #[cfg(target_os = "android")]
    fn capture_mask_source_from_active_preview(
        &self,
        frame: &eframe::Frame,
    ) -> Result<MaskRgbImage, String> {
        let render_state = frame
            .wgpu_render_state()
            .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
        let raw = self
            .preview_raw
            .as_ref()
            .ok_or_else(|| "The preview image is not available yet.".to_owned())?;
        let pipeline = self
            .gpu_pipeline
            .as_ref()
            .ok_or_else(|| "Open an image before creating this mask.".to_owned())?;

        // The expanded processing graph no longer reliably leaves enough of
        // Android's GPU budget for a second full RawGpuPipeline. Render the
        // canonical unedited source through the already-resident preview graph,
        // read it back, then restore the current edited preview before this UI
        // frame is painted. This is also the path used while reconstructing
        // saved range-mask sources during image load.
        let reference_exposure = ExposureParams::scene_referred_default();
        let reference_masks = MaskStack::default();
        let reference_params = GpuParams::new(&reference_exposure, &reference_masks, raw);
        pipeline.recompute(&render_state.queue, &render_state.device, &reference_params);

        let readback = pipeline.read_output_region_blocking(
            &render_state.device,
            &render_state.queue,
            0,
            0,
            pipeline.width,
            pipeline.height,
        );

        // `recompute` updates the complete graph, so restore exactly the state
        // the user should see even when the readback failed. A failure must not
        // leave the preview displaying the neutral AI-mask rendition.
        let restore_params = if self.original_preview_requested {
            GpuParams::new(&self.original_preview_exposure, &reference_masks, raw)
                .with_vignette_geometry(self.geometry)
        } else {
            GpuParams::new(&self.target_exposure, &self.masks, raw)
                .with_vignette_geometry(self.geometry)
        };
        pipeline.recompute(&render_state.queue, &render_state.device, &restore_params);

        let rgba = readback
            .map_err(|error| format!("Could not read the original RAW for masking: {error:#}"))?;
        MaskRgbImage::new(pipeline.width, pipeline.height, rgba)
            .ok_or_else(|| "The canonical mask source has invalid dimensions.".to_owned())
    }

    pub(crate) fn capture_mask_source(&mut self, frame: &eframe::Frame) -> Result<(), String> {
        if self.mask_source_cache.is_some() {
            return Ok(());
        }

        #[cfg(target_os = "android")]
        {
            let source = self.capture_mask_source_from_active_preview(frame)?;
            self.mask_source_cache = Some(source);
            return Ok(());
        }

        #[cfg(not(target_os = "android"))]
        {
            let render_state = frame
                .wgpu_render_state()
                .ok_or_else(|| "The GPU preview is not available.".to_owned())?;
            let program_template = self
                .gpu_pipeline
                .as_ref()
                .map(RawGpuPipeline::program_template)
                .or_else(|| self.preview_program_template.clone())
                .ok_or_else(|| "Open an image before creating this mask.".to_owned())?;
            let full_raw = self
                .loaded_raw
                .as_ref()
                .ok_or_else(|| "The original RAW is not available.".to_owned())?;
            let raw = if full_raw.width.max(full_raw.height) <= 3072 {
                Arc::clone(full_raw)
            } else {
                Arc::new(build_proxy(
                    full_raw,
                    ProxySpec { max_edge: 3072 },
                ))
            };

            let reference_exposure = ExposureParams::scene_referred_default();
            let reference_masks = MaskStack::default();
            let params = GpuParams::new(&reference_exposure, &reference_masks, &raw);
            let reference_pipeline = RawGpuPipeline::new_headless_reusing_program_template(
                &render_state.device,
                &render_state.queue,
                &raw,
                &params,
                ProcessingQuality::Preview,
                &program_template,
            )
            .map_err(|error| {
                format!("Could not prepare the original RAW for masking: {error:#}")
            })?;
            reference_pipeline
                .update_inpaint_layer(
                    &render_state.queue,
                    self.inpaint_layer.as_ref(),
                    0,
                    0,
                    raw.width,
                    raw.height,
                )
                .map_err(|error| {
                    format!("Could not prepare erased pixels for masking: {error:#}")
                })?;
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
                .map_err(|error| {
                    format!("Could not read the original RAW for masking: {error:#}")
                })?;
            let source = MaskRgbImage::new(
                reference_pipeline.width,
                reference_pipeline.height,
                rgba,
            )
            .ok_or_else(|| "The canonical mask source has invalid dimensions.".to_owned())?;
            self.mask_source_cache = Some(source);
            Ok(())
        }
    }

    pub(crate) fn report_ai_mask_error(&mut self, error: String) {
        self.notice = Some(error.clone());
        self.object_error_dialog = Some(error);
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn request_subject_mask(&mut self, frame: &eframe::Frame) {
        self.object_error_dialog = None;
        self.recover_terminal_ai_mask_task_owners();
        if let Some(mask) = self.subject_mask_cache.clone() {
            self.apply_subject_mask(mask);
            return;
        }
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
            return;
        }
        if let Err(error) = self.capture_mask_source(frame) {
            self.report_ai_mask_error(error);
            return;
        }
        let path = self.birefnet_model_path();
        let vitmatte = self.vitmatte_model_path();
        if path.is_file() && vitmatte.is_file() {
            self.start_subject_worker(path);
        } else {
            self.subject_consent_open = true;
        }
    }

    fn start_subject_worker(&mut self, model_path: PathBuf) {
        if self.subject_task_id.is_some() || self.subject_receiver.is_some() {
            return;
        }
        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The preview could not be prepared for subject selection.".to_owned());
            return;
        };
        let vitmatte_path = self.vitmatte_model_path();
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;

        self.subject_generation = self.subject_generation.wrapping_add(1);
        let generation = self.subject_generation;
        let request = SubjectMaskTaskRequest {
            document_id: self.sidecar_generation,
            generation,
            source,
            model_path,
            vitmatte_path,
            runtime_path,
            runtime_sha256,
        };

        if let Some(task_id) = self.library_ai_mask_refresh_task_id.filter(|task_id| {
            self.background_tasks.current_id() == Some(*task_id) && self.ai_mask_update_active
        }) {
            self.start_subject_mask_task(task_id, request);
        } else {
            // Full SHA-256 verification belongs in the worker. Hashing the
            // 328 MB subject model set here blocks Android's UI thread long
            // enough to look like a hung mask operation.
            let needs_download =
                !request.model_path.is_file() || !request.vitmatte_path.is_file();
            if needs_download {
                let task_id = self.enqueue_background_action(
                    TaskKind::SubjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Downloading subject-mask model",
                    TaskProgress::indeterminate("Waiting for earlier background work…"),
                    true,
                    BackgroundAction::SubjectMask(request),
                );
                self.subject_task_id = Some(task_id);
            } else {
                let task_id = self.background_tasks.start_nonblocking(
                    TaskKind::SubjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Generating subject mask",
                    TaskProgress::indeterminate("Running local subject-mask inference…"),
                    true,
                );
                self.start_subject_mask_task(task_id, request);
            }
        }
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
        let Some(task_id) = self.subject_task_id else {
            return;
        };
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.subject_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        let finished = matches!(event, SubjectMaskEvent::Finished(_));
                        events.push(event);
                        if finished {
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut finished = None;
        for event in events {
            match event {
                SubjectMaskEvent::DownloadProgress {
                    label,
                    downloaded,
                    total,
                } => {
                    self.subject_download_progress = Some((label, downloaded, total));
                    self.subject_inferencing = false;
                    self.background_tasks.set_global_visible(task_id, true);
                    self.background_tasks
                        .rename(task_id, "Downloading subject-mask model");
                    let progress = TaskProgress::units(
                        downloaded,
                        total,
                        Some("bytes".to_owned()),
                        format!("Downloading {label}"),
                    )
                    .with_detail(format!(
                        "{:.1} / {:.1} MB",
                        downloaded as f64 / 1_000_000.0,
                        total as f64 / 1_000_000.0
                    ));
                    self.update_background_progress(Some(task_id), progress);
                }
                SubjectMaskEvent::Inferencing => {
                    self.subject_download_progress = None;
                    self.subject_inferencing = true;
                    self.background_tasks.set_global_visible(task_id, false);
                    if matches!(
                        self.background_tasks.snapshot(task_id).map(|task| task.kind),
                        Some(TaskKind::SubjectMask { .. })
                    ) {
                        self.background_tasks.release_current(task_id);
                    }
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::indeterminate("Running local subject-mask inference…"),
                    );
                }
                SubjectMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err("The subject-mask worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            return;
        };

        let updating_all = self.ai_mask_update_active && self.ai_mask_update_subject_pending;
        let library_task = self.library_ai_mask_refresh_task_id == Some(task_id);
        let cancelled = self.background_task_cancelled(task_id);
        let stale = self.subject_job_document_id != self.sidecar_generation
            || self.subject_job_generation != self.subject_generation;
        self.subject_receiver = None;
        self.subject_task_id = None;
        self.subject_download_progress = None;
        self.subject_inferencing = false;

        let mut succeeded = false;
        let mut error_message = None;
        if !cancelled && !stale {
            match result {
                Ok(result) => {
                    if let Some(mask) = MaskImage::new(result.width, result.height, result.mask) {
                        self.apply_subject_mask(mask);
                        succeeded = true;
                    } else {
                        error_message =
                            Some("Subject selection returned an invalid mask image.".to_owned());
                    }
                }
                Err(error) => {
                    error_message = Some(format!("Subject selection failed: {error}"));
                }
            }
        }

        if library_task {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if updating_all {
                self.ai_mask_update_subject_pending = false;
                self.ai_mask_update_failed |= !succeeded;
                if let Some(message) = error_message.clone() {
                    self.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if cancelled || succeeded {
            self.finish_background_task(task_id);
        } else {
            let message = error_message
                .unwrap_or_else(|| {
                    if stale {
                        "Subject selection became stale before inference completed.".to_owned()
                    } else {
                        "Subject selection did not produce a mask.".to_owned()
                    }
                });
            self.notice = Some(message.clone());
            self.fail_background_task(task_id, message);
        }
        self.egui_ctx.request_repaint();
    }
        let mut events = Vec::new();
        let mut disconnected = false;
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        let finished = matches!(event::Finished(_));
                        events.push(event);
                        if finished {
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }
        let mut finished = None;
        for event in events {
            match event {
                    self.background_tasks.set_global_visible(task_id, true);
                    self.background_tasks
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::units(
                            downloaded,
                            total,
                            Some("bytes".to_owned()),
                        )
                        .with_detail(format!(
                            "{:.1} / {:.1} MB",
                            downloaded as f64 / 1_000_000.0,
                            total as f64 / 1_000_000.0
                        )),
                    );
                }
                    self.background_tasks.set_global_visible(task_id, false);
                    if matches!(
                        self.background_tasks.snapshot(task_id).map(|task| task.kind),
                    ) {
                        self.background_tasks.release_current(task_id);
                    }
                    self.update_background_progress(
                        Some(task_id),
                    );
                }
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err(
            ));
        }
        let Some(result) = finished else {
            return;
        };

        let updating_all = self.ai_mask_update_active && target.is_some();
        let library_task = self.library_ai_mask_refresh_task_id == Some(task_id);
        let cancelled = self.background_task_cancelled(task_id);

        let mut succeeded = false;
        let mut error_message = None;
        if !cancelled && !stale {
            match (target, job_category, result) {
                (Some(target), Some(expected_category), Ok(result)) => {
                    let location = self.resolve_ai_mask_target(&target);
                    let mask_image = MaskImage::new(result.width, result.height, result.mask);
                    match (location, mask_image) {
                        (_, None) => {
                            error_message = Some(
                                    .to_owned(),
                            );
                        }
                        (Err(error), Some(_)) => error_message = Some(error),
                        (Ok((mask_index, component_index)), Some(mask_image)) => {
                            let applied = self
                                .masks
                                .masks
                                .get_mut(mask_index)
                                .and_then(|mask| mask.components.get_mut(component_index))
                                .is_some_and(|component| {
                (_, _, Err(error)) => {
                }
            }
        }

        if library_task {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if updating_all {
                self.ai_mask_update_failed |= !succeeded;
                if let Some(message) = error_message.clone() {
                    self.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if cancelled || succeeded {
            self.finish_background_task(task_id);
        } else {
            let message = error_message
                .unwrap_or_else(|| {
                    if stale {
                    } else {
                    }
                });
            self.notice = Some(message.clone());
            self.fail_background_task(task_id, message);
        }
        self.egui_ctx.request_repaint();
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
        self.object_error_dialog = None;
        self.recover_terminal_ai_mask_task_owners();
        #[cfg(not(target_os = "android"))]
        if !self.validate_onnx_runtime_for_ai() {
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
            self.report_ai_mask_error(
                "The original image source is not ready for object selection. Re-open the Object mask or create it again."
                    .to_owned(),
            );
            return;
        }
        let (encoder, decoder) = self.sam21_model_paths();
        if encoder.is_file() && decoder.is_file() && self.vitmatte_model_path().is_file() {
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
        let library_task = self.library_ai_mask_refresh_task_id.filter(|task_id| {
            self.background_tasks.current_id() == Some(*task_id) && self.ai_mask_update_active
        });
        if library_task.is_none() {
            if let Some(existing) = self.object_task_id {
                self.cancel_background_task(existing);
                if self.object_receiver.is_some() {
                    // Native inference may only stop between phases. Keep the
                    // receiver paired with its stable task ID and retain only
                    // the newest requested target for the follow-up run.
                    self.object_pending_target = Some((mask_index, component_index));
                    self.object_generation = self.object_generation.wrapping_add(1);
                    return;
                }
            }
        } else if self.object_receiver.is_some() {
            self.object_pending_target = Some((mask_index, component_index));
            self.object_generation = self.object_generation.wrapping_add(1);
            return;
        }

        let Some(source) = self.mask_source_cache.clone() else {
            self.notice =
                Some("The original image source is unavailable for object selection.".to_owned());
            return;
        };
        let (strokes, brush_size, edge_refine) = {
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
                ..
            } = &component.geometry
            else {
                return;
            };
            let captured_brush_size = strokes
                .iter()
                .filter_map(|stroke| (stroke.brush_size > 0.0).then_some(stroke.brush_size))
                .fold(0.0f32, f32::max);
            (
                strokes.clone(),
                if captured_brush_size > 0.0 {
                    captured_brush_size
                } else {
                    *brush_size
                },
                *edge_refine,
            )
        };
        let cache = self
            .object_cache
            .as_ref()
            .filter(|(target, _)| *target == (mask_index, component_index))
            .map(|(_, cache)| cache.clone());
        let object_request = ObjectMaskRequest {
            source_width: source.width,
            source_height: source.height,
            source_rgba: source.rgba.to_vec(),
            strokes,
            brush_size,
            edge_refine,
            cache,
        };
        self.object_generation = self.object_generation.wrapping_add(1);
        let generation = self.object_generation;
        self.object_pending_target = None;
        #[cfg(not(target_os = "android"))]
        let runtime_path = self.onnx_runtime_path.clone();
        #[cfg(not(target_os = "android"))]
        let runtime_sha256 = self.onnx_runtime_sha256.clone();
        #[cfg(target_os = "android")]
        let runtime_path = None;
        #[cfg(target_os = "android")]
        let runtime_sha256 = None;
        let Some(target) = self.capture_ai_mask_target(mask_index, component_index) else {
            self.notice = Some("The selected object mask is no longer available.".to_owned());
            return;
        };
        let request = ObjectMaskTaskRequest {
            document_id: self.sidecar_generation,
            generation,
            target,
            encoder_path,
            decoder_path,
            vitmatte_path: self.vitmatte_model_path(),
            runtime_path,
            runtime_sha256,
            request: object_request,
        };

        if let Some(task_id) = library_task {
            self.start_object_mask_task(task_id, request);
        } else {
            // The object worker verifies every model before loading it.
            // Keep this UI-thread decision metadata-only on Android/mobile
            // storage instead of synchronously hashing the complete model set.
            let needs_download = !request.encoder_path.is_file()
                || !request.decoder_path.is_file()
                || !request.vitmatte_path.is_file();
            if needs_download {
                let task_id = self.enqueue_background_action(
                    TaskKind::ObjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Downloading object-mask model",
                    TaskProgress::indeterminate("Waiting for earlier background work…"),
                    true,
                    BackgroundAction::ObjectMask(request),
                );
                self.object_task_id = Some(task_id);
            } else {
                let task_id = self.background_tasks.start_nonblocking(
                    TaskKind::ObjectMask {
                        document_id: request.document_id,
                        generation,
                    },
                    "Generating object mask",
                    TaskProgress::indeterminate(
                        "Encoding the image and generating the object mask…",
                    ),
                    true,
                );
                self.start_object_mask_task(task_id, request);
            }
        }
        self.egui_ctx.request_repaint();
    }

    fn poll_object_worker(&mut self) {
        let Some(task_id) = self.object_task_id else {
            return;
        };
        let mut events = Vec::new();
        let mut disconnected = false;
        if let Some(receiver) = &self.object_receiver {
            loop {
                match receiver.try_recv() {
                    Ok(event) => {
                        let finished = matches!(event, ObjectMaskEvent::Finished(_));
                        events.push(event);
                        if finished {
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
        }

        let mut finished = None;
        for event in events {
            match event {
                ObjectMaskEvent::DownloadProgress {
                    label,
                    downloaded,
                    total,
                } => {
                    self.object_download_progress = Some((label, downloaded, total));
                    self.object_inferencing = false;
                    self.background_tasks.set_global_visible(task_id, true);
                    self.background_tasks
                        .rename(task_id, "Downloading object-mask model");
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::units(
                            downloaded,
                            total,
                            Some("bytes".to_owned()),
                            format!("Downloading {label}"),
                        )
                        .with_detail(format!(
                            "{:.1} / {:.1} MB",
                            downloaded as f64 / 1_000_000.0,
                            total as f64 / 1_000_000.0
                        )),
                    );
                }
                ObjectMaskEvent::Inferencing { decoder_only } => {
                    self.object_download_progress = None;
                    self.object_inferencing = true;
                    self.object_decoder_only = decoder_only;
                    self.background_tasks.set_global_visible(task_id, false);
                    if matches!(
                        self.background_tasks.snapshot(task_id).map(|task| task.kind),
                        Some(TaskKind::ObjectMask { .. })
                    ) {
                        self.background_tasks.release_current(task_id);
                    }
                    self.update_background_progress(
                        Some(task_id),
                        TaskProgress::indeterminate(if decoder_only {
                            "Updating the object mask…"
                        } else {
                            "Encoding the image and generating the object mask…"
                        }),
                    );
                }
                ObjectMaskEvent::Finished(result) => finished = Some(result),
            }
        }
        if finished.is_none() && disconnected {
            finished = Some(Err("The object-mask worker stopped unexpectedly.".to_owned()));
        }
        let Some(result) = finished else {
            return;
        };

        let target = self.object_job_target.take();
        let generation = self.object_job_generation;
        let document_id = self.object_job_document_id;
        let updating_all = self.ai_mask_update_active && target.is_some();
        let library_task = self.library_ai_mask_refresh_task_id == Some(task_id);
        let cancelled = self.background_task_cancelled(task_id);
        let stale = generation != self.object_generation
            || document_id != self.sidecar_generation;
        let failed_during_inference = self.object_inferencing;
        self.object_receiver = None;
        self.object_task_id = None;
        self.object_download_progress = None;
        self.object_inferencing = false;

        let mut succeeded = false;
        let mut error_message = None;
        if !cancelled && !stale {
            match (target, result) {
                (Some(target), Ok(result)) => {
                    let crate::ai_masks::ObjectMaskResult {
                        width,
                        height,
                        mask: pixels,
                        cache,
                    } = result;
                    let location = self.resolve_ai_mask_target(&target);
                    let mask = MaskImage::new(width, height, pixels);
                    match (location, mask) {
                        (_, None) => {
                            error_message = Some(
                                "Object selection returned malformed dimensions or pixel data."
                                    .to_owned(),
                            );
                        }
                        (Err(error), Some(_)) => error_message = Some(error),
                        (Ok((mask_index, component_index)), Some(mask)) => {
                            let applied = self
                                .masks
                                .masks
                                .get_mut(mask_index)
                                .and_then(|local| local.components.get_mut(component_index))
                                .is_some_and(|component| {
                                    if let MaskGeometry::Object {
                                        mask: generated_mask,
                                        ..
                                    } = &mut component.geometry
                                    {
                                        *generated_mask = Some(mask);
                                        true
                                    } else {
                                        false
                                    }
                                });
                            if applied {
                                self.object_cache = Some(((mask_index, component_index), cache));
                                self.mark_mask_geometry_dirty(mask_index);
                                self.blink_selected_component();
                                succeeded = true;
                            } else {
                                error_message = Some(
                                    "The target component changed type before inference completed."
                                        .to_owned(),
                                );
                            }
                        }
                    }
                }
                (_, Ok(_)) => {}
                (_, Err(error)) => {
                    error_message = Some(format!("Object selection failed: {error}"));
                }
            }
        }

        if library_task {
            if cancelled {
                self.cancel_ai_mask_update();
            } else if let Some((mask_index, component_index)) = self.object_pending_target.take() {
                let (encoder, decoder) = self.sam21_model_paths();
                self.start_object_worker(mask_index, component_index, encoder, decoder);
            } else if updating_all {
                self.ai_mask_update_failed |= !succeeded;
                if !succeeded {
                    self.ai_mask_update_object_queue.clear();
                }
                if let Some(message) = error_message.clone() {
                    self.notice = Some(message);
                }
                self.continue_ai_mask_update();
            }
        } else if cancelled || stale {
            self.finish_background_task(task_id);
            if let Some((mask_index, component_index)) = self.object_pending_target.take() {
                let (encoder, decoder) = self.sam21_model_paths();
                self.start_object_worker(mask_index, component_index, encoder, decoder);
            }
        } else if succeeded {
            self.finish_background_task(task_id);
        } else {
            let message = error_message
                .unwrap_or_else(|| {
                    if stale {
                        "Object selection became stale before inference completed.".to_owned()
                    } else {
                        "Object selection did not produce a mask.".to_owned()
                    }
                });
            self.notice = Some(message.clone());
            if failed_during_inference {
                self.object_error_dialog = Some(message);
                self.finish_background_task(task_id);
            } else {
                self.fail_background_task(task_id, message);
            }
        }
        self.egui_ctx.request_repaint();
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
    fn vitmatte_model_path(&self) -> PathBuf {
        let root = std::env::var_os("XDG_CACHE_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
            .unwrap_or_else(std::env::temp_dir);
        root.join("auraw/models/vitmatte-small-composition-1k.onnx")
    }

    #[cfg(not(target_os = "android"))]
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
        if self.desktop_picker_receiver.is_some() {
            return;
        }
        let mut dialog = rfd::AsyncFileDialog::new()
            .set_title("Select the ONNX Runtime shared library");
        if let Some(parent) = self
            .onnx_runtime_path
            .as_deref()
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            dialog = dialog.set_directory(parent);
        }
        let (sender, receiver) = std::sync::mpsc::channel();
        let context = self.egui_ctx.clone();
        std::thread::spawn(move || {
            let result = pollster::block_on(dialog.pick_file())
            .map(|handle| handle.path().to_path_buf())
            .map(Self::validate_and_persist_onnx_runtime)
            .transpose();
            let _ = sender.send(crate::app::DesktopPickerEvent::OnnxRuntime(result));
            context.request_repaint();
        });
        self.desktop_picker_receiver = Some(receiver);
    }

    #[cfg(not(target_os = "android"))]
    fn validate_and_persist_onnx_runtime(path: PathBuf) -> Result<(PathBuf, String), String> {
        if !path.is_file() {
            return Err(format!("{} is not a file.", path.display()));
        }
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let looks_like_runtime = if cfg!(target_os = "windows") {
            file_name == "onnxruntime.dll"
        } else if cfg!(target_os = "macos") {
            file_name == "libonnxruntime.dylib"
                || (file_name.starts_with("libonnxruntime.") && file_name.ends_with(".dylib"))
        } else {
            file_name == "libonnxruntime.so" || file_name.starts_with("libonnxruntime.so.")
        };
        if !looks_like_runtime {
            return Err(
                "Select the ONNX Runtime shared library (onnxruntime.dll, libonnxruntime.so, or libonnxruntime.dylib)."
                    .to_owned(),
            );
        }
        let sha256 = crate::ai_masks::sha256_file_hex(&path)
            .map_err(|error| format!("Could not hash selected ONNX Runtime: {error:#}"))?;
        if let Err(error) = crate::ai_masks::probe_runtime_subprocess(&path, &sha256) {
            return Err(format!(
                "This ONNX Runtime could not be loaded safely: {error:#}"
            ));
        }
        Self::persist_onnx_runtime_selection(Some((&path, &sha256)))?;
        Ok((path, sha256))
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

    #[cfg(target_os = "android")]
    fn vitmatte_model_path(&self) -> PathBuf {
        self.android_app
            .internal_data_path()
            .unwrap_or_else(std::env::temp_dir)
            .join("models/vitmatte-small-composition-1k.onnx")
    }

    #[cfg(target_os = "android")]
}
