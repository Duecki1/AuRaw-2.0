use super::*;

#[cfg(not(target_os = "android"))]
pub(super) fn apply_library_adjustment_paste(
    app: &mut AurawApp,
    paths: Vec<PathBuf>,
    mode: crate::sidecar::AdjustmentPasteMode,
    context: &egui::Context,
    frame: &eframe::Frame,
) {
    let total = paths.len();
    let (completed, ai_refresh, failures) =
        app.paste_library_adjustments_to_paths(&paths, mode, frame);
    app.library.clear_selection();
    app.library.refresh(context);
    app.library.status = if failures.is_empty() {
        format!(
            "Pasted adjustments to {} selected {}",
            completed.len(),
            if completed.len() == 1 {
                "image"
            } else {
                "images"
            }
        )
    } else {
        format!(
            "Pasted adjustments to {} of {total} selected images. {}",
            completed.len(),
            failures.join(" · ")
        )
    };
    app.library.ai_mask_refresh_prompt =
        (!ai_refresh.is_empty()).then_some(LibraryAiMaskRefreshPrompt { paths: ai_refresh });
}

#[cfg(target_os = "android")]
pub(super) fn apply_library_adjustment_paste(
    app: &mut AurawApp,
    targets: Vec<(String, String)>,
    mode: crate::sidecar::AdjustmentPasteMode,
    context: &egui::Context,
    frame: &eframe::Frame,
) {
    let total = targets.len();
    let (completed, ai_refresh, failures) =
        app.paste_library_adjustments_to_android(&targets, mode, frame);
    app.library.clear_selection();
    crate::android::set_back_navigation_active(false);
    app.library.refresh(context);
    app.library.status = if failures.is_empty() {
        format!(
            "Pasted adjustments to {} selected {}",
            completed.len(),
            if completed.len() == 1 {
                "image"
            } else {
                "images"
            }
        )
    } else {
        format!(
            "Pasted adjustments to {} of {total} selected images. {}",
            completed.len(),
            failures.join(" · ")
        )
    };
    app.library.ai_mask_refresh_prompt =
        (!ai_refresh.is_empty()).then_some(LibraryAiMaskRefreshPrompt {
            targets: ai_refresh,
        });
}

#[cfg(target_os = "android")]
pub(super) fn prepare_android_cloud_adjustment_paste(
    ui: &mut Ui,
    app: &mut AurawApp,
    paths: Vec<PathBuf>,
    frame: &eframe::Frame,
) {
    let (edited_count, failures) = app.library_adjustment_edit_count_paths(&paths);
    if !failures.is_empty() {
        app.library.status = format!(
            "Could not inspect selected cloud adjustments. {}",
            failures.join(" · ")
        );
    } else if edited_count > 0 {
        app.library.adjustment_paste_dialog = Some(LibraryAdjustmentPasteDialog {
            targets: AndroidAdjustmentPasteTargets::Cloud(paths),
            edited_count,
        });
    } else {
        apply_android_cloud_adjustment_paste(
            app,
            paths,
            crate::sidecar::AdjustmentPasteMode::Merge,
            ui.ctx(),
            frame,
        );
    }
}

#[cfg(target_os = "android")]
pub(super) fn apply_android_cloud_adjustment_paste(
    app: &mut AurawApp,
    paths: Vec<PathBuf>,
    mode: crate::sidecar::AdjustmentPasteMode,
    context: &egui::Context,
    frame: &eframe::Frame,
) {
    let total = paths.len();
    let (completed, ai_refresh, failures) =
        app.paste_library_adjustments_to_paths(&paths, mode, frame);
    app.library.clear_selection();
    crate::android::set_back_navigation_active(false);
    app.library.refresh(context);
    let mut status = if failures.is_empty() {
        format!(
            "Pasted adjustments to {} selected {}",
            completed.len(),
            if completed.len() == 1 {
                "cloud image"
            } else {
                "cloud images"
            }
        )
    } else {
        format!(
            "Pasted adjustments to {} of {total} selected cloud images. {}",
            completed.len(),
            failures.join(" · ")
        )
    };
    if !ai_refresh.is_empty() {
        status.push_str(
            " Content-aware masks were marked for regeneration and can be refreshed when each cloud RAW is opened.",
        );
    }
    app.library.status = status;
}

