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
PIPELINE_EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
ANDROID = (ROOT / "src/android.rs").read_text(encoding="utf-8")


def test_opening_a_raw_is_not_blocked_by_export_receivers():
    start = LIFECYCLE.index("fn open_path_labeled_with_options")
    end = LIFECYCLE.index("let Some(render_state)", start)
    preflight = LIFECYCLE[start:end]
    assert "if self.load_receiver.is_some()" in preflight
    assert "export_receiver.is_some()" not in preflight
    assert "export_publish_pending" not in preflight


def test_desktop_batch_export_owns_a_separate_worker_and_does_not_open_documents():
    assert "fn spawn_desktop_library_batch_export" in EXPORT
    start = EXPORT.index("pub(crate) fn start_library_exports")
    desktop_start = EXPORT[start : EXPORT.index("fn on_library_batch_load_finished", start)]
    assert "self.open_path(job.source" not in desktop_start
    assert "Desktop batch export owns a separate decode/export worker" in EXPORT
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
    assert "EXPORT_TILE_PHASE_WEIGHT: f32 = 0.90" in EXPORT
    assert "EXPORT_MAX_INCOMPLETE_FRACTION: f32 = 0.99" in EXPORT
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
    assert 'self.start_export_task(task_id, request, frame)' in android_item_start
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


def test_desktop_global_progress_control_does_not_force_top_bar_height():
    start = RUNTIME.index("pub(crate) fn show_global_task_control")
    end = RUNTIME.index("fn background_task_progress_widget", start)
    control = RUNTIME[start:end]

    assert '.horizontal(|ui| {' in control
    assert 'desktop_top_bar_height' not in control
    assert 'available_size_before_wrap().y' not in control
    assert 'ui.set_min_height(' not in control
    assert '.horizontal_centered(|ui| {' not in control


def test_single_export_progress_reserves_finalization_work():
    worker = EXPORT[EXPORT.index("fn poll_export_worker") :]
    assert "tile_fraction * EXPORT_TILE_PHASE_WEIGHT" in worker
    assert '"Finalizing export…"' in worker
    assert "EXPORT_MAX_INCOMPLETE_FRACTION" in worker


def test_export_dispatch_is_shared_by_single_and_batch_workers():
    assert EXPORT.count("fn spawn_export_request") == 1
    assert "spawn_export_request(request, cancellation)" in RUNTIME
    assert "spawn_export_request(request, Arc::clone(&cancellation))" in EXPORT


def test_cancelled_completed_export_is_not_published():
    start = PIPELINE_EXPORT.index("fn export_to_destination")
    end = PIPELINE_EXPORT.index("fn ensure_export_not_cancelled", start)
    publish = PIPELINE_EXPORT[start:end]
    final_cancel = publish.rindex("ensure_export_not_cancelled")
    publication = publish.index("publish_completed_export")
    assert final_cancel < publication
    assert "remove_file(&temporary)" in publish[final_cancel:publication]
    assert "cancelled_export_removes_temporary_output_before_publication" in PIPELINE_EXPORT


def test_android_single_and_batch_exports_release_preview_gpu_resources_centrally():
    helper_start = EXPORT.index("fn suspend_android_preview_for_export")
    helper_end = EXPORT.index("fn capture_export_task_request", helper_start)
    helper = EXPORT[helper_start:helper_end]
    assert "take_preview_pipeline_and_release_textures" in helper
    assert helper.index("let previous_pipeline") < helper.index("drop(previous_pipeline)")
    assert "if restore_after_export" in helper
    assert "self.preview_quality_dirty = true;" in helper

    starter_start = RUNTIME.index("fn start_export_task")
    starter_end = RUNTIME.index("fn start_library_batch_export_task", starter_start)
    starter = RUNTIME[starter_start:starter_end]
    assert "self.suspend_android_preview_for_export(frame, restore_preview)" in starter
    assert "let restore_preview = self.library_batch_export.is_none();" in starter

    batch_start = EXPORT.index("fn on_library_batch_load_finished")
    batch_end = EXPORT.index("fn complete_android_library_batch_export_item", batch_start)
    batch_item = EXPORT[batch_start:batch_end]
    assert "self.start_export_task(task_id, request, frame)" in batch_item
    assert "take_preview_pipeline_and_release_textures" not in batch_item


def test_android_preview_rebuild_waits_until_export_and_publication_finish():
    start = EXPORT.index("fn apply_pending_preview_quality")
    end = EXPORT.index("fn advance_preview_detail", start)
    rebuild = EXPORT[start:end]
    assert "self.export_receiver.is_some()" in rebuild
    assert "self.export_publish_pending" in rebuild
    assert "self.library_batch_export.is_some()" in rebuild


def test_raw_load_releases_renderer_lock_before_batch_export_callback():
    start = LIFECYCLE.index("fn poll_load_worker")
    end = LIFECYCLE.index("#[cfg(test)]", start)
    poll = LIFECYCLE[start:end]
    lock = poll.index("let previous_pipeline = {")
    unlock = poll.index("drop(previous_pipeline);", lock)
    callback = poll.index("self.on_library_batch_load_finished(true, frame);", unlock)
    assert lock < unlock < callback
    assert "epaint 10-second deadlock panic" in poll


def test_android_picker_synchronous_open_failures_are_routed_to_internal_owners():
    picker_start = LIFECYCLE.index(
        "while let Some(result) = crate::android::take_picker_result()"
    )
    picker = LIFECYCLE[picker_start:]
    assert "if self.load_receiver.is_none()" in picker
    failure = picker[picker.index("if self.load_receiver.is_none()") :]
    assert "complete_android_library_batch_export_item(Err(error))" in failure
    assert "complete_android_library_ai_mask_open_failure(error, frame)" in failure


def test_document_changes_do_not_silently_dismiss_unacknowledged_failures():
    for function_name in [
        "fn cancel_document_bound_background_tasks",
        "fn cancel_stale_document_background_tasks",
    ]:
        start = RUNTIME.index(function_name)
        tail = RUNTIME[start:]
        next_function = tail.find("\n    fn ", len(function_name))
        body = tail if next_function < 0 else tail[:next_function]
        assert "task.status != TaskStatus::Failed" in body


def test_detached_global_tasks_and_waiting_badge_use_manager_state():
    assert "fn global_primary_snapshot_and_waiting_count" in TASKS
    assert "global_waiting_count_excludes_the_displayed_queued_task" in TASKS
    assert "global_waiting_count_includes_all_tasks_after_the_running_task" in TASKS
    assert "global_primary_snapshot_and_waiting_count()" in RUNTIME


def test_android_notification_dedup_resets_for_new_activity():
    assert "activity_key: usize" in ANDROID
    assert "current.activity_key == activity_key" in ANDROID
    install = ANDROID[
        ANDROID.index("pub fn install_context") : ANDROID.index(
            "pub fn set_back_navigation_active"
        )
    ]
    assert "TASK_NOTIFICATION_STATE.lock()" in install
    assert "*notification = None;" in install


def test_lens_worker_reuses_android_cached_correction_when_available():
    app_source = (ROOT / "src/app.rs").read_text(encoding="utf-8")
    assert "cached_raws: Option<(Arc<LoadedRaw>, Arc<LoadedRaw>)>" in app_source
    assert "if let Some(cached_raws) = request.cached_raws" in EXPORT
    assert "lens_corrected_preview_cache" in EXPORT
    assert "lens_original_preview_cache" in EXPORT


def test_object_download_failure_uses_one_error_surface():
    finished = MASKS[MASKS.index("fn poll_object_worker") :]
    assert "if failed_during_inference" in finished
    assert "self.object_error_dialog = Some(message);" in finished
    assert "self.fail_background_task(task_id, message);" in finished


def test_library_batch_progress_dialog_is_rendered_by_one_shared_helper():
    assert LIBRARY_UI.count("fn show_library_batch_export_progress") == 1
    assert LIBRARY_UI.count("show_library_batch_export_progress(ui, app);") == 2
    assert LIBRARY_UI.count('egui::Window::new("Exporting images")') == 1


def test_android_batch_cancellation_during_raw_load_never_starts_export():
    start = EXPORT.index("fn on_library_batch_load_finished")
    end = EXPORT.index("fn complete_android_library_batch_export_item", start)
    handler = EXPORT[start:end]
    cancellation = handler.index("if batch.cancel_requested")
    capture = handler.index("capture_export_task_request")
    assert cancellation < capture
    assert '"batch export cancelled"' in handler[cancellation:capture]


def test_batch_export_lifecycle_helpers_are_shared_across_platforms():
    assert EXPORT.count("fn finish_library_batch_export(") == 1
    assert EXPORT.count("fn library_batch_export_status(") == 1
    assert EXPORT.count("fn library_batch_export_progress(") == 1
    assert EXPORT.count("fn library_batch_export_tile_progress(") == 1
    assert EXPORT.count("fn request_library_batch_export_cancellation(") == 1


def test_android_lens_cache_setup_does_not_panic_on_missing_preview_state():
    assert 'expect("loaded RAW set")' not in LIFECYCLE
    assert 'expect("preview RAW set")' not in LIFECYCLE
    assert 'self.lens_corrected_preview_cache = match (' in LIFECYCLE
    assert '(Some(selection), Some(full_raw), Some(preview_raw))' in LIFECYCLE


def test_missing_background_action_uses_repainting_failure_path():
    start = RUNTIME.index("fn drive_background_tasks")
    end = RUNTIME.index("fn start_export_task", start)
    driver = RUNTIME[start:end]
    assert 'self.fail_background_task(id, "The queued background action was unavailable.");' in driver
    assert 'self.background_tasks\n                .fail(' not in driver


def test_unknown_unit_total_uses_indeterminate_progress():
    start = RUNTIME.index("fn background_task_progress_widget")
    end = RUNTIME.index("pub(crate) fn show_background_task_detail_windows", start)
    widget = RUNTIME[start:end]
    assert "if *total == 0" in widget
    assert "egui::ProgressBar::new(0.0).animate(true)" in widget
