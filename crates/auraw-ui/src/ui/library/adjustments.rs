use super::*;

pub(super) fn apply_library_adjustment_paste(
    app: &mut AurawApp,
    assets: Vec<LibraryAsset>,
    mode: crate::sidecar::AdjustmentPasteMode,
    context: &egui::Context,
    frame: &eframe::Frame,
) {
    let total = assets.len();
    let (completed, ai_refresh, failures) = app.paste_library_adjustments(&assets, mode, frame);
    app.library.clear_selection();
    #[cfg(target_os = "android")]
    crate::android::set_back_navigation_active(false);
    app.library.refresh(context);
    app.library.status = if failures.is_empty() {
        format!(
            "Pasted adjustments to {completed} selected {}",
            if completed == 1 { "image" } else { "images" }
        )
    } else {
        format!(
            "Pasted adjustments to {completed} of {total} selected images. {}",
            failures.join(" · ")
        )
    };
    app.library.ai_mask_refresh_prompt =
        (!ai_refresh.is_empty()).then_some(LibraryAiMaskRefreshPrompt { assets: ai_refresh });
}

pub(super) fn start_library_ai_mask_refresh_for_assets(
    app: &mut AurawApp,
    assets: Vec<LibraryAsset>,
    frame: &eframe::Frame,
) {
    start_local_library_ai_mask_refresh(app, &assets, frame);
}
