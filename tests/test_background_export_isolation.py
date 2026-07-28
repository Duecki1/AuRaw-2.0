from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
LIFECYCLE = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
TASKS = (ROOT / "src/app/background_tasks.rs").read_text(encoding="utf-8")
RUNTIME = (ROOT / "src/app/background_task_runtime.rs").read_text(encoding="utf-8")
MASKS = (ROOT / "src/app/masks_ai.rs").read_text(encoding="utf-8")
INPAINT = (ROOT / "src/app/inpainting.rs").read_text(encoding="utf-8")
UI_MOD = (ROOT / "src/ui/mod.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
LIBRARY_UI = (ROOT / "src/ui/library.rs").read_text(encoding="utf-8")
TOP_BAR = (ROOT / "src/ui/top_bar.rs").read_text(encoding="utf-8")


def test_opening_a_raw_is_not_blocked_by_export_receivers():
    start = LIFECYCLE.index("fn open_path_labeled_with_options")
    end = LIFECYCLE.index("let Some(render_state)", start)
    preflight = LIFECYCLE[start:end]
    assert "if self.load_receiver.is_some()" in preflight
    assert "export_receiver.is_some()" not in preflight
    assert "export_publish_pending" not in preflight


def test_desktop_batch_export_owns_a_separate_worker_and_does_not_open_documents():
    assert "fn spawn_desktop_library_batch_export" in EXPORT
    desktop_start = EXPORT[
        EXPORT.index("pub(crate) fn start_library_exports") : EXPORT.index(
            "fn finish_library_batch_export", EXPORT.index("pub(crate) fn start_library_exports")
        )
    ]
    assert "self.open_path(job.source" not in desktop_start
    assert "Desktop batch export owns a separate decode/export worker" in desktop_start
    assert "self.active_tab = AppTab::Library" not in desktop_start
    assert "library_batch_export_receiver" in RUNTIME
    assert "library_batch_export_tile_progress" in EXPORT


def test_ai_generation_is_hidden_from_global_task_ui_after_download():
    assert "global_visible" in TASKS
    assert "global_snapshots" in TASKS
    assert "self.background_tasks.set_global_visible(task_id, false);" in MASKS
    assert "self.background_tasks.set_global_visible(task_id, false);" in INPAINT
    assert '"Downloading subject-mask model"' in MASKS
    assert '"Downloading object-mask model"' in MASKS
    assert '"Downloading inpainting model"' in INPAINT


def test_local_ai_inference_starts_outside_the_fifo_background_slot():
    assert "fn start_nonblocking" in TASKS
    assert "fn release_current" in TASKS
    assert "self.background_tasks.start_nonblocking(" in MASKS
    assert "self.background_tasks.start_nonblocking(" in INPAINT
    assert "self.background_tasks.release_current(task_id);" in MASKS
    assert "self.background_tasks.release_current(task_id);" in INPAINT


def test_ai_downloads_remain_fifo_tasks_but_inference_releases_the_slot():
    subject_download = MASKS[MASKS.index('"Downloading subject-mask model"') - 300 : MASKS.index('"Downloading subject-mask model"') + 500]
    object_download = MASKS[MASKS.index('"Downloading object-mask model"') - 300 : MASKS.index('"Downloading object-mask model"') + 500]
    inpaint_download = INPAINT[INPAINT.index('"Downloading inpainting model"') - 300 : INPAINT.index('"Downloading inpainting model"') + 500]
    assert "enqueue_background_action" in subject_download
    assert "enqueue_background_action" in object_download
    assert "enqueue_background_action" in inpaint_download
    assert "Some(TaskKind::SubjectMask { .. })" in MASKS
    assert "Some(TaskKind::ObjectMask { .. })" in MASKS


def test_detached_ai_tasks_remain_cancellable_and_document_bound():
    assert "released_task_can_be_cancelled_cooperatively" in TASKS
    assert "nonblocking_task_does_not_occupy_fifo_slot" in TASKS
    assert "released_download_allows_next_fifo_task_while_inference_continues" in TASKS
    runtime = RUNTIME[RUNTIME.index("fn cancel_document_bound_background_tasks") :]
    assert "TaskKind::SubjectMask" in runtime
    assert "TaskKind::ObjectMask" in runtime
    assert "TaskKind::Inpainting" in runtime

def test_batch_export_progress_reserves_finalization_and_cannot_finish_early():
    assert "BATCH_EXPORT_TILE_PHASE_WEIGHT: f32 = 0.90" in EXPORT
    assert "BATCH_EXPORT_MAX_INCOMPLETE_FRACTION: f32 = 0.99" in EXPORT
    assert "fn batch_export_overall_fraction" in EXPORT
    assert "library_batch_export_overall_fraction" in EXPORT
    assert "batch_export_overall_fraction(" in RUNTIME
    assert "Finalizing {name}" in RUNTIME
    assert "fully_rendered_current_image_reserves_finalization_progress" in EXPORT

def test_android_batch_export_does_not_open_a_queued_wait_dialog_over_settings():
    android_enqueue_start = EXPORT.index("pub(crate) fn start_android_library_exports")
    android_enqueue_end = EXPORT.index("fn start_next_library_export", android_enqueue_start)
    android_enqueue = EXPORT[android_enqueue_start:android_enqueue_end]
    assert '"Queued for batch export…"' in android_enqueue
    assert "false," in android_enqueue
    assert "BackgroundAction::LibraryBatchExport" in android_enqueue

    android_start_marker = '#[cfg(target_os = "android")]\n    fn start_library_batch_export_task'
    android_start = RUNTIME.index(android_start_marker)
    android_start_end = RUNTIME.index("    fn start_subject_mask_task", android_start)
    android_runtime = RUNTIME[android_start:android_start_end]
    assert "self.background_tasks.set_details_open(id, true);" in android_runtime
    assert android_runtime.index("self.library_batch_export = Some") < android_runtime.index(
        "self.background_tasks.set_details_open(id, true);"
    )


def test_android_batch_items_reuse_the_parent_task_instead_of_queueing_single_exports():
    start = EXPORT.index('fn on_library_batch_load_finished')
    end = EXPORT.index('fn complete_android_library_batch_export_item', start)
    android_item_start = EXPORT[start:end]

    assert 'capture_export_task_request(destination, frame, format)' in android_item_start
    assert 'self.start_export_task(task_id, request);' in android_item_start
    assert 'self.sync_library_batch_background_progress();' in android_item_start
    assert 'self.start_export(destination, frame, format)' not in android_item_start
    assert 'BackgroundAction::SingleExport' not in android_item_start
    assert 'nested SingleExport task' in android_item_start


def test_raw_open_reuses_preview_pipeline_for_canonical_mask_source():
    start = LIFECYCLE.index(
        "// Range and promptable-object source images are canonical RAW renditions"
    )
    end = LIFECYCLE.index("let params =", start)
    canonical_source = LIFECYCLE[start:end]

    assert "RawGpuPipeline::new_headless_reusing_programs" not in canonical_source
    assert "GpuParams::new(&reference_exposure, &reference_masks, &preview_raw)" in canonical_source
    assert "pipeline.recompute(&queue, &device, &reference_params);" in canonical_source
    assert "MaskRgbImage::new(pipeline.width, pipeline.height, rgba)" in canonical_source
    assert "one preview allocation" in canonical_source

    display_transform = LIFECYCLE.index(
        "write_output_transform(&queue, &display_output_transform)", end
    )
    assert display_transform > end


def test_android_background_work_is_modal_and_blocks_raw_opening():
    assert 'fn android_foreground_task_active(&self) -> bool' in RUNTIME
    assert 'self.background_tasks.has_visible_tasks()' in RUNTIME
    assert 'show_android_foreground_task_blocker(ui.ctx());' in EFRAME
    assert 'android-foreground-task-input-blocker' in EFRAME
    assert '.order(egui::Order::Middle)' in EFRAME
    assert '.interactable(true)' in EFRAME
    assert 'if self.android_foreground_task_active() {' in LIFECYCLE
    open_library = LIFECYCLE[LIFECYCLE.index('pub fn open_android_library_document'):]
    assert open_library.index('if self.android_foreground_task_active()') < open_library.index(
        'crate::android::open_library_document'
    )
    assert 'cannot be opened while an export or another foreground operation is running' in open_library


def test_android_progress_windows_stay_above_the_modal_input_blocker():
    assert '#[cfg(target_os = "android")]\n    let window = window.order(eframe::egui::Order::Foreground);' in UI_MOD
    assert 'crate::ui::responsive_popup' in LIBRARY_UI
    assert 'crate::ui::responsive_popup' in MASKS
    assert 'crate::ui::responsive_popup' in INPAINT
    assert 'crate::ui::responsive_popup' in RUNTIME


def test_minimize_buttons_are_desktop_only():
    sources = [RUNTIME, MASKS, INPAINT, LIBRARY_UI]
    for source in sources:
        lines = source.splitlines()
        for index, line in enumerate(lines):
            if '"Minimize"' not in line:
                continue
            context = '\n'.join(lines[max(0, index - 4): index + 1])
            assert '#[cfg(not(target_os = "android"))]' in context


def test_android_back_and_edit_shortcuts_are_blocked_during_foreground_tasks():
    assert 'if self.android_foreground_task_active() {' in EFRAME
    assert 'Ignore system' in EFRAME
    assert 'if !self.android_foreground_task_active() {' in EFRAME
    shortcut_block = EFRAME[EFRAME.index('if !self.android_foreground_task_active() {'):]
    assert 'self.handle_edit_history_shortcuts(ui.ctx());' in shortcut_block
    assert 'self.handle_sidecar_shortcut(ui.ctx());' in shortcut_block


def test_desktop_global_progress_control_is_vertically_centered_without_taking_full_width():
    start = RUNTIME.index("pub(crate) fn show_global_task_control")
    end = RUNTIME.index("fn background_task_progress_widget", start)
    control = RUNTIME[start:end]

    assert 'let available_height = ui.available_size_before_wrap().y;' in control
    assert 'if available_height.is_finite()' in control
    assert 'ui.set_min_height(desktop_top_bar_height);' in control
    assert '.horizontal(|ui| {' in control
    assert '.horizontal_centered(|ui| {' not in control
    assert '#[cfg(not(target_os = "android"))]' in control
