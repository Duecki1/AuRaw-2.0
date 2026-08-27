use super::{needs_canonical_mask_source, AppTab, AurawApp, LensCorrectionState};
use crate::pipeline::{ExposureParams, MaskGeometry, MaskStack, ProcessingStage, RemoveEditState};
use eframe::egui;
use std::collections::VecDeque;
use std::sync::Arc;

const EDIT_HISTORY_LIMIT: usize = if cfg!(target_os = "android") { 32 } else { 64 };

#[derive(Clone, Debug, PartialEq, Eq)]
struct LensEditState {
    enabled: bool,
    selected_maker: String,
    selected_model: String,
}

impl LensEditState {
    fn capture(lens: &LensCorrectionState) -> Self {
        Self {
            enabled: lens.enabled,
            selected_maker: lens.selected_maker.clone(),
            selected_model: lens.selected_model.clone(),
        }
    }

    fn matches(&self, lens: &LensCorrectionState) -> bool {
        self.enabled == lens.enabled
            && self.selected_maker == lens.selected_maker
            && self.selected_model == lens.selected_model
    }

    fn apply_to(&self, lens: &mut LensCorrectionState) {
        lens.enabled = self.enabled;
        lens.selected_maker.clone_from(&self.selected_maker);
        lens.selected_model.clone_from(&self.selected_model);
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct MaskSelection {
    mask: Option<usize>,
    component: Option<usize>,
}

impl MaskSelection {
    fn capture(navigation: &MaskStack, contents: &MaskStack) -> Self {
        let mask = navigation
            .selected_mask
            .filter(|index| *index < contents.masks.len());
        let component = mask.and_then(|mask_index| {
            navigation.selected_component.filter(|component_index| {
                *component_index < contents.masks[mask_index].components.len()
            })
        });
        Self { mask, component }
    }

    fn updated_from(self, navigation: &MaskStack, contents: &MaskStack) -> Self {
        let Some(mask) = navigation.selected_mask else {
            return Self::default();
        };
        let Some(mask_contents) = contents.masks.get(mask) else {
            return self;
        };
        let component = match navigation.selected_component {
            Some(component) if component < mask_contents.components.len() => Some(component),
            Some(_) if self.mask == Some(mask) => self.component,
            Some(_) | None => None,
        };
        Self {
            mask: Some(mask),
            component,
        }
    }

    fn apply_to(self, masks: &mut MaskStack) {
        masks.selected_mask = self.mask;
        masks.selected_component = self.component;
    }
}

#[derive(Clone, Debug)]
struct EditSnapshot {
    exposure: ExposureParams,
    masks: Arc<MaskStack>,
    mask_selection: MaskSelection,
    lens: LensEditState,
    remove: Arc<RemoveEditState>,
}

impl EditSnapshot {
    fn capture(exposure: &ExposureParams, masks: &MaskStack, lens: &LensCorrectionState) -> Self {
        let mut contents = masks.clone();
        contents.selected_mask = None;
        contents.selected_component = None;
        let contents = Arc::new(contents);
        Self {
            exposure: *exposure,
            mask_selection: MaskSelection::capture(masks, &contents),
            masks: contents,
            lens: LensEditState::capture(lens),
            remove: Arc::new(RemoveEditState::default()),
        }
    }

    fn capture_successor(
        &self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        mask_contents_match: bool,
    ) -> Self {
        let contents = if mask_contents_match {
            Arc::clone(&self.masks)
        } else {
            let mut contents = masks.clone();
            contents.selected_mask = None;
            contents.selected_component = None;
            Arc::new(contents)
        };
        Self {
            exposure: *exposure,
            mask_selection: MaskSelection::capture(masks, &contents),
            masks: contents,
            lens: LensEditState::capture(lens),
            remove: Arc::clone(&self.remove),
        }
    }

    fn remember_selection(&mut self, masks: &MaskStack) {
        self.mask_selection = self.mask_selection.updated_from(masks, &self.masks);
    }

    fn materialize_masks(&self) -> MaskStack {
        let mut masks = (*self.masks).clone();
        self.mask_selection.apply_to(&mut masks);
        masks
    }
}

pub(crate) struct EditHistory {
    undo: VecDeque<EditSnapshot>,
    redo: VecDeque<EditSnapshot>,
    current: EditSnapshot,
    interaction_pending: bool,
    mask_interaction_pending: bool,
    change_observed: bool,
    mask_change_observed: bool,
    restoring_snapshot: bool,
    committed_revision: u64,
}

impl EditHistory {
    pub(super) fn new(
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            current: EditSnapshot::capture(exposure, masks, lens),
            interaction_pending: false,
            mask_interaction_pending: false,
            change_observed: false,
            mask_change_observed: false,
            restoring_snapshot: false,
            committed_revision: 0,
        }
    }

    fn push_bounded(history: &mut VecDeque<EditSnapshot>, snapshot: EditSnapshot) {
        if history.len() == EDIT_HISTORY_LIMIT {
            history.pop_front();
        }
        history.push_back(snapshot);
    }

    pub(super) fn reset(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        remove: &Arc<RemoveEditState>,
    ) {
        self.undo.clear();
        self.redo.clear();
        self.current = EditSnapshot::capture(exposure, masks, lens);
        self.current.remove = Arc::clone(remove);
        self.interaction_pending = false;
        self.mask_interaction_pending = false;
        self.change_observed = false;
        self.mask_change_observed = false;
        self.restoring_snapshot = false;
    }

    fn note_change(&mut self) {
        if !self.restoring_snapshot {
            self.change_observed = true;
        }
    }

    fn note_mask_change(&mut self) {
        if !self.restoring_snapshot {
            self.change_observed = true;
            self.mask_change_observed = true;
        }
    }

    fn set_restoring_snapshot(&mut self, restoring: bool) {
        self.restoring_snapshot = restoring;
    }

    pub(super) fn observe(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        interaction_active: bool,
    ) {
        self.current.remember_selection(masks);
        if self.mask_change_observed {
            self.mask_interaction_pending = true;
            self.mask_change_observed = false;
        }
        if self.change_observed {
            self.interaction_pending = true;
            self.change_observed = false;
        }
        if !self.interaction_pending || interaction_active {
            return;
        }
        self.commit_current_state(exposure, masks, lens);
    }

    fn commit_current_state(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) {
        let mask_change_pending = self.mask_interaction_pending || self.mask_change_observed;
        self.change_observed = false;
        self.mask_change_observed = false;
        let mask_contents_match = if self.current.masks.masks.len() != masks.masks.len() {
            false
        } else if mask_change_pending {
            self.current.masks.masks == masks.masks
        } else {
            true
        };
        if self.current.exposure == *exposure
            && mask_contents_match
            && self.current.lens.matches(lens)
        {
            self.current.remember_selection(masks);
            self.interaction_pending = false;
            self.mask_interaction_pending = false;
            return;
        }

        let next = self
            .current
            .capture_successor(exposure, masks, lens, mask_contents_match);
        let previous = std::mem::replace(&mut self.current, next);
        Self::push_bounded(&mut self.undo, previous);
        self.redo.clear();
        self.interaction_pending = false;
        self.mask_interaction_pending = false;
        self.committed_revision = self.committed_revision.wrapping_add(1);
    }

    pub(super) fn commit_remove_state(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        remove: &Arc<RemoveEditState>,
    ) {
        self.commit_current_state(exposure, masks, lens);
        if Arc::ptr_eq(&self.current.remove, remove) {
            return;
        }
        let mut next = self.current.capture_successor(exposure, masks, lens, true);
        next.remove = Arc::clone(remove);
        let previous = std::mem::replace(&mut self.current, next);
        Self::push_bounded(&mut self.undo, previous);
        self.redo.clear();
        self.committed_revision = self.committed_revision.wrapping_add(1);
    }

    pub(super) fn can_undo(&self) -> bool {
        !self.undo.is_empty()
            || self.interaction_pending
            || self.mask_interaction_pending
            || self.change_observed
            || self.mask_change_observed
    }

    pub(super) fn can_redo(&self) -> bool {
        !self.interaction_pending
            && !self.mask_interaction_pending
            && !self.change_observed
            && !self.mask_change_observed
            && !self.redo.is_empty()
    }

    fn undo(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) -> Option<(EditSnapshot, bool, bool)> {
        self.commit_current_state(exposure, masks, lens);
        let target = self.undo.pop_back()?;
        let masks_changed = !Arc::ptr_eq(&target.masks, &self.current.masks);
        let remove_changed = !Arc::ptr_eq(&target.remove, &self.current.remove);
        let present = std::mem::replace(&mut self.current, target.clone());
        Self::push_bounded(&mut self.redo, present);
        self.committed_revision = self.committed_revision.wrapping_add(1);
        Some((target, masks_changed, remove_changed))
    }

    fn redo(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) -> Option<(EditSnapshot, bool, bool)> {
        self.commit_current_state(exposure, masks, lens);
        let target = self.redo.pop_back()?;
        let masks_changed = !Arc::ptr_eq(&target.masks, &self.current.masks);
        let remove_changed = !Arc::ptr_eq(&target.remove, &self.current.remove);
        let present = std::mem::replace(&mut self.current, target.clone());
        Self::push_bounded(&mut self.undo, present);
        self.committed_revision = self.committed_revision.wrapping_add(1);
        Some((target, masks_changed, remove_changed))
    }

    fn committed_revision(&self) -> u64 {
        self.committed_revision
    }

    fn committed_masks(&self) -> Arc<MaskStack> {
        Arc::clone(&self.current.masks)
    }

    fn committed_remove(&self) -> Arc<RemoveEditState> {
        Arc::clone(&self.current.remove)
    }
}

impl AurawApp {
    pub(crate) fn note_edit_changed(&mut self) {
        self.persistence.history.note_change();
    }

    pub(crate) fn note_remove_edit_changed(&mut self) {
        self.persistence.history.commit_remove_state(
            &self.develop.exposure,
            &self.masks.stack,
            &self.develop.lens_correction,
            &self.inpaint.edits,
        );
        self.queue_preview_processing(ProcessingStage::Raw);
    }

    pub(crate) fn note_geometry_changed(&mut self) {
        self.develop.geometry = self.develop.geometry.sanitized();
        self.develop.geometry_revision = self.develop.geometry_revision.wrapping_add(1);
    }

    pub(crate) fn note_mask_edit_changed(&mut self) {
        self.persistence.history.note_mask_change();
    }

    pub(crate) fn edit_commit_revision(&self) -> u64 {
        self.persistence
            .history
            .committed_revision()
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ self.develop.geometry_revision.rotate_left(17)
    }

    pub(crate) fn committed_mask_state_for_persistence(&self) -> Arc<MaskStack> {
        self.persistence.history.committed_masks()
    }

    pub(crate) fn committed_remove_state_for_persistence(&self) -> Arc<RemoveEditState> {
        self.persistence.history.committed_remove()
    }

    pub(crate) fn reset_edit_history(&mut self) {
        self.persistence.lens_restore_masks = None;
        self.persistence.history.reset(
            &self.develop.exposure,
            &self.masks.stack,
            &self.develop.lens_correction,
            &self.inpaint.edits,
        );
    }

    pub(crate) fn observe_edit_history(&mut self, ctx: &egui::Context) {
        let interaction_active = ctx.input(|input| input.pointer.any_down());
        self.persistence.history.observe(
            &self.develop.exposure,
            &self.masks.stack,
            &self.develop.lens_correction,
            interaction_active,
        );
    }

    pub(crate) fn commit_edit_history_now(&mut self) {
        self.finish_mask_geometry_interaction();
        self.persistence.history.observe(
            &self.develop.exposure,
            &self.masks.stack,
            &self.develop.lens_correction,
            false,
        );
    }

    pub(crate) fn can_undo_edit(&self) -> bool {
        self.develop.loaded_raw.is_some() && self.persistence.history.can_undo()
    }

    pub(crate) fn can_redo_edit(&self) -> bool {
        self.develop.loaded_raw.is_some() && self.persistence.history.can_redo()
    }

    pub(crate) fn undo_edit(&mut self) {
        self.finish_mask_geometry_interaction();
        let snapshot = self.persistence.history.undo(
            &self.develop.exposure,
            &self.masks.stack,
            &self.develop.lens_correction,
        );
        if let Some((snapshot, masks_changed, remove_changed)) = snapshot {
            self.apply_edit_snapshot(snapshot, masks_changed, remove_changed);
            self.ui.notice = Some("Undid edit.".to_owned());
        }
    }

    pub(crate) fn redo_edit(&mut self) {
        self.finish_mask_geometry_interaction();
        let snapshot = self.persistence.history.redo(
            &self.develop.exposure,
            &self.masks.stack,
            &self.develop.lens_correction,
        );
        if let Some((snapshot, masks_changed, remove_changed)) = snapshot {
            self.apply_edit_snapshot(snapshot, masks_changed, remove_changed);
            self.ui.notice = Some("Redid edit.".to_owned());
        }
    }

    pub(crate) fn handle_edit_history_shortcuts(&mut self, ctx: &egui::Context) {
        if self.ui.active_tab != AppTab::Develop {
            return;
        }
        let redo_shift_z = egui::KeyboardShortcut::new(
            egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
            egui::Key::Z,
        );
        let redo_y = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y);
        let undo = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z);

        let redo_requested = self.can_redo_edit()
            && (ctx.input_mut(|input| input.consume_shortcut(&redo_shift_z))
                || ctx.input_mut(|input| input.consume_shortcut(&redo_y)));
        if redo_requested {
            self.redo_edit();
        } else if self.can_undo_edit() && ctx.input_mut(|input| input.consume_shortcut(&undo)) {
            self.undo_edit();
        }
    }

    fn apply_edit_snapshot(
        &mut self,
        snapshot: EditSnapshot,
        masks_changed: bool,
        remove_changed: bool,
    ) {
        let lens_changed = !snapshot.lens.matches(&self.develop.lens_correction);
        let ai_denoise_changed =
            snapshot.exposure.ai_denoise_enabled != self.develop.exposure.ai_denoise_enabled;
        self.cancel_document_bound_foreground_operation();

        self.persistence.history.set_restoring_snapshot(true);
        self.develop.exposure = snapshot.exposure;
        self.develop.exposure.sanitize_tone_curves();
        if masks_changed {
            self.masks.stack = snapshot.materialize_masks();
        } else {
            snapshot.mask_selection.apply_to(&mut self.masks.stack);
        }
        if remove_changed {
            if let Some(cancellation) = self.inpaint.cancellation.take() {
                cancellation.store(true, std::sync::atomic::Ordering::Release);
            }
            self.inpaint.edits = Arc::clone(&snapshot.remove);
            self.inpaint.active_points.clear();
            self.inpaint.pending_brush = None;
            self.inpaint.model_consent_open = false;
            self.inpaint.receiver = None;
            self.inpaint.processing_label = None;
        }
        snapshot.lens.apply_to(&mut self.develop.lens_correction);
        self.rehydrate_restored_mask_state();
        if ai_denoise_changed && self.develop.exposure.ai_denoise_enabled {
            self.ai.denoise_resume_pending = true;
        }

        if lens_changed {
            self.persistence.lens_restore_masks = Some(std::mem::take(&mut self.masks.stack));
            self.mark_lens_correction_dirty();
        } else {
            if masks_changed {
                self.mark_all_mask_layers_dirty();
            }
            self.mark_pipeline_dirty();
            if remove_changed {
                self.queue_preview_processing(ProcessingStage::Raw);
            }
            if ai_denoise_changed && self.develop.exposure.ai_denoise_enabled {
                self.preview.quality_dirty = true;
                self.preview.detail = None;
                self.preview.navigation = None;
            }
        }
        self.persistence.history.set_restoring_snapshot(false);
    }

    pub(super) fn rehydrate_restored_mask_state(&mut self) {
        self.masks.active_tool = self
            .masks
            .stack
            .selected_component()
            .map(|component| component.kind)
            .filter(|kind| kind.is_available());
        self.masks.subject_refinement_active &= self.masks.active_tool.is_some_and(|kind| {
            matches!(
                kind,
                crate::pipeline::MaskKind::Subject | crate::pipeline::MaskKind::Background
            )
        });
        self.masks.drag = None;
        self.masks.last_brush_point = None;
        self.masks.touch_gesture_backup = None;
        self.masks.interaction_dirty_layer = None;
        self.masks.interaction_last_upload = None;
        self.masks.interaction_has_uncommitted_change = false;
        self.masks.overlay_revision = self.masks.overlay_revision.wrapping_add(1);
        self.masks.overlay_texture = None;
        self.masks.overlay_texture_key = None;
        self.masks.overlay_blink = None;
        self.masks.thumbnail_group_textures.clear();
        self.masks.thumbnail_component_mask = None;
        self.masks.thumbnail_component_textures.clear();
        self.masks.thumbnail_revision = self.masks.overlay_revision;

        let restored_source = self.masks.stack.masks.iter().find_map(|mask| {
            mask.components
                .iter()
                .find_map(|component| match &component.geometry {
                    MaskGeometry::LuminanceRange {
                        source: Some(source),
                        ..
                    }
                    | MaskGeometry::ColorRange {
                        source: Some(source),
                        ..
                    } => Some(source.clone()),
                    _ => None,
                })
        });
        if restored_source.is_some() || !needs_canonical_mask_source(&self.masks.stack) {
            self.masks.source_cache = restored_source;
        }
        self.masks.subject_cache = self.masks.stack.masks.iter().find_map(|mask| {
            mask.components
                .iter()
                .find_map(|component| match &component.geometry {
                    MaskGeometry::Ai {
                        mask: Some(mask), ..
                    } => Some(mask.clone()),
                    _ => None,
                })
        });
        self.ai.mask_update_active = false;
        self.ai.mask_update_subject_pending = false;
        self.ai.mask_update_object_queue.clear();
        self.ai.mask_update_failed = false;
        if self.ai.masks_need_update {
            let (subject, objects) = self.generated_ai_mask_targets();
            self.ai.masks_need_update =
                subject || !objects.is_empty() || self.has_range_mask_targets();
        }
        self.ai.subject_consent_open = false;
        self.ai.object_consent_open = false;
        self.ai.object_pending_target = None;
        self.ai.object_cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{MaskKind, RemoveEditState, RemoveStroke};

    fn state() -> (ExposureParams, MaskStack, LensCorrectionState) {
        (
            ExposureParams::scene_referred_default(),
            MaskStack::default(),
            LensCorrectionState::default(),
        )
    }

    #[test]
    fn interaction_changes_are_coalesced_into_one_step() {
        let (mut exposure, masks, lens) = state();
        let mut history = EditHistory::new(&exposure, &masks, &lens);

        exposure.exposure = 1.0;
        history.note_change();
        history.observe(&exposure, &masks, &lens, true);
        exposure.exposure = 2.0;
        history.note_change();
        history.observe(&exposure, &masks, &lens, true);
        history.observe(&exposure, &masks, &lens, false);

        let (restored, masks_changed, _remove_changed) =
            history.undo(&exposure, &masks, &lens).unwrap();
        assert!(!masks_changed);
        assert_eq!(restored.exposure.exposure, 0.0);
        assert!(history.undo.is_empty());

        exposure = restored.exposure;
        let (redone, masks_changed, _remove_changed) =
            history.redo(&exposure, &masks, &lens).unwrap();
        assert!(!masks_changed);
        assert_eq!(redone.exposure.exposure, 2.0);
    }

    #[test]
    fn selection_navigation_does_not_create_history() {
        let (exposure, mut masks, lens) = state();
        masks.add_mask(MaskKind::Radial).unwrap();
        let mut history = EditHistory::new(&exposure, &masks, &lens);
        let mask_contents = history.committed_masks();

        masks.selected_mask = None;
        masks.selected_component = None;
        history.observe(&exposure, &masks, &lens, false);

        assert!(history.undo.is_empty());
        assert!(!history.can_undo());
        assert!(Arc::ptr_eq(&mask_contents, &history.current.masks));
        let materialized = history.current.materialize_masks();
        assert_eq!(materialized.selected_mask, None);
        assert_eq!(materialized.selected_component, None);
    }

    #[test]
    fn undo_redo_restore_each_mask_states_valid_selection() {
        let (exposure, mut masks, lens) = state();
        masks.add_mask(MaskKind::Radial).unwrap();
        let mut history = EditHistory::new(&exposure, &masks, &lens);

        masks.add_mask(MaskKind::Linear).unwrap();
        history.note_mask_change();
        history.observe(&exposure, &masks, &lens, false);

        let (restored, masks_changed, _remove_changed) =
            history.undo(&exposure, &masks, &lens).unwrap();
        assert!(masks_changed);
        let restored_masks = restored.materialize_masks();
        assert_eq!(restored_masks.masks.len(), 1);
        assert_eq!(restored_masks.selected_mask, Some(0));
        assert_eq!(restored_masks.selected_component, Some(0));

        let (redone, masks_changed, _remove_changed) =
            history.redo(&exposure, &restored_masks, &lens).unwrap();
        assert!(masks_changed);
        let redone_masks = redone.materialize_masks();
        assert_eq!(redone_masks.masks.len(), 2);
        assert_eq!(redone_masks.selected_mask, Some(1));
        assert_eq!(redone_masks.selected_component, Some(0));
    }

    #[test]
    fn separate_mask_gestures_are_separate_undo_steps() {
        let (exposure, mut masks, lens) = state();
        masks.add_mask(MaskKind::Brush).unwrap();
        let mut history = EditHistory::new(&exposure, &masks, &lens);

        masks.masks[0].opacity = 0.8;
        history.note_mask_change();
        history.observe(&exposure, &masks, &lens, true);
        history.observe(&exposure, &masks, &lens, false);

        masks.masks[0].opacity = 0.6;
        history.note_mask_change();
        history.observe(&exposure, &masks, &lens, true);
        history.observe(&exposure, &masks, &lens, false);

        let (first_undo, masks_changed, _remove_changed) =
            history.undo(&exposure, &masks, &lens).unwrap();
        assert!(masks_changed);
        let first_masks = first_undo.materialize_masks();
        assert_eq!(first_masks.masks[0].opacity, 0.8);

        let (second_undo, masks_changed, _remove_changed) =
            history.undo(&exposure, &first_masks, &lens).unwrap();
        assert!(masks_changed);
        assert_eq!(second_undo.materialize_masks().masks[0].opacity, 1.0);

        let (first_redo, masks_changed, _remove_changed) = history
            .redo(&exposure, &second_undo.materialize_masks(), &lens)
            .unwrap();
        assert!(masks_changed);
        assert_eq!(first_redo.materialize_masks().masks[0].opacity, 0.8);

        let (second_redo, masks_changed, _remove_changed) = history
            .redo(&exposure, &first_redo.materialize_masks(), &lens)
            .unwrap();
        assert!(masks_changed);
        assert_eq!(second_redo.materialize_masks().masks[0].opacity, 0.6);
    }

    #[test]
    fn global_edits_share_mask_contents_across_snapshots() {
        let (mut exposure, mut masks, lens) = state();
        masks.add_mask(MaskKind::Brush).unwrap();
        let mut history = EditHistory::new(&exposure, &masks, &lens);
        let original_contents = history.committed_masks();

        exposure.exposure = 1.5;
        history.note_change();
        history.observe(&exposure, &masks, &lens, false);

        assert!(Arc::ptr_eq(&original_contents, &history.current.masks));
        assert!(Arc::ptr_eq(
            &history.undo.back().unwrap().masks,
            &history.current.masks
        ));
        assert!(Arc::ptr_eq(
            &history.committed_masks(),
            &history.current.masks
        ));
        let (_, masks_changed, _remove_changed) = history.undo(&exposure, &masks, &lens).unwrap();
        assert!(!masks_changed);
    }

    #[test]
    fn a_mask_edit_allocates_new_contents_once_then_global_edits_reuse_it() {
        let (mut exposure, mut masks, lens) = state();
        masks.add_mask(MaskKind::Radial).unwrap();
        let mut history = EditHistory::new(&exposure, &masks, &lens);
        let before_mask_edit = history.committed_masks();

        masks.masks[0].opacity = 0.4;
        history.note_mask_change();
        history.observe(&exposure, &masks, &lens, false);
        let after_mask_edit = history.committed_masks();
        assert!(!Arc::ptr_eq(&before_mask_edit, &after_mask_edit));

        exposure.exposure = 1.25;
        history.note_change();
        history.observe(&exposure, &masks, &lens, false);
        assert!(Arc::ptr_eq(&after_mask_edit, &history.current.masks));
        assert!(Arc::ptr_eq(
            &history.undo.back().unwrap().masks,
            &history.current.masks
        ));
    }

    #[test]
    fn mask_content_and_lens_selection_round_trip() {
        let (exposure, mut masks, mut lens) = state();
        let mut history = EditHistory::new(&exposure, &masks, &lens);

        masks.add_mask(MaskKind::Radial).unwrap();
        lens.enabled = true;
        lens.selected_maker = "Example".to_owned();
        lens.selected_model = "Prime 50".to_owned();
        history.note_mask_change();
        history.observe(&exposure, &masks, &lens, false);

        let (restored, masks_changed, _remove_changed) =
            history.undo(&exposure, &masks, &lens).unwrap();
        assert!(masks_changed);
        assert!(restored.masks.masks.is_empty());
        assert!(!restored.lens.enabled);

        masks = restored.materialize_masks();
        restored.lens.apply_to(&mut lens);
        let (redone, masks_changed, _remove_changed) =
            history.redo(&exposure, &masks, &lens).unwrap();
        assert!(masks_changed);
        assert_eq!(redone.masks.masks.len(), 1);
        assert!(redone.lens.enabled);
        assert_eq!(redone.lens.selected_maker, "Example");
        assert_eq!(redone.lens.selected_model, "Prime 50");
    }

    #[test]
    fn redo_is_kept_for_a_reverted_gesture_and_cleared_by_a_new_edit() {
        let (mut exposure, masks, lens) = state();
        let mut history = EditHistory::new(&exposure, &masks, &lens);

        exposure.exposure = 1.0;
        history.note_change();
        history.observe(&exposure, &masks, &lens, false);
        let (restored, _, _) = history.undo(&exposure, &masks, &lens).unwrap();
        exposure = restored.exposure;
        assert!(history.can_redo());

        exposure.exposure = 0.9;
        history.note_change();
        history.observe(&exposure, &masks, &lens, true);
        exposure.exposure = 0.0;
        history.observe(&exposure, &masks, &lens, false);
        assert!(history.can_redo());

        exposure.exposure = 1.2;
        history.note_change();
        history.observe(&exposure, &masks, &lens, false);
        assert!(!history.can_redo());
    }

    #[test]
    fn history_is_bounded() {
        let (mut exposure, masks, lens) = state();
        let mut history = EditHistory::new(&exposure, &masks, &lens);
        for index in 0..(EDIT_HISTORY_LIMIT + 7) {
            exposure.exposure = index as f32;
            history.note_change();
            history.observe(&exposure, &masks, &lens, false);
        }
        assert_eq!(history.undo.len(), EDIT_HISTORY_LIMIT);
    }

    #[test]
    fn revision_changes_only_when_a_transaction_commits() {
        let (mut exposure, masks, lens) = state();
        let mut history = EditHistory::new(&exposure, &masks, &lens);

        exposure.exposure = 1.0;
        history.note_change();
        history.observe(&exposure, &masks, &lens, true);
        assert_eq!(history.committed_revision(), 0);

        exposure.exposure = 1.5;
        history.note_change();
        history.observe(&exposure, &masks, &lens, true);
        assert_eq!(history.committed_revision(), 0);

        history.observe(&exposure, &masks, &lens, false);
        assert_eq!(history.committed_revision(), 1);
    }

    #[test]
    fn remove_strokes_are_discrete_undoable_history_steps() {
        let (exposure, masks, lens) = state();
        let mut remove = Arc::new(RemoveEditState::default());
        let mut history = EditHistory::new(&exposure, &masks, &lens);
        history.reset(&exposure, &masks, &lens, &remove);

        Arc::make_mut(&mut remove)
            .strokes
            .push(RemoveStroke::default());
        history.commit_remove_state(&exposure, &masks, &lens, &remove);
        assert_eq!(history.committed_remove().strokes.len(), 1);

        let (undone, masks_changed, remove_changed) =
            history.undo(&exposure, &masks, &lens).unwrap();
        assert!(!masks_changed);
        assert!(remove_changed);
        assert!(undone.remove.strokes.is_empty());

        let (redone, masks_changed, remove_changed) =
            history.redo(&exposure, &masks, &lens).unwrap();
        assert!(!masks_changed);
        assert!(remove_changed);
        assert_eq!(redone.remove.strokes.len(), 1);
    }
}
