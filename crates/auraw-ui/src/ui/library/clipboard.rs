use super::*;

pub(super) fn cloud_batch_summary(
    verb: &str,
    total: usize,
    completed: usize,
    errors: Vec<String>,
) -> Result<String, String> {
    let noun = if total == 1 { "RAW" } else { "RAWs" };
    if errors.is_empty() {
        Ok(format!("{verb} {completed} cloud {noun}."))
    } else {
        Err(format!(
            "{verb} {completed} of {total} cloud {noun}. {}",
            errors.join(" · ")
        ))
    }
}

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

pub(super) fn run_image_paste(
    config: &crate::cloud::CloudConfig,
    cache_root: Option<&Path>,
    allow_network: bool,
    clipboard: ImageClipboard,
    destination: ImagePasteDestination,
    #[cfg(target_os = "android")] android_app: &auraw_ffi::AndroidApp,
) -> ImagePasteCompletion {
    let mode = clipboard.mode;
    // A Cut can succeed for only part of a multi-selection. Keep a private
    // copy and remove each item only after its complete move has committed so
    // retrying the paste never acts on sources that already moved.
    let mut remaining_cut_clipboard = (mode == ImageClipboardMode::Cut).then(|| clipboard.clone());
    let result = match (clipboard.content, destination) {
        #[cfg(not(target_os = "android"))]
        (ImageClipboardContent::Local(paths), ImagePasteDestination::LocalFolder(folder)) => {
            let total = paths.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for path in paths {
                let result = if mode == ImageClipboardMode::Cut
                    && path.parent() == Some(folder.as_path())
                {
                    Ok(())
                } else {
                    let name = path
                        .file_name()
                        .ok_or_else(|| format!("{} has no usable filename", path.display()));
                    name.and_then(|name| {
                        copy_raw_bundle_to_folder(&path, name, &folder).and_then(|destination| {
                            if mode == ImageClipboardMode::Cut {
                                if let Err(error) = remove_local_raw_bundle(&path) {
                                    let _ = remove_local_raw_bundle(&destination);
                                    return Err(error);
                                }
                            }
                            Ok(())
                        })
                    })
                };
                match result {
                    Ok(_) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate != &path);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", path.display())),
                }
            }
            image_paste_summary(
                mode,
                total,
                completed,
                &folder.display().to_string(),
                errors,
            )
        }
        #[cfg(not(target_os = "android"))]
        (ImageClipboardContent::Local(paths), ImagePasteDestination::CloudFolder(folder_id)) => {
            let total = paths.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for path in paths {
                let label = path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("local RAW")
                    .to_owned();
                let result = crate::cloud::upload_asset_path_to_folder(config, &path, &folder_id)
                    .and_then(|uploaded| {
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = remove_local_raw_bundle(&path) {
                                let rollback = crate::cloud::delete_asset(config, &uploaded);
                                return Err(if let Err(rollback) = rollback {
                                    format!("{error} The uploaded rollback also failed: {rollback}")
                                } else {
                                    error
                                });
                            }
                        }
                        Ok(())
                    });
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate != &path);
                        }
                    }
                    Err(error) => errors.push(format!("{label}: {error}")),
                }
            }
            image_paste_summary(mode, total, completed, "AuRaw Cloud", errors)
        }
        #[cfg(target_os = "android")]
        (ImageClipboardContent::Local(items), ImagePasteDestination::LocalLibrary) => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                let result = if mode == ImageClipboardMode::Cut {
                    Ok(())
                } else {
                    crate::android::duplicate_library_document(
                        android_app,
                        &item.uri,
                        &item.display_name,
                    )
                    .map(|_| ())
                };
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate.uri != item.uri);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", item.display_name)),
                }
            }
            image_paste_summary(mode, total, completed, "the local library", errors)
        }
        #[cfg(target_os = "android")]
        (ImageClipboardContent::Local(items), ImagePasteDestination::CloudFolder(folder_id)) => {
            let total = items.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for item in items {
                let staged_sidecar = crate::android::materialize_raw_sidecar(
                    android_app,
                    &item.uri,
                    &item.display_name,
                );
                let result = staged_sidecar.and_then(|staged_sidecar| {
                    let developed_thumbnail = if staged_sidecar.is_some() {
                        crate::android::developed_thumbnail_cache_file(
                            android_app,
                            &item.uri,
                            &item.display_name,
                        )
                    } else {
                        Ok(None)
                    }?;
                    let upload = (|| {
                        let raw =
                            crate::android::open_document_for_cloud_upload(android_app, &item.uri)?;
                        crate::cloud::upload_asset_file_with_sidecar_and_thumbnail_to_folder(
                            config,
                            raw,
                            &item.display_name,
                            Some(item.bytes),
                            staged_sidecar.as_deref(),
                            developed_thumbnail.as_deref(),
                            &folder_id,
                        )
                    })();
                    if let Some(path) = staged_sidecar.as_deref() {
                        let _ = std::fs::remove_file(path);
                    }
                    upload.and_then(|uploaded| {
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = crate::android::delete_library_document(
                                android_app,
                                &item.uri,
                                &item.display_name,
                            ) {
                                let rollback = crate::cloud::delete_asset(config, &uploaded);
                                return Err(if let Err(rollback) = rollback {
                                    format!("{error} The uploaded rollback also failed: {rollback}")
                                } else {
                                    error
                                });
                            }
                        }
                        Ok(())
                    })
                });
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Local(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate.uri != item.uri);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", item.display_name)),
                }
            }
            image_paste_summary(mode, total, completed, "AuRaw Cloud", errors)
        }
        (ImageClipboardContent::Cloud(assets), ImagePasteDestination::CloudFolder(folder_id)) => {
            let total = assets.len();
            let mut completed = 0usize;
            let mut errors = Vec::new();
            for asset in assets {
                let result = if mode == ImageClipboardMode::Copy {
                    crate::cloud::copy_asset(config, &asset, &folder_id).map(|_| ())
                } else {
                    crate::cloud::update_asset(config, &asset, &folder_id, &asset.name).map(|_| ())
                };
                match result {
                    Ok(()) => {
                        completed += 1;
                        if let Some(ImageClipboard {
                            content: ImageClipboardContent::Cloud(remaining),
                            ..
                        }) = remaining_cut_clipboard.as_mut()
                        {
                            remaining.retain(|candidate| candidate.id != asset.id);
                        }
                    }
                    Err(error) => errors.push(format!("{}: {error}", asset.name)),
                }
            }
            image_paste_summary(mode, total, completed, "AuRaw Cloud", errors)
        }
        #[cfg(not(target_os = "android"))]
        (ImageClipboardContent::Cloud(assets), ImagePasteDestination::LocalFolder(folder)) => {
            let total = assets.len();
            let result = (|| {
                let cache_root = cache_root
                    .ok_or_else(|| "AuRaw could not locate its private cloud cache.".to_owned())?;
                let cached = crate::cloud::open_assets(config, cache_root, &assets, allow_network)?;
                let mut completed = 0usize;
                let mut errors = Vec::new();
                for (asset, cached) in assets.iter().zip(cached) {
                    let copied = copy_raw_bundle_to_folder(
                        &cached.raw_path,
                        std::ffi::OsStr::new(&asset.name),
                        &folder,
                    )
                    .and_then(|destination| {
                        let destination_sidecar =
                            crate::sidecar::sidecar_path_for_raw(&destination);
                        let has_developed_thumbnail =
                            crate::sidecar::developed_thumbnail_cache_is_fresh(&destination)?;
                        if destination_sidecar.is_file() && !has_developed_thumbnail {
                            let thumbnail = crate::cloud::load_thumbnail(
                                config,
                                cache_root,
                                asset,
                                THUMBNAIL_EDGE,
                                allow_network,
                            )?;
                            let fingerprint =
                                crate::sidecar::desktop_sidecar_fingerprint(&destination)?
                                    .ok_or_else(|| {
                                        "The copied cloud sidecar disappeared before its thumbnail was saved."
                                            .to_owned()
                                    })?;
                            if let Err(error) = crate::sidecar::save_developed_thumbnail_cache(
                                &destination,
                                &thumbnail,
                                fingerprint,
                            ) {
                                let _ = remove_local_raw_bundle(&destination);
                                return Err(error);
                            }
                        }
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = crate::cloud::delete_asset(config, asset) {
                                let _ = remove_local_raw_bundle(&destination);
                                return Err(error);
                            }
                        }
                        Ok(())
                    });
                    match copied {
                        Ok(()) => {
                            completed += 1;
                            if let Some(ImageClipboard {
                                content: ImageClipboardContent::Cloud(remaining),
                                ..
                            }) = remaining_cut_clipboard.as_mut()
                            {
                                remaining.retain(|candidate| candidate.id != asset.id);
                            }
                        }
                        Err(error) => errors.push(format!("{}: {error}", asset.name)),
                    }
                }
                image_paste_summary(
                    mode,
                    total,
                    completed,
                    &folder.display().to_string(),
                    errors,
                )
            })();
            result
        }
        #[cfg(target_os = "android")]
        (ImageClipboardContent::Cloud(assets), ImagePasteDestination::LocalLibrary) => {
            let total = assets.len();
            let result = (|| {
                let cache_root = cache_root
                    .ok_or_else(|| "AuRaw could not locate its private cloud cache.".to_owned())?;
                let cached = crate::cloud::open_assets(config, cache_root, &assets, allow_network)?;
                let mut completed = 0usize;
                let mut errors = Vec::new();
                for (asset, cached) in assets.iter().zip(cached) {
                    let thumbnail =
                        if crate::sidecar::sidecar_path_for_raw(&cached.raw_path).is_file() {
                            crate::cloud::load_thumbnail(
                                config,
                                cache_root,
                                asset,
                                THUMBNAIL_EDGE,
                                allow_network,
                            )
                            .map(Some)
                        } else {
                            Ok(None)
                        };
                    let thumbnail = match thumbnail {
                        Ok(thumbnail) => thumbnail,
                        Err(error) => {
                            errors.push(format!("{}: {error}", asset.name));
                            continue;
                        }
                    };
                    let copied = crate::android::import_cached_library_document(
                        android_app,
                        &cached.raw_path,
                        &asset.name,
                    )
                    .and_then(|imported| {
                        if let Some(thumbnail) = thumbnail.as_ref() {
                            if let Err(error) = crate::android::save_developed_thumbnail_cache(
                                android_app,
                                &imported.uri,
                                &imported.display_name,
                                thumbnail,
                            ) {
                                let rollback = crate::android::delete_imported_library_document(
                                    android_app,
                                    &imported.uri,
                                    &imported.display_name,
                                );
                                return Err(if let Err(rollback) = rollback {
                                    format!(
                                        "{error} The imported-copy rollback also failed: {rollback}"
                                    )
                                } else {
                                    error
                                });
                            }
                        }
                        if mode == ImageClipboardMode::Cut {
                            if let Err(error) = crate::cloud::delete_asset(config, asset) {
                                let rollback = crate::android::delete_imported_library_document(
                                    android_app,
                                    &imported.uri,
                                    &imported.display_name,
                                );
                                return Err(if let Err(rollback) = rollback {
                                    format!(
                                        "{error} The imported-copy rollback also failed: {rollback}"
                                    )
                                } else {
                                    error
                                });
                            }
                        }
                        Ok(())
                    });
                    match copied {
                        Ok(()) => {
                            completed += 1;
                            if let Some(ImageClipboard {
                                content: ImageClipboardContent::Cloud(remaining),
                                ..
                            }) = remaining_cut_clipboard.as_mut()
                            {
                                remaining.retain(|candidate| candidate.id != asset.id);
                            }
                        }
                        Err(error) => errors.push(format!("{}: {error}", asset.name)),
                    }
                }
                image_paste_summary(mode, total, completed, "the local library", errors)
            })();
            result
        }
    };
    let clear_clipboard = remaining_cut_clipboard
        .as_ref()
        .is_some_and(|clipboard| clipboard.count() == 0);
    let remaining_clipboard = remaining_cut_clipboard.filter(|clipboard| clipboard.count() > 0);
    ImagePasteCompletion {
        result,
        clear_clipboard,
        remaining_clipboard,
    }
}

