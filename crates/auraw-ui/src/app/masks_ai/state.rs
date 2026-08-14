use super::*;

impl AurawApp {
    pub(in crate::app) fn capture_ai_mask_target(
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

    pub(in crate::app) fn resolve_ai_mask_target(
        &self,
        target: &AiMaskTarget,
    ) -> std::result::Result<(usize, usize), String> {
        Self::resolve_ai_mask_target_in_stack(&self.masks, target)
    }

    pub(in crate::app) fn resolve_ai_mask_target_in_stack(
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

    pub(crate) fn note_subject_refinement_interaction(&mut self) {
        const INTERACTIVE_MASK_INTERVAL: Duration = Duration::from_millis(45);
        const SHARED_REFINEMENT_LAYER: usize = MAX_LOCAL_MASKS;

        if self.mask_interaction_dirty_layer != Some(SHARED_REFINEMENT_LAYER) {
            self.finish_mask_geometry_interaction();
            self.mask_interaction_dirty_layer = Some(SHARED_REFINEMENT_LAYER);
            self.mask_interaction_last_upload = None;
        }

        self.mask_interaction_has_uncommitted_change = true;
        let now = Instant::now();
        let upload_due = self
            .mask_interaction_last_upload
            .is_none_or(|last| now.duration_since(last) >= INTERACTIVE_MASK_INTERVAL);
        if upload_due {
            self.mark_all_mask_layers_dirty();
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
                if layer == MAX_LOCAL_MASKS {
                    self.mark_all_mask_layers_dirty();
                } else {
                    self.mark_mask_geometry_dirty(layer);
                }
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
            subject_refinement: self
                .subject_refinement_active
                .then(|| self.masks.subject_refinement.clone()),
            object_cache: self.object_cache.clone(),
        });
    }

    pub(crate) fn commit_mask_touch_gesture(&mut self) {
        self.mask_touch_gesture_backup = None;
    }

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
        let refinement_restored = if let Some(subject_refinement) = backup.subject_refinement {
            self.masks.subject_refinement = subject_refinement;
            true
        } else {
            false
        };
        self.object_cache = backup.object_cache;
        self.object_generation = self.object_generation.wrapping_add(1);
        self.last_brush_point = None;
        self.mask_drag = None;
        self.mask_interaction_dirty_layer = None;
        self.mask_interaction_last_upload = None;
        self.mask_interaction_has_uncommitted_change = false;
        if refinement_restored {
            self.mark_all_mask_layers_dirty();
        } else if restored {
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
        self.active_mask_tool = (kind.is_available() && kind != MaskKind::Fullscreen).then_some(kind);
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_touch_gesture_backup = None;
        if !matches!(kind, MaskKind::Subject | MaskKind::Background) {
            self.subject_refinement_active = false;
        }
        if matches!(kind, MaskKind::Brush | MaskKind::Object) {
            self.brush_mode = BrushMode::Paint;
        }
    }

    pub(crate) fn select_mask_tool(&mut self, kind: MaskKind) {
        self.finish_mask_geometry_interaction();
        self.active_mask_tool = (kind.is_available() && kind != MaskKind::Fullscreen).then_some(kind);
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_touch_gesture_backup = None;
        if !matches!(kind, MaskKind::Subject | MaskKind::Background) {
            self.subject_refinement_active = false;
        }
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
            || self.landscape_task_id.is_some()
            || self.subject_receiver.is_some()
            || self.object_receiver.is_some()
            || self.landscape_receiver.is_some()
            || self.subject_consent_open
            || self.object_consent_open
            || self.landscape_consent_open
    }

    pub(in crate::app) fn recover_terminal_ai_mask_task_owners(&mut self) {
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

        let stale_landscape = self.landscape_receiver.is_none()
            && self.landscape_task_id.is_some_and(|id| {
                self.background_tasks
                    .snapshot(id)
                    .is_none_or(|task| task.status == TaskStatus::Failed)
            });
        if stale_landscape {
            if let Some(id) = self.landscape_task_id {
                self.clear_ai_mask_task_owner(id);
            }
        }
    }

    pub(crate) fn ai_masks_need_update(&self) -> bool {
        self.ai_masks_need_update && !self.masks.masks.is_empty()
    }

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
        let current_landscape = usize::from(
            self.landscape_receiver.is_some() || self.landscape_pending_target.is_some(),
        );
        let subject_remaining = usize::from(self.ai_mask_update_subject_pending) * subject_targets;
        subject_remaining
            + self.ai_mask_update_object_queue.len()
            + current_object
            + self.ai_mask_update_landscape_queue.len()
            + current_landscape
    }

    pub(in crate::app) fn generated_ai_mask_targets(&self) -> GeneratedAiMaskTargets {
        let mut subject = false;
        let mut objects = VecDeque::new();
        let mut landscapes = VecDeque::new();
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
                    (MaskKind::Landscape, MaskGeometry::Landscape { .. }) => {
                        landscapes.push_back((mask_index, component_index));
                    }
                    _ => {}
                }
            }
        }
        (subject, objects, landscapes)
    }

    pub(in crate::app) fn has_range_mask_targets(&self) -> bool {
        self.masks.masks.iter().any(|mask| {
            mask.components.iter().any(|component| {
                matches!(
                    &component.geometry,
                    MaskGeometry::LuminanceRange { .. } | MaskGeometry::ColorRange { .. }
                )
            })
        })
    }

    pub(in crate::app) fn invalidate_generated_mask_sources(&mut self) {
        self.mask_source_cache = None;
        self.subject_mask_cache = None;
        self.object_cache = None;
        self.landscape_generation = self.landscape_generation.wrapping_add(1);
        self.landscape_pending_target = None;
        self.subject_generation = self.subject_generation.wrapping_add(1);
        self.object_generation = self.object_generation.wrapping_add(1);
        self.object_pending_target = None;
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        self.ai_mask_update_landscape_queue.clear();
        self.ai_mask_update_failed = false;
    }

    pub(crate) fn note_inpainting_changed_for_ai_masks(&mut self) {
        let (has_subject, object_targets, landscape_targets) = self.generated_ai_mask_targets();
        let has_ranges = self.has_range_mask_targets();
        self.invalidate_generated_mask_sources();
        self.ai_masks_need_update = has_subject
            || !object_targets.is_empty()
            || !landscape_targets.is_empty()
            || has_ranges;
    }

    pub(crate) fn note_lens_correction_changed_for_masks(&mut self) {
        let (has_subject, object_targets, landscape_targets) = self.generated_ai_mask_targets();
        let has_ranges = self.has_range_mask_targets();
        self.invalidate_generated_mask_sources();
        // Manual/geometric masks remain intact and are immediately reused.
        // Only source-dependent masks need regeneration against the newly
        // corrected (or uncorrected) image geometry.
        self.ai_masks_need_update = has_subject
            || !object_targets.is_empty()
            || !landscape_targets.is_empty()
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

    pub(in crate::app) fn ai_runtime_ready(&mut self) -> bool {
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
        let (update_subject, object_targets, landscape_targets) =
            self.generated_ai_mask_targets();
        let update_ranges = self.has_range_mask_targets();
        if self.masks.masks.is_empty() {
            self.ai_masks_need_update = false;
            return;
        }
        #[cfg(not(target_os = "android"))]
        if (update_subject || !object_targets.is_empty() || !landscape_targets.is_empty())
            && !self.validate_onnx_runtime_for_ai()
        {
            return;
        }

        if update_subject
            || !object_targets.is_empty()
            || !landscape_targets.is_empty()
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

        if !update_subject && object_targets.is_empty() && landscape_targets.is_empty() {
            self.ai_masks_need_update = false;
            self.notice = Some("Masks were refreshed for the current image geometry.".to_owned());
            self.egui_ctx.request_repaint();
            return;
        }

        self.ai_mask_update_active = true;
        self.ai_mask_update_subject_pending = update_subject;
        self.ai_mask_update_object_queue = object_targets;
        self.ai_mask_update_landscape_queue = landscape_targets;
        self.ai_mask_update_failed = false;

        if update_subject {
            let path = self.birefnet_model_path();
            if path.is_file() {
                self.start_subject_worker(path);
            } else {
                self.subject_consent_open = true;
                self.egui_ctx.request_repaint();
            }
        } else {
            self.continue_ai_mask_update();
        }
    }

    pub(in crate::app) fn continue_ai_mask_update(&mut self) {
        if !self.ai_mask_update_active
            || self.ai_mask_update_subject_pending
            || self.subject_receiver.is_some()
            || self.object_receiver.is_some()
            || self.landscape_receiver.is_some()
            || self.subject_consent_open
            || self.object_consent_open
            || self.landscape_consent_open
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
            self.ai_mask_update_landscape_queue.pop_front()
        {
            let valid = self
                .masks
                .masks
                .get(mask_index)
                .and_then(|mask| mask.components.get(component_index))
                .is_some_and(|component| {
                    matches!(
                        (component.kind, &component.geometry),
                        (MaskKind::Landscape, MaskGeometry::Landscape { .. })
                    )
                });
            if !valid {
                continue;
            }
            let path = self.landscape_model_path();
            if crate::ai_masks::landscape_model_is_verified(&path)
                && crate::ai_masks::vitmatte_model_is_verified(&self.vitmatte_model_path())
            {
                self.start_landscape_worker(mask_index, component_index, path, false);
            } else {
                self.landscape_pending_target = Some((mask_index, component_index));
                self.landscape_consent_open = true;
                self.egui_ctx.request_repaint();
            }
            return;
        }

        self.finish_ai_mask_update();
    }

    pub(in crate::app) fn finish_ai_mask_update(&mut self) {
        if !self.ai_mask_update_active {
            return;
        }
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        self.ai_mask_update_landscape_queue.clear();
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

    pub(in crate::app) fn cancel_ai_mask_update(&mut self) {
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        self.ai_mask_update_landscape_queue.clear();
        self.ai_mask_update_failed = false;
        self.object_pending_target = None;
        self.landscape_pending_target = None;
        self.ai_masks_need_update = true;
        self.notice = Some("AI-mask update canceled.".to_owned());
        self.egui_ctx.request_repaint();
    }
}
