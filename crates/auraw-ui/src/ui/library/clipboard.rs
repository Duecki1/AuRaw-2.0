use super::*;

pub(super) fn image_paste_summary(
    mode: ImageClipboardMode,
    total: usize,
    completed: usize,
    destination: &str,
    errors: Vec<String>,
) -> Result<String, String> {
    let verb = if mode == ImageClipboardMode::Copy {
        "Copied"
    } else {
        "Moved"
    };
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

fn copy_asset_to_destination(
    asset: &LibraryAsset,
    destination: &LibraryTransferDestination,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> Result<ImportedLibraryAsset, String> {
    let materialized = materialize_library_asset(
        asset,
        #[cfg(target_os = "android")]
        android_app,
    )?;
    let result = import_materialized_library_asset(
        &materialized,
        destination,
        #[cfg(target_os = "android")]
        android_app,
    )
    .and_then(|imported| {
        if let Err(error) = preserve_imported_thumbnail(
            asset,
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
        } else {
            Ok(imported)
        }
    });
    materialized.cleanup();
    result
}

pub(super) fn run_image_paste(
    clipboard: ImageClipboard,
    destination: LibraryTransferDestination,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> AssetTransferCompletion {
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
        let result = copy_asset_to_destination(
            &asset,
            &destination,
            #[cfg(target_os = "android")]
            android_app,
        )
        .and_then(|imported| {
            if mode != ImageClipboardMode::Cut {
                return Ok(());
            }
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
        });

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
        LibraryTransferDestination::LocalFolder(folder) => folder.display().to_string(),
    };
    #[cfg(target_os = "android")]
    let destination_label = match &destination {
        LibraryTransferDestination::LocalLibrary { path } if !path.is_empty() => path.clone(),
        LibraryTransferDestination::LocalLibrary { .. } => "the Library".to_owned(),
    };

    let result = image_paste_summary(mode, total, completed, &destination_label, errors);
    let remaining_clipboard = if mode == ImageClipboardMode::Cut && !remaining.is_empty() {
        Some(ImageClipboard {
            mode,
            assets: remaining,
        })
    } else {
        None
    };
    AssetTransferCompletion {
        result,
        clear_clipboard: mode == ImageClipboardMode::Cut && remaining_clipboard.is_none(),
        remaining_clipboard,
    }
}

pub(super) fn run_duplicate_assets(
    assets: Vec<LibraryAsset>,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> AssetTransferCompletion {
    let total = assets.len();
    let mut completed = 0usize;
    let mut errors = Vec::new();
    for asset in assets {
        let result = duplicate_destination(&asset).and_then(|destination| {
            copy_asset_to_destination(
                &asset,
                &destination,
                #[cfg(target_os = "android")]
                android_app,
            )
            .map(|_| ())
        });
        match result {
            Ok(()) => completed += 1,
            Err(error) => errors.push(format!("{}: {error}", asset.display_name)),
        }
    }
    let noun = if total == 1 { "image" } else { "images" };
    let result = if errors.is_empty() {
        Ok(format!("Duplicated {completed} selected {noun}"))
    } else {
        Err(format!(
            "Duplicated {completed} of {total} selected images. {}",
            errors.join(" · ")
        ))
    };
    AssetTransferCompletion {
        result,
        clear_clipboard: false,
        remaining_clipboard: None,
    }
}

pub(super) fn start_duplicate_assets(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
    context: &egui::Context,
) {
    if app.library.local_mutation_in_progress() {
        app.library.status = "Another Library asset transfer is still running.".to_owned();
        return;
    }
    if assets.is_empty() {
        return;
    }
    let assets = assets.to_vec();
    let total = assets.len();
    let (sender, receiver) = mpsc::channel();
    app.library.asset_transfer_receiver = Some(receiver);
    app.library.status = if total == 1 {
        format!("Duplicating {}…", assets[0].display_name)
    } else {
        format!("Duplicating {total} selected images…")
    };
    let repaint = context.clone();
    #[cfg(target_os = "android")]
    let android_app = app.library.platform.app.clone();
    let spawn = std::thread::Builder::new()
        .name("auraw-library-duplicate".to_owned())
        .spawn(move || {
            let completion = run_duplicate_assets(
                assets,
                #[cfg(target_os = "android")]
                &android_app,
            );
            let _ = sender.send(completion);
            repaint.request_repaint();
        });
    if let Err(error) = spawn {
        app.library.asset_transfer_receiver = None;
        app.library.status = format!("Could not start duplicate operation: {error}");
    }
}

pub(super) fn start_image_clipboard_paste(
    app: &mut AurawApp,
    destination: LibraryTransferDestination,
    context: &egui::Context,
) {
    if app.library.local_mutation_in_progress() {
        app.library.status = "Another Library asset transfer is still running.".to_owned();
        return;
    }
    let Some(clipboard) = app.library.image_clipboard.clone() else {
        app.library.status = "Copy or cut Library images first.".to_owned();
        return;
    };
    let (sender, receiver) = mpsc::channel();
    app.library.asset_transfer_receiver = Some(receiver);
    app.library.status = format!("Pasting {}…", clipboard.paste_label());
    let repaint = context.clone();
    #[cfg(target_os = "android")]
    let android_app = app.library.platform.app.clone();
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
        app.library.asset_transfer_receiver = None;
        app.library.status = format!("Could not start Library paste: {error}");
    }
}
