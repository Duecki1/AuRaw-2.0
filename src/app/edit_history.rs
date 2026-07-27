use super::{needs_canonical_mask_source, AppTab, AurawApp, LensCorrectionState};
use crate::pipeline::{ExposureParams, InpaintStroke, MaskGeometry, MaskStack};
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

/// CPU-side edit state only. GPU pipelines and texture handles deliberately do
/// not participate in history; applying a snapshot goes through the normal
/// dirty-stage and mask-atlas paths.
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
        // A newly-created mask/component can be selected before the new
        // semantic snapshot is committed. It does not exist in the previous
        // snapshot, so keep that snapshot's last valid selection instead of
        // replacing it with invalid (or empty) navigation.
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
    /// Canonical mask contents with navigation fields cleared. Consecutive
    /// global/lens snapshots share this allocation; only a semantic mask edit
    /// creates a new one.
    masks: Arc<MaskStack>,
    mask_selection: MaskSelection,
    /// Inpainting patches can be much larger than ordinary adjustment state.
    /// Share them across unrelated snapshots and only clone the vector when an
    /// inpainting edit actually changes.
    inpainting: Arc<Vec<InpaintStroke>>,
    lens: LensEditState,
}

impl EditSnapshot {
    fn capture(exposure: &ExposureParams, masks: &MaskStack, lens: &LensCorrectionState) -> Self {
        Self::capture_with_inpainting(exposure, masks, lens, &[])
    }

    fn capture_with_inpainting(
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        inpainting: &[InpaintStroke],
    ) -> Self {
        let mut contents = masks.clone();
        contents.selected_mask = None;
        contents.selected_component = None;
        let contents = Arc::new(contents);
        Self {
            exposure: *exposure,
            mask_selection: MaskSelection::capture(masks, &contents),
            masks: contents,
            inpainting: Arc::new(inpainting.to_vec()),
            lens: LensEditState::capture(lens),
        }
    }

    fn capture_successor(
        &self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        inpainting: &[InpaintStroke],
        mask_contents_match: bool,
        inpainting_contents_match: bool,
    ) -> Self {
        let contents = if mask_contents_match {
            Arc::clone(&self.masks)
        } else {
            let mut contents = masks.clone();
            contents.selected_mask = None;
            contents.selected_component = None;
            Arc::new(contents)
        };
        let inpainting = if inpainting_contents_match {
            Arc::clone(&self.inpainting)
        } else {
            Arc::new(inpainting.to_vec())
        };
        Self {
            exposure: *exposure,
            mask_selection: MaskSelection::capture(masks, &contents),
            masks: contents,
            inpainting,
            lens: LensEditState::capture(lens),
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

    fn materialize_inpainting(&self) -> Vec<InpaintStroke> {
        (*self.inpainting).clone()
    }
}

pub(super) struct EditHistory {
    undo: VecDeque<EditSnapshot>,
    redo: VecDeque<EditSnapshot>,
    current: EditSnapshot,
    interaction_pending: bool,
    mask_interaction_pending: bool,
    inpainting_interaction_pending: bool,
    change_observed: bool,
    mask_change_observed: bool,
    inpainting_change_observed: bool,
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
            inpainting_interaction_pending: false,
            change_observed: false,
            mask_change_observed: false,
            inpainting_change_observed: false,
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
    ) {
        self.reset_with_inpainting(exposure, masks, lens, &[]);
    }

    pub(super) fn reset_with_inpainting(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        inpainting: &[InpaintStroke],
    ) {
        self.undo.clear();
        self.redo.clear();
        self.current = EditSnapshot::capture_with_inpainting(exposure, masks, lens, inpainting);
        self.interaction_pending = false;
        self.mask_interaction_pending = false;
        self.inpainting_interaction_pending = false;
        self.change_observed = false;
        self.mask_change_observed = false;
        self.inpainting_change_observed = false;
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

    fn note_inpainting_change(&mut self) {
        if !self.restoring_snapshot {
            self.change_observed = true;
            self.inpainting_change_observed = true;
        }
    }

    fn set_restoring_snapshot(&mut self, restoring: bool) {
        self.restoring_snapshot = restoring;
    }

    /// Observe the final application state for a frame, but only inspect the
    /// potentially large mask stack after an edit path signalled a change.
    /// While a pointer or text field owns an interaction, keep the original
    /// baseline and defer the one comparison/snapshot until release/focus
    /// loss. A whole slider drag, curve drag, brush stroke, geometry drag, or
    /// text rename is therefore one edit without O(mask-size) work per frame.
    pub(super) fn observe(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        interaction_active: bool,
    ) {
        self.observe_with_inpainting(exposure, masks, lens, &[], interaction_active);
    }

    pub(super) fn observe_with_inpainting(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        inpainting: &[InpaintStroke],
        interaction_active: bool,
    ) {
        self.current.remember_selection(masks);
        if self.mask_change_observed {
            self.mask_interaction_pending = true;
            self.mask_change_observed = false;
        }
        if self.inpainting_change_observed {
            self.inpainting_interaction_pending = true;
            self.inpainting_change_observed = false;
        }
        if self.change_observed {
            self.interaction_pending = true;
            self.change_observed = false;
        }
        if !self.interaction_pending || interaction_active {
            return;
        }

        self.commit_current_state(exposure, masks, lens, inpainting);
    }

    fn commit_current_state(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        inpainting: &[InpaintStroke],
    ) {
        let mask_change_pending = self.mask_interaction_pending || self.mask_change_observed;
        let inpainting_change_pending =
            self.inpainting_interaction_pending || self.inpainting_change_observed;
        self.change_observed = false;
        self.mask_change_observed = false;
        self.inpainting_change_observed = false;
        // Mask equality can walk every brush dab and cached range image. Only
        // pay for it after a mask edit path explicitly signalled a semantic
        // change. The length check is O(1) and preserves the normal lens-change
        // behavior, which clears masks as part of rebuilding image geometry.
        let mask_contents_match = if self.current.masks.masks.len() != masks.masks.len() {
            false
        } else if mask_change_pending {
            self.current.masks.masks == masks.masks
        } else {
            true
        };
        let inpainting_contents_match = if inpainting_change_pending {
            self.current.inpainting.as_slice() == inpainting
        } else {
            true
        };
        if self.current.exposure == *exposure
            && mask_contents_match
            && inpainting_contents_match
            && self.current.lens.matches(lens)
        {
            self.current.remember_selection(masks);
            self.interaction_pending = false;
            self.mask_interaction_pending = false;
            self.inpainting_interaction_pending = false;
            return;
        }

        let next = self.current.capture_successor(
            exposure,
            masks,
            lens,
            inpainting,
            mask_contents_match,
            inpainting_contents_match,
        );
        let previous = std::mem::replace(&mut self.current, next);
        Self::push_bounded(&mut self.undo, previous);
        self.redo.clear();
        self.interaction_pending = false;
        self.mask_interaction_pending = false;
        self.inpainting_interaction_pending = false;
        self.committed_revision = self.committed_revision.wrapping_add(1);
    }

    pub(super) fn can_undo(
        &self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) -> bool {
        let _ = (exposure, masks, lens);
        !self.undo.is_empty()
            || self.interaction_pending
            || self.mask_interaction_pending
            || self.inpainting_interaction_pending
            || self.change_observed
            || self.mask_change_observed
            || self.inpainting_change_observed
    }

    pub(super) fn can_redo(
        &self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) -> bool {
        let _ = (exposure, masks, lens);
        !self.interaction_pending
            && !self.mask_interaction_pending
            && !self.inpainting_interaction_pending
            && !self.change_observed
            && !self.mask_change_observed
            && !self.inpainting_change_observed
            && !self.redo.is_empty()
    }

    fn undo(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) -> Option<(EditSnapshot, bool)> {
        self.undo_with_inpainting(exposure, masks, lens, &[])
            .map(|(snapshot, masks_changed, _)| (snapshot, masks_changed))
    }

    fn undo_with_inpainting(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        inpainting: &[InpaintStroke],
    ) -> Option<(EditSnapshot, bool, bool)> {
        self.commit_current_state(exposure, masks, lens, inpainting);
        let target = self.undo.pop_back()?;
        let masks_changed = !Arc::ptr_eq(&target.masks, &self.current.masks);
        let inpainting_changed = !Arc::ptr_eq(&target.inpainting, &self.current.inpainting);
        let present = std::mem::replace(&mut self.current, target.clone());
        Self::push_bounded(&mut self.redo, present);
        self.committed_revision = self.committed_revision.wrapping_add(1);
        Some((target, masks_changed, inpainting_changed))
    }

    fn redo(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
    ) -> Option<(EditSnapshot, bool)> {
        self.redo_with_inpainting(exposure, masks, lens, &[])
            .map(|(snapshot, masks_changed, _)| (snapshot, masks_changed))
    }

    fn redo_with_inpainting(
        &mut self,
        exposure: &ExposureParams,
        masks: &MaskStack,
        lens: &LensCorrectionState,
        inpainting: &[InpaintStroke],
    ) -> Option<(EditSnapshot, bool, bool)> {
        // Normally semantic state already matches `current`. Settling here
        // also makes programmatic edits immediately before Redo behave like a
        // new branch, rather than overwriting them with stale history.
        self.commit_current_state(exposure, masks, lens, inpainting);
        let target = self.redo.pop_back()?;
        let masks_changed = !Arc::ptr_eq(&target.masks, &self.current.masks);
        let inpainting_changed = !Arc::ptr_eq(&target.inpainting, &self.current.inpainting);
        let present = std::mem::replace(&mut self.current, target.clone());
        Self::push_bounded(&mut self.undo, present);
        self.committed_revision = self.committed_revision.wrapping_add(1);
        Some((target, masks_changed, inpainting_changed))
    }

    fn committed_revision(&self) -> u64 {
        self.committed_revision
    }

    fn committed_masks(&self) -> Arc<MaskStack> {
        Arc::clone(&self.current.masks)
    }
}

impl AurawApp {
    /// Signal a semantic edit after mutating CPU state. The history observer
    /// will commit it on pointer release (or immediately for non-pointer
    /// changes). Persistence can watch
    /// `edit_commit_revision` and therefore never serialize every drag frame.
    pub(crate) fn note_edit_changed(&mut self) {
        self.edit_history.note_change();
    }

    pub(crate) fn note_geometry_changed(&mut self) {
        self.geometry = self.geometry.sanitized();
        self.geometry_revision = self.geometry_revision.wrapping_add(1);
    }

    /// Signal an edit that changed semantic mask contents. This separate domain
    /// lets global and lens-only transactions share the existing mask snapshot
    /// without scanning large brush/range-mask data.
    pub(crate) fn note_mask_edit_changed(&mut self) {
        self.edit_history.note_mask_change();
    }

    pub(crate) fn note_inpainting_edit_changed(&mut self) {
        self.edit_history.note_inpainting_change();
    }

    pub(crate) fn edit_commit_revision(&self) -> u64 {
        self.edit_history
            .committed_revision()
            .wrapping_mul(0x9e37_79b9_7f4a_7c15)
            ^ self.inpaint_revision
            ^ self.geometry_revision.rotate_left(17)
    }

    /// O(1) snapshot for persistence. Call `commit_edit_history_now` first so
    /// the history's canonical mask contents represent the final UI value.
    /// Navigation selection is intentionally not part of persisted edit data.
    pub(crate) fn committed_mask_state_for_persistence(&self) -> Arc<MaskStack> {
        self.edit_history.committed_masks()
    }

    pub(crate) fn reset_edit_history(&mut self) {
        self.history_lens_restore_masks = None;
        self.edit_history.reset_with_inpainting(
            &self.exposure,
            &self.masks,
            &self.lens_correction,
            &self.inpaint_strokes,
        );
    }

    pub(crate) fn observe_edit_history(&mut self, ctx: &egui::Context) {
        // History transactions follow the pointer gesture itself, not the
        // lifetime of keyboard focus. `egui_wants_keyboard_input()` can remain
        // true after a text field/dialog has taken focus; using it here kept
        // subsequent mask clicks and drags in the same pending transaction,
        // so one Undo could roll back several otherwise independent mask
        // edits. Mask renames are only applied when their dialog is saved, so
        // they still enter history as one discrete change without extending
        // the transaction across unrelated edits.
        let interaction_active = ctx.input(|input| input.pointer.any_down());
        self.edit_history.observe_with_inpainting(
            &self.exposure,
            &self.masks,
            &self.lens_correction,
            &self.inpaint_strokes,
            interaction_active,
        );
    }

    /// Commit a pending slider/text/mask transaction before an explicit save
    /// or image switch. This prevents the final value from remaining only in
    /// the UI state while persistence snapshots the previous baseline.
    pub(crate) fn commit_edit_history_now(&mut self) {
        self.finish_mask_geometry_interaction();
        self.edit_history.observe_with_inpainting(
            &self.exposure,
            &self.masks,
            &self.lens_correction,
            &self.inpaint_strokes,
            false,
        );
    }

    pub(crate) fn can_undo_edit(&self) -> bool {
        self.loaded_raw.is_some()
            && self
                .edit_history
                .can_undo(&self.exposure, &self.masks, &self.lens_correction)
    }

    pub(crate) fn can_redo_edit(&self) -> bool {
        self.loaded_raw.is_some()
            && self
                .edit_history
                .can_redo(&self.exposure, &self.masks, &self.lens_correction)
    }

    pub(crate) fn undo_edit(&mut self) {
        self.finish_mask_geometry_interaction();
        let snapshot = self.edit_history.undo_with_inpainting(
            &self.exposure,
            &self.masks,
            &self.lens_correction,
            &self.inpaint_strokes,
        );
        if let Some((snapshot, masks_changed, inpainting_changed)) = snapshot {
            self.apply_edit_snapshot(snapshot, masks_changed, inpainting_changed);
            self.notice = Some("Undid edit.".to_owned());
        }
    }

    pub(crate) fn redo_edit(&mut self) {
        self.finish_mask_geometry_interaction();
        let snapshot = self.edit_history.redo_with_inpainting(
            &self.exposure,
            &self.masks,
            &self.lens_correction,
            &self.inpaint_strokes,
        );
        if let Some((snapshot, masks_changed, inpainting_changed)) = snapshot {
            self.apply_edit_snapshot(snapshot, masks_changed, inpainting_changed);
            self.notice = Some("Redid edit.".to_owned());
        }
    }

    pub(crate) fn handle_edit_history_shortcuts(&mut self, ctx: &egui::Context) {
        if self.active_tab != AppTab::Develop {
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
        inpainting_changed: bool,
    ) {
        let lens_changed = !snapshot.lens.matches(&self.lens_correction);

        self.edit_history.set_restoring_snapshot(true);
        self.exposure = snapshot.exposure;
        self.exposure.sanitize_tone_curves();
        if masks_changed {
            self.masks = snapshot.materialize_masks();
        } else {
            snapshot.mask_selection.apply_to(&mut self.masks);
        }
        if inpainting_changed {
            self.inpaint_strokes = snapshot.materialize_inpainting();
            self.rebuild_inpaint_layer();
            self.inpaint_revision = self.inpaint_revision.wrapping_add(1);
            self.note_inpainting_changed_for_ai_masks();
        }
        snapshot.lens.apply_to(&mut self.lens_correction);
        self.rehydrate_restored_mask_state();

        if lens_changed {
            // Lens correction rebuilds image geometry. Keep the snapshot's
            // masks aside so the rebuild uploads the mask stack that belongs
            // to that exact historical lens state before marking generated
            // mask sources as needing an explicit refresh.
            self.history_lens_restore_masks = Some(std::mem::take(&mut self.masks));
            self.mark_lens_correction_dirty();
        } else {
            if masks_changed {
                self.mark_all_mask_layers_dirty();
            }
            if inpainting_changed {
                self.queue_preview_processing(crate::pipeline::ProcessingStage::Tone);
            }
            self.mark_pipeline_dirty();
        }
        self.edit_history.set_restoring_snapshot(false);
    }

    pub(super) fn rehydrate_restored_mask_state(&mut self) {
        self.active_mask_tool = self
            .masks
            .selected_component()
            .map(|component| component.kind)
            .filter(|kind| kind.is_available());
        self.mask_drag = None;
        self.last_brush_point = None;
        self.mask_touch_gesture_backup = None;
        self.mask_interaction_dirty_layer = None;
        self.mask_interaction_last_upload = None;
        self.mask_interaction_has_uncommitted_change = false;
        self.mask_overlay_revision = self.mask_overlay_revision.wrapping_add(1);
        self.mask_overlay_texture = None;
        self.mask_overlay_texture_key = None;
        self.mask_overlay_blink = None;
        self.mask_thumbnail_group_textures.clear();
        self.mask_thumbnail_component_mask = None;
        self.mask_thumbnail_component_textures.clear();
        self.mask_thumbnail_revision = self.mask_overlay_revision;

        let restored_source = self.masks.masks.iter().find_map(|mask| {
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
        if restored_source.is_some() || !needs_canonical_mask_source(&self.masks) {
            self.mask_source_cache = restored_source;
        }
        self.subject_mask_cache = self.masks.masks.iter().find_map(|mask| {
            mask.components
                .iter()
                .find_map(|component| match &component.geometry {
                    MaskGeometry::Ai {
                        mask: Some(mask), ..
                    } => Some(mask.clone()),
                    _ => None,
                })
        });
        self.ai_mask_update_active = false;
        self.ai_mask_update_subject_pending = false;
        self.ai_mask_update_object_queue.clear();
        self.ai_mask_update_failed = false;
        if self.ai_masks_need_update {
            let (subject, objects) = self.generated_ai_mask_targets();
            self.ai_masks_need_update =
                subject || !objects.is_empty() || self.has_range_mask_targets();
        }
        self.subject_consent_open = false;
        self.subject_receiver = None;
        self.subject_download_progress = None;
        self.subject_inferencing = false;
        self.object_consent_open = false;
        self.object_pending_target = None;
        self.object_receiver = None;
        self.object_download_progress = None;
        self.object_inferencing = false;
        self.object_decoder_only = false;
        self.object_generation = self.object_generation.wrapping_add(1);
        self.object_job_generation = 0;
        self.object_job_target = None;
        self.object_cache = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::MaskKind;

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

        let (restored, masks_changed) = history.undo(&exposure, &masks, &lens).unwrap();
        assert!(!masks_changed);
        assert_eq!(restored.exposure.exposure, 0.0);
        assert!(history.undo.is_empty());

        exposure = restored.exposure;
        let (redone, masks_changed) = history.redo(&exposure, &masks, &lens).unwrap();
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
        assert!(!history.can_undo(&exposure, &masks, &lens));
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

        let (restored, masks_changed) = history.undo(&exposure, &masks, &lens).unwrap();
        assert!(masks_changed);
        let restored_masks = restored.materialize_masks();
        assert_eq!(restored_masks.masks.len(), 1);
        assert_eq!(restored_masks.selected_mask, Some(0));
        assert_eq!(restored_masks.selected_component, Some(0));

        let (redone, masks_changed) = history.redo(&exposure, &restored_masks, &lens).unwrap();
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

        let (first_undo, masks_changed) = history.undo(&exposure, &masks, &lens).unwrap();
        assert!(masks_changed);
        let first_masks = first_undo.materialize_masks();
        assert_eq!(first_masks.masks[0].opacity, 0.8);

        let (second_undo, masks_changed) = history
            .undo(&exposure, &first_masks, &lens)
            .unwrap();
        assert!(masks_changed);
        assert_eq!(second_undo.materialize_masks().masks[0].opacity, 1.0);

        let (first_redo, masks_changed) = history
            .redo(&exposure, &second_undo.materialize_masks(), &lens)
            .unwrap();
        assert!(masks_changed);
        assert_eq!(first_redo.materialize_masks().masks[0].opacity, 0.8);

        let (second_redo, masks_changed) = history
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
        let (_, masks_changed) = history.undo(&exposure, &masks, &lens).unwrap();
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

        let (restored, masks_changed) = history.undo(&exposure, &masks, &lens).unwrap();
        assert!(masks_changed);
        assert!(restored.masks.masks.is_empty());
        assert!(!restored.lens.enabled);

        masks = restored.materialize_masks();
        restored.lens.apply_to(&mut lens);
        let (redone, masks_changed) = history.redo(&exposure, &masks, &lens).unwrap();
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
        let (restored, _) = history.undo(&exposure, &masks, &lens).unwrap();
        exposure = restored.exposure;
        assert!(history.can_redo(&exposure, &masks, &lens));

        exposure.exposure = 0.9;
        history.note_change();
        history.observe(&exposure, &masks, &lens, true);
        exposure.exposure = 0.0;
        history.observe(&exposure, &masks, &lens, false);
        assert!(history.can_redo(&exposure, &masks, &lens));

        exposure.exposure = 1.2;
        history.note_change();
        history.observe(&exposure, &masks, &lens, false);
        assert!(!history.can_redo(&exposure, &masks, &lens));
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
}
