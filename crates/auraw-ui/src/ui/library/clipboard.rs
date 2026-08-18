use super::*;

pub(super) fn image_paste_summary(
    mode: ImageClipboardMode,
    total: usize,
    completed: usize,
    destination: &str,
    errors: Vec<String>,
) -> Result<String, String> {
    let verb = if mode == ImageClipboardMode::Copy { "Copied" } else { "Moved" };
    let noun = if total == 1 { "RAW" } else { "RAWs" };
    if errors.is_empty() {
        Ok(format!("{verb} {completed} {noun} to {destination}."))
    } else {
        Err(format!(
            "{verb} {completed} of {total} {noun} to {destination}. {}",
            errors.join(" · ")
        ))
    }
}

pub(super) fn run_image_paste(
    clipboard: ImageClipboard,
    destination: ImagePasteDestination,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> ImagePasteCompletion {
    let mode = clipboard.mode;
    let total = clipboard.assets.len();
    let mut completed = 0usize;
    let mut errors = Vec::new();
    let mut remaining = clipboard.assets.clone();

    for asset in clipboard.assets {
        if mode == ImageClipboardMode::Cut && asset_is_at_destination(&asset, &destination) {
            completed += 1;
            remaining.retain(|candidate| candidate.id != asset.id);
            continue;
        }
        let materialized = match materialize_library_asset(
            &asset,
            #[cfg(target_os = "android")]
            android_app,
        ) {
            Ok(materialized) => materialized,
            Err(error) => {
                errors.push(format!("{}: {error}", asset.display_name));
                continue;
            }
        };
        let imported = import_materialized_library_asset(
            &materialized,
            &destination,
            #[cfg(target_os = "android")]
            android_app,
        );
        let result = match imported {
            Ok(imported) => {
                if let Err(error) = preserve_imported_thumbnail(
                    &asset,
                    &imported,
                    #[cfg(target_os = "android")]
                    android_app,
                ) {
                    rollback_imported_library_asset(
                        imported,
                        #[cfg(target_os = "android")]
                        android_app,
                    );
                    Err(error)
                } else if mode == ImageClipboardMode::Cut {
                    if let Err(error) = remove_library_asset(
                        &asset,
                        #[cfg(target_os = "android")]
                        android_app,
                    ) {
                        rollback_imported_library_asset(
                            imported,
                            #[cfg(target_os = "android")]
                            android_app,
                        );
                        Err(error)
                    } else {
                        Ok(())
                    }
                } else {
                    Ok(())
                }
            }
            Err(error) => Err(error),
        };
        materialized.cleanup();

        match result {
            Ok(()) => {
                completed += 1;
                remaining.retain(|candidate| candidate.id != asset.id);
            }
            Err(error) => errors.push(format!("{}: {error}", asset.display_name)),
        }
    }

    #[cfg(not(target_os = "android"))]
    let destination_label = match &destination {
        ImagePasteDestination::LocalFolder(folder) => folder.display().to_string(),
    };
    #[cfg(target_os = "android")]
    let destination_label = match &destination {
        ImagePasteDestination::LocalLibrary { path } if !path.is_empty() => path.clone(),
        ImagePasteDestination::LocalLibrary { .. } => "the Library".to_owned(),
    };

    let result = image_paste_summary(mode, total, completed, &destination_label, errors);
    let remaining_clipboard = if mode == ImageClipboardMode::Cut && !remaining.is_empty() {
        Some(ImageClipboard { mode, assets: remaining })
    } else {
        None
    };
    ImagePasteCompletion {
        result,
        clear_clipboard: mode == ImageClipboardMode::Cut && remaining_clipboard.is_none(),
        remaining_clipboard,
    }
}

pub(super) fn start_image_clipboard_paste(
    app: &mut AurawApp,
    destination: ImagePasteDestination,
    context: &egui::Context,
) {
    if app.library.image_paste_receiver.is_some() {
        app.library.status = "Another Library paste is still running.".to_owned();
        return;
    }
    let Some(clipboard) = app.library.image_clipboard.clone() else {
        app.library.status = "Copy or cut Library images first.".to_owned();
        return;
    };
    let (sender, receiver) = mpsc::channel();
    app.library.image_paste_receiver = Some(receiver);
    app.library.status = format!("Pasting {}…", clipboard.paste_label());
    let repaint = context.clone();
    #[cfg(target_os = "android")]
    let android_app = app.library.android_app.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-library-paste".to_owned())
        .spawn(move || {
            let completion = run_image_paste(
                clipboard,
                destination,
                #[cfg(target_os = "android")]
                &android_app,
            );
            let _ = sender.send(completion);
            repaint.request_repaint();
        });
    if let Err(error) = spawn {
        app.library.image_paste_receiver = None;
        app.library.status = format!("Could not start Library paste: {error}");
    }
}
