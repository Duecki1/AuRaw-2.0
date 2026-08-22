use super::*;

impl MaskState {
    pub(in crate::app) fn capture_ai_target(
        &self,
        mask_index: usize,
        component_index: usize,
    ) -> Option<AiMaskTarget> {
        let component = self
            .stack
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

    pub(in crate::app) fn resolve_ai_target(
        &self,
        target: &AiMaskTarget,
    ) -> std::result::Result<(usize, usize), String> {
        Self::resolve_ai_target_in_stack(&self.stack, target)
    }

    pub(in crate::app) fn resolve_ai_target_in_stack(
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
                    Some(mask) if mask.components[target.component_index].kind != target.kind => {
                        "The target component changed type before inference completed."
                    }
                    Some(_) => "The target component changed before inference completed.",
                };
                Err(message.to_owned())
            }
            _ => Err(
                "The target component is ambiguous after editing; the stale result was discarded."
                    .to_owned(),
            ),
        }
    }

    fn mark_geometry_dirty(&mut self, layer: usize) {
        if layer < MAX_LOCAL_MASKS {
            self.dirty_layers[layer] = true;
            self.detail_dirty_layers[layer] = true;
            self.navigation_dirty_layers[layer] = true;
        }
        self.overlay_revision = self.overlay_revision.wrapping_add(1);
    }

    fn mark_all_layers_dirty(&mut self) {
        self.dirty_layers.fill(true);
        self.detail_dirty_layers.fill(true);
        self.navigation_dirty_layers.fill(true);
        self.overlay_revision = self.overlay_revision.wrapping_add(1);
    }
}

impl AurawApp {
    pub(crate) fn mark_mask_adjustments_dirty(&mut self) {
        self.note_mask_edit_changed();
        if self.preview.gpu_pipeline.is_none() {
            return;
        }
        self.queue_preview_processing(ProcessingStage::Output);
    }

    pub(crate) fn mark_mask_geometry_dirty(&mut self, layer: usize) {
        self.masks.mark_geometry_dirty(layer);
        self.mark_mask_adjustments_dirty();
    }

    pub(crate) fn note_mask_geometry_interaction(&mut self, layer: usize) {
        const INTERACTIVE_MASK_INTERVAL: Duration = Duration::from_millis(45);

        if self.masks.interaction_dirty_layer != Some(layer) {
            self.finish_mask_geometry_interaction();
            self.masks.interaction_dirty_layer = Some(layer);
            self.masks.interaction_last_upload = None;
        }

        self.masks.interaction_has_uncommitted_change = true;
        let now = Instant::now();
        let upload_due = self.masks.interaction_last_upload
            .is_none_or(|last| now.duration_since(last) >= INTERACTIVE_MASK_INTERVAL);
        if upload_due {
            self.mark_mask_geometry_dirty(layer);
            self.masks.interaction_last_upload = Some(now);
            self.masks.interaction_has_uncommitted_change = false;
        }
    }

    pub(crate) fn note_subject_refinement_interaction(&mut self) {
        const INTERACTIVE_MASK_INTERVAL: Duration = Duration::from_millis(45);
        const SHARED_REFINEMENT_LAYER: usize = MAX_LOCAL_MASKS;

        if self.masks.interaction_dirty_layer != Some(SHARED_REFINEMENT_LAYER) {
            self.finish_mask_geometry_interaction();
            self.masks.interaction_dirty_layer = Some(SHARED_REFINEMENT_LAYER);
            self.masks.interaction_last_upload = None;
        }

        self.masks.interaction_has_uncommitted_change = true;
        let now = Instant::now();
        let upload_due = self.masks.interaction_last_upload
            .is_none_or(|last| now.duration_since(last) >= INTERACTIVE_MASK_INTERVAL);
        if upload_due {
            self.mark_all_mask_layers_dirty();
            self.masks.interaction_last_upload = Some(now);
            self.masks.interaction_has_uncommitted_change = false;
        }
    }

    pub(crate) fn finish_mask_geometry_interaction(&mut self) {
        let layer = self.masks.interaction_dirty_layer.take();
        let should_commit = self.masks.interaction_has_uncommitted_change;
        self.masks.interaction_last_upload = None;
        self.masks.interaction_has_uncommitted_change = false;
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
        if self.masks.touch_gesture_backup.is_some() {
            return;
        }
        let Some(geometry) = self
            .masks
            .stack
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .map(|component| component.geometry.clone())
        else {
            return;
        };
        self.masks.touch_gesture_backup = Some(MaskTouchGestureBackup {
            mask_index,
            component_index,
            geometry,
            subject_refinement: self.masks.subject_refinement_active
                .then(|| self.masks.stack.subject_refinement.clone()),
            object_cache: self.ai.object_cache.clone(),
        });
    }

    pub(crate) fn commit_mask_touch_gesture(&mut self) {
        self.masks.touch_gesture_backup = None;
    }

    pub(crate) fn cancel_mask_touch_gesture(&mut self) {
        let Some(backup) = self.masks.touch_gesture_backup.take() else {
            self.masks.last_brush_point = None;
            self.masks.drag = None;
            return;
        };
        let restored = self
            .masks
            .stack
            .masks
            .get_mut(backup.mask_index)
            .and_then(|mask| mask.components.get_mut(backup.component_index))
            .is_some_and(|component| {
                component.geometry = backup.geometry;
                true
            });
        let refinement_restored = if let Some(subject_refinement) = backup.subject_refinement {
            self.masks.stack.subject_refinement = subject_refinement;
            true
        } else {
            false
        };
        self.ai.object_cache = backup.object_cache;
        self.cancel_foreground_operation_if(ForegroundOperationKind::ObjectMask);
        self.masks.last_brush_point = None;
        self.masks.drag = None;
        self.masks.interaction_dirty_layer = None;
        self.masks.interaction_last_upload = None;
        self.masks.interaction_has_uncommitted_change = false;
        if refinement_restored {
            self.mark_all_mask_layers_dirty();
        } else if restored {
            self.mark_mask_geometry_dirty(backup.mask_index);
        }
    }

    pub(crate) fn mark_all_mask_layers_dirty(&mut self) {
        self.masks.mark_all_layers_dirty();
        self.mark_mask_adjustments_dirty();
    }

    pub(crate) fn sync_selected_mask_tool(&mut self) {
        self.masks.thumbnail_component_mask = None;
        let kind = self.masks.stack.selected_component().map(|component| component.kind);
        if let Some(kind) = kind {
            self.select_mask_tool(kind);
        } else {
            self.masks.active_tool = None;
        }
    }

    pub(crate) fn activate_mask_tool(&mut self, kind: MaskKind) {
        self.finish_mask_geometry_interaction();
        self.masks.active_tool = (kind.is_available() && kind != MaskKind::Fullscreen).then_some(kind);
        self.masks.drag = None;
        self.masks.last_brush_point = None;
        self.masks.touch_gesture_backup = None;
        if !matches!(kind, MaskKind::Subject | MaskKind::Background) {
            self.masks.subject_refinement_active = false;
        }
        if matches!(kind, MaskKind::Brush | MaskKind::Object) {
            self.masks.brush_mode = BrushMode::Paint;
        }
    }

    pub(crate) fn select_mask_tool(&mut self, kind: MaskKind) {
        self.finish_mask_geometry_interaction();
        self.masks.active_tool = (kind.is_available() && kind != MaskKind::Fullscreen).then_some(kind);
        self.masks.drag = None;
        self.masks.last_brush_point = None;
        self.masks.touch_gesture_backup = None;
        if !matches!(kind, MaskKind::Subject | MaskKind::Background) {
            self.masks.subject_refinement_active = false;
        }
    }

    pub(crate) fn blink_selected_mask(&mut self) {
        self.masks.overlay_blink = Some((std::time::Instant::now(), MaskOverlayBlink::GroupTwice));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn blink_selected_component(&mut self) {
        self.masks.overlay_blink = Some((
            std::time::Instant::now(),
            MaskOverlayBlink::ComponentThenGroup,
        ));
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn ai_mask_update_busy(&self) -> bool {
        self.ai.mask_update_active
            || matches!(
                self.foreground_operation_kind(),
                Some(
                    ForegroundOperationKind::SubjectMask
                        | ForegroundOperationKind::ObjectMask
                )
            )
            || self.ai.subject_consent_open
            || self.ai.object_consent_open
    }

    pub(crate) fn ai_masks_need_update(&self) -> bool {
        self.ai.masks_need_update && !self.masks.stack.masks.is_empty()
    }

    pub(crate) fn ai_mask_update_remaining_target_count(&self) -> usize {
        if !self.ai.mask_update_active {
            return 0;
        }

        let subject_targets = self.masks.stack
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
            self.foreground_operation_is(ForegroundOperationKind::ObjectMask)
                || self.ai.object_pending_target.is_some(),
        );
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

    pub(in crate::app) fn has_range_mask_targets(&self) -> bool {
        self.masks.stack.masks.iter().any(|mask| {
            mask.components.iter().any(|component| {
                matches!(
                    &component.geometry,
                    MaskGeometry::LuminanceRange { .. } | MaskGeometry::ColorRange { .. }
                )
            })
        })
    }

    pub(in crate::app) fn invalidate_generated_mask_sources(&mut self) {
        self.masks.source_cache = None;
        self.masks.subject_cache = None;
        self.ai.object_cache = None;
        if matches!(
            self.foreground_operation_kind(),
            Some(
                ForegroundOperationKind::SubjectMask
                    | ForegroundOperationKind::ObjectMask
            )
        ) {
            self.cancel_foreground_operation();
        }
        self.ai.object_pending_target = None;
        self.ai.mask_update_active = false;
        self.ai.mask_update_subject_pending = false;
        self.ai.mask_update_object_queue.clear();
        self.ai.mask_update_failed = false;
    }

    pub(crate) fn note_lens_correction_changed_for_masks(&mut self) {
        let has_ranges = self.has_range_mask_targets();
        self.invalidate_generated_mask_sources();
        // Manual/geometric masks remain intact and are immediately reused.
        // Only source-dependent masks need regeneration against the newly
        // corrected (or uncorrected) image geometry.
        self.ai.masks_need_update = has_subject
            || !object_targets.is_empty()
            || has_ranges;
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn validate_onnx_runtime_for_ai(&mut self) -> bool {
        let (Some(runtime_path), Some(runtime_sha256)) = (
            self.ai.runtime_path.clone(),
            self.ai.runtime_sha256.clone(),
        ) else {
            self.ui.notice = Some(
                "Choose an ONNX Runtime library under Settings before using desktop AI tools."
                    .to_owned(),
            );
            return false;
        };
        match crate::ai_masks::probe_runtime_subprocess(&runtime_path, &runtime_sha256) {
            Ok(()) => true,
            Err(error) => {
                self.ui.notice = Some(format!(
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
        if self.ai_mask_update_busy() {
            self.ui.notice = Some("Wait for the current AI mask operation to finish.".to_owned());
            return;
        }
            self.generated_ai_mask_targets();
        let update_ranges = self.has_range_mask_targets();
        if self.masks.stack.masks.is_empty() {
            self.ai.masks_need_update = false;
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
            // Force a new canonical source because lens correction changed
            // the image under content-aware masks.
            self.masks.source_cache = None;
            self.masks.subject_cache = None;
            self.ai.object_cache = None;
            if let Err(error) = self.capture_mask_source(frame) {
                self.ui.notice = Some(error);
                return;
            }

            if update_ranges {
                let source = self.masks.source_cache.clone();
                let mut range_layers_changed = Vec::new();
                for (mask_index, mask) in self.masks.stack.masks.iter_mut().enumerate() {
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

            self.ai.masks_need_update = false;
            self.ui.notice = Some("Masks were refreshed for the current image geometry.".to_owned());
            self.egui_ctx.request_repaint();
            return;
        }

        self.ai.mask_update_active = true;
        self.ai.mask_update_subject_pending = update_subject;
        self.ai.mask_update_object_queue = object_targets;
        self.ai.mask_update_failed = false;

        if update_subject {
            let path = self.birefnet_model_path();
            if crate::ai_masks::birefnet_model_is_verified(self.ai.birefnet_quality, &path) {
                self.start_subject_worker(path, false);
            } else {
                self.ai.subject_consent_open = true;
                self.egui_ctx.request_repaint();
            }
        } else {
            self.continue_ai_mask_update();
        }
    }

    pub(in crate::app) fn continue_ai_mask_update(&mut self) {
        if !self.ai.mask_update_active
            || self.ai.mask_update_subject_pending
            || matches!(
                self.foreground_operation_kind(),
                Some(
                    ForegroundOperationKind::SubjectMask
                        | ForegroundOperationKind::ObjectMask
                )
            )
            || self.ai.subject_consent_open
            || self.ai.object_consent_open
        {
            return;
        }

        while let Some((mask_index, component_index)) =
            self.ai.mask_update_object_queue.pop_front()
        {
            let valid = self.masks.stack
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
            let vitmatte = self.vitmatte_model_path();
            if crate::ai_masks::object_models_are_verified(&encoder, &decoder, &vitmatte) {
                self.start_object_worker(mask_index, component_index, encoder, decoder, false);
            } else {
                self.ai.object_pending_target = Some((mask_index, component_index));
                self.ai.object_consent_open = true;
                self.egui_ctx.request_repaint();
            }
            return;
        }

        while let Some((mask_index, component_index)) =
        {
            let valid = self.masks.stack
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
                && crate::ai_masks::vitmatte_model_is_verified(&self.vitmatte_model_path())
            {
            } else {
                self.egui_ctx.request_repaint();
            }
            return;
        }

        self.finish_ai_mask_update();
    }

    pub(in crate::app) fn finish_ai_mask_update(&mut self) {
        if !self.ai.mask_update_active {
            return;
        }
        self.ai.mask_update_active = false;
        self.ai.mask_update_subject_pending = false;
        self.ai.mask_update_object_queue.clear();
        if self.ai.mask_update_failed {
            self.ai.masks_need_update = true;
            self.ui.notice = Some(
                "Some AI masks could not be updated. The update button will remain available."
                    .to_owned(),
            );
        } else {
            self.ai.masks_need_update = false;
            self.ui.notice = Some("Masks were refreshed for the current image geometry.".to_owned());
        }
        self.egui_ctx.request_repaint();
    }

    pub(in crate::app) fn cancel_ai_mask_update(&mut self) {
        self.ai.mask_update_active = false;
        self.ai.mask_update_subject_pending = false;
        self.ai.mask_update_object_queue.clear();
        self.ai.mask_update_failed = false;
        self.ai.object_pending_target = None;
        self.ai.masks_need_update = true;
        self.ui.notice = Some("AI-mask update canceled.".to_owned());
        self.egui_ctx.request_repaint();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mask_state() -> MaskState {
        MaskState {
            stack: MaskStack::default(),
            active_tool: None,
            brush_mode: BrushMode::default(),
            subject_refinement_active: false,
            drag: None,
            last_brush_point: None,
            touch_gesture_backup: None,
            interaction_dirty_layer: None,
            interaction_last_upload: None,
            interaction_has_uncommitted_change: false,
            overlay_revision: 10,
            overlay_texture: None,
            overlay_texture_key: None,
            overlay_blink: None,
            thumbnail_revision: 0,
            thumbnail_group_textures: Vec::new(),
            thumbnail_component_mask: None,
            thumbnail_component_textures: Vec::new(),
            source_cache: None,
            subject_cache: None,
            dirty_layers: [false; MAX_LOCAL_MASKS],
            detail_dirty_layers: [false; MAX_LOCAL_MASKS],
            navigation_dirty_layers: [false; MAX_LOCAL_MASKS],
        }
    }

    #[test]
    fn geometry_invalidation_marks_every_preview_for_the_layer() {
        let mut state = mask_state();
        state.mark_geometry_dirty(3);
        assert!(state.dirty_layers[3]);
        assert!(state.detail_dirty_layers[3]);
        assert!(state.navigation_dirty_layers[3]);
        assert_eq!(state.overlay_revision, 11);
        assert!(!state.dirty_layers[2]);
    }

    #[test]
    fn full_invalidation_marks_all_mask_previews() {
        let mut state = mask_state();
        state.mark_all_layers_dirty();
        assert!(state.dirty_layers.iter().all(|dirty| *dirty));
        assert!(state.detail_dirty_layers.iter().all(|dirty| *dirty));
        assert!(state.navigation_dirty_layers.iter().all(|dirty| *dirty));
        assert_eq!(state.overlay_revision, 11);
    }
}
