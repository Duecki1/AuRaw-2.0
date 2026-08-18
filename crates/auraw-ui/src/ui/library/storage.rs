use super::*;

/// Convert shared Library assets to the platform-native adjustment target at the
/// storage boundary. UI/action code above this module never branches on paths vs URIs.
#[cfg(not(target_os = "android"))]
pub(super) fn desktop_paths(assets: &[LibraryAsset]) -> Vec<PathBuf> {
    assets
        .iter()
        .filter_map(|asset| asset.desktop_path().map(Path::to_path_buf))
        .collect()
}

#[cfg(target_os = "android")]
pub(super) fn android_targets(assets: &[LibraryAsset]) -> Vec<(String, String)> {
    assets
        .iter()
        .filter_map(|asset| {
            asset
                .android_uri()
                .map(|uri| (uri.to_owned(), asset.display_name.clone()))
        })
        .collect()
}


pub(super) fn start_library_ai_mask_refresh(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
    frame: &eframe::Frame,
) {
    #[cfg(not(target_os = "android"))]
    app.start_library_ai_mask_refresh_paths(desktop_paths(assets), frame);
    #[cfg(target_os = "android")]
    app.start_library_ai_mask_refresh_android(android_targets(assets), frame);
}

pub(super) fn start_library_export(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
    settings: ExportSettings,
    format: ExportFormat,
    _frame: &eframe::Frame,
) -> bool {
    #[cfg(not(target_os = "android"))]
    {
        let Some(jobs) = library_export_jobs(&desktop_paths(assets), format) else {
            return false;
        };
        app.start_library_exports(jobs, settings, format, _frame);
        true
    }
    #[cfg(target_os = "android")]
    {
        let targets = android_targets(assets)
            .into_iter()
            .map(|(uri, display_name)| crate::app::AndroidLibraryExportTarget {
                uri,
                display_name,
            })
            .collect::<Vec<_>>();
        app.start_android_library_exports(targets, settings, format);
        true
    }
}

pub(super) fn copy_adjustments(app: &mut AurawApp, asset: &LibraryAsset) -> Result<(), String> {
    app.copy_library_adjustments(asset)
}

pub(super) fn adjustment_edit_count(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
) -> (usize, Vec<String>) {
    app.library_adjustment_edit_count(assets)
}

/// Apply the shared adjustment clipboard and return (completed, needs AI refresh, failures).
pub(super) fn paste_adjustments(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
    mode: crate::sidecar::AdjustmentPasteMode,
    frame: &eframe::Frame,
) -> (usize, Vec<LibraryAsset>, Vec<String>) {
    app.paste_library_adjustments(assets, mode, frame)
}

pub(super) fn reset_adjustments(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
) -> (usize, Vec<String>) {
    let mut changed = 0usize;
    let mut failures = Vec::new();
    #[cfg(not(target_os = "android"))]
    for asset in assets {
        let Some(path) = asset.desktop_path() else { continue };
        match crate::sidecar::reset_desktop_adjustments(path) {
            Ok(reset) => {
                app.library.invalidate_adjustment_thumbnail_for_path(path);
                changed += usize::from(reset);
            }
            Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
        }
    }
    #[cfg(target_os = "android")]
    for asset in assets {
        let Some(uri) = asset.android_uri() else { continue };
        match app.reset_android_library_adjustments(uri, &asset.display_name) {
            Ok(()) => {
                app.library.invalidate_android_adjustment_thumbnail(uri);
                changed += 1;
            }
            Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
        }
    }
    (changed, failures)
}

pub(super) fn duplicate_assets(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
    context: &egui::Context,
) {
    duplicate_materialized_assets(app, assets, context);
}

pub(super) fn rename_asset(
    app: &mut AurawApp,
    asset: &LibraryAsset,
    requested_name: &str,
) -> Result<LibraryAsset, String> {
    #[cfg(not(target_os = "android"))]
    {
        let path = asset
            .desktop_path()
            .ok_or_else(|| "Library asset is not available from desktop storage".to_owned())?;
        let destination = rename_raw_bundle(path, requested_name)?;
        let mut renamed = asset.clone();
        renamed.id = LibraryAssetId::Desktop(destination.clone());
        renamed.display_name = requested_name.to_owned();
        renamed.display_path = destination.display().to_string();
        renamed.locator = LibraryLocator::Desktop(destination);
        Ok(renamed)
    }
    #[cfg(target_os = "android")]
    {
        let uri = asset
            .android_uri()
            .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
        let renamed_uri =
            app.rename_android_library_item(uri, &asset.display_name, requested_name)?;
        let mut renamed = asset.clone();
        renamed.id = LibraryAssetId::Android(renamed_uri.clone());
        renamed.display_name = requested_name.to_owned();
        renamed.display_path = Path::new(&asset.display_path)
            .parent()
            .map(|parent| parent.join(requested_name).display().to_string())
            .unwrap_or_else(|| requested_name.to_owned());
        renamed.locator = LibraryLocator::Android { uri: renamed_uri };
        Ok(renamed)
    }
}

pub(super) fn delete_assets(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
) -> (usize, Vec<String>) {
    let total = assets.len();
    let mut deleted = 0usize;
    let mut failures = Vec::new();
    #[cfg(not(target_os = "android"))]
    {
        let current = app.current_path.clone();
        if let Some(path) = current.as_deref() {
            if assets.iter().any(|asset| asset.desktop_path() == Some(path)) {
                app.detach_current_file_for_library_action(path);
            }
        }
        for asset in assets {
            let Some(path) = asset.desktop_path() else { continue };
            match fs::remove_file(path) {
                Ok(()) => {
                    deleted += 1;
                    if current.as_deref() == Some(path) {
                        app.current_path = None;
                    }
                    if let Err(error) = crate::sidecar::remove_desktop_edits(path) {
                        failures.push(format!("{} sidecar: {error}", asset.display_name));
                    }
                }
                Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
            }
        }
    }
    #[cfg(target_os = "android")]
    for asset in assets {
        let Some(uri) = asset.android_uri() else { continue };
        match app.delete_android_library_item(uri, &asset.display_name) {
            Ok(()) => deleted += 1,
            Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
        }
    }
    debug_assert!(deleted <= total);
    (deleted, failures)
}

#[derive(Debug)]
pub(super) struct MaterializedLibraryAsset {
    pub(super) raw_path: PathBuf,
    pub(super) display_name: String,
    cleanup: bool,
}

impl MaterializedLibraryAsset {
    pub(super) fn cleanup(&self) {
        if !self.cleanup {
            return;
        }
        let _ = fs::remove_file(&self.raw_path);
        let _ = fs::remove_file(crate::sidecar::sidecar_path_for_raw(&self.raw_path));
    }
}

pub(super) fn materialize_library_asset(
    asset: &LibraryAsset,
    #[cfg(target_os = "android")] app: &auraw_ffi::AndroidApp,
) -> Result<MaterializedLibraryAsset, String> {
    #[cfg(not(target_os = "android"))]
    {
        let raw_path = asset
            .desktop_path()
            .ok_or_else(|| "Library asset is not available from desktop storage".to_owned())?
            .to_owned();
        Ok(MaterializedLibraryAsset {
            raw_path,
            display_name: asset.display_name.clone(),
            cleanup: false,
        })
    }
    #[cfg(target_os = "android")]
    {
        let uri = asset
            .android_uri()
            .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
        let raw_path = crate::android::materialize_library_document(app, uri, &asset.display_name)?;
        let sidecar = crate::android::materialize_raw_sidecar(app, uri, &asset.display_name)?;
        if let Some(sidecar) = sidecar {
            let destination = crate::sidecar::sidecar_path_for_raw(&raw_path);
            let copy_result = fs::copy(&sidecar, &destination)
                .map(|_| ())
                .map_err(|error| format!("could not stage {} sidecar: {error}", asset.display_name));
            let _ = fs::remove_file(sidecar);
            if let Err(error) = copy_result {
                let _ = fs::remove_file(&raw_path);
                return Err(error);
            }
        }
        Ok(MaterializedLibraryAsset {
            raw_path,
            display_name: asset.display_name.clone(),
            cleanup: true,
        })
    }
}

pub(super) fn asset_is_at_destination(
    asset: &LibraryAsset,
    destination: &ImagePasteDestination,
) -> bool {
    #[cfg(not(target_os = "android"))]
    {
        let ImagePasteDestination::LocalFolder(folder) = destination;
        asset
            .desktop_path()
            .and_then(Path::parent)
            .is_some_and(|parent| parent == folder.as_path())
    }
    #[cfg(target_os = "android")]
    {
        let ImagePasteDestination::LocalLibrary { path } = destination;
        Path::new(&asset.display_path)
            .parent()
            .is_some_and(|parent| parent == Path::new(path))
    }
}

#[derive(Debug)]
pub(super) enum ImportedLibraryAsset {
    #[cfg(not(target_os = "android"))]
    Desktop(PathBuf),
    #[cfg(target_os = "android")]
    Android(crate::android::ImportedLibraryDocument),
}

pub(super) fn import_materialized_library_asset(
    materialized: &MaterializedLibraryAsset,
    destination: &ImagePasteDestination,
    #[cfg(target_os = "android")] app: &auraw_ffi::AndroidApp,
) -> Result<ImportedLibraryAsset, String> {
    #[cfg(not(target_os = "android"))]
    {
        let ImagePasteDestination::LocalFolder(folder) = destination;
        let destination = copy_raw_bundle_to_folder(
            &materialized.raw_path,
            std::ffi::OsStr::new(&materialized.display_name),
            folder,
        )?;
        Ok(ImportedLibraryAsset::Desktop(destination))
    }
    #[cfg(target_os = "android")]
    {
        let ImagePasteDestination::LocalLibrary { .. } = destination;
        crate::android::import_local_library_document(
            app,
            &materialized.raw_path,
            &materialized.display_name,
        )
        .map(ImportedLibraryAsset::Android)
    }
}

pub(super) fn preserve_imported_thumbnail(
    asset: &LibraryAsset,
    imported: &ImportedLibraryAsset,
    #[cfg(target_os = "android")] app: &auraw_ffi::AndroidApp,
) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        let _ = (asset, imported);
        // Desktop copy_raw_bundle_to_folder already preserves the developed cache.
        Ok(())
    }
    #[cfg(target_os = "android")]
    {
        let source_uri = asset
            .android_uri()
            .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
        let ImportedLibraryAsset::Android(imported) = imported;
        crate::android::copy_library_developed_thumbnail_cache(app, source_uri, &imported.uri)
    }
}

pub(super) fn remove_library_asset(
    asset: &LibraryAsset,
    #[cfg(target_os = "android")] app: &auraw_ffi::AndroidApp,
) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        let path = asset
            .desktop_path()
            .ok_or_else(|| "Library asset is not available from desktop storage".to_owned())?;
        remove_local_raw_bundle(path)
    }
    #[cfg(target_os = "android")]
    {
        let uri = asset
            .android_uri()
            .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
        crate::android::delete_library_document(app, uri, &asset.display_name)
    }
}

pub(super) fn rollback_imported_library_asset(
    imported: ImportedLibraryAsset,
    #[cfg(target_os = "android")] app: &auraw_ffi::AndroidApp,
) {
    match imported {
        #[cfg(not(target_os = "android"))]
        ImportedLibraryAsset::Desktop(path) => {
            if let Err(error) = remove_local_raw_bundle(&path) {
                log::warn!("could not roll back imported Library bundle {}: {error}", path.display());
            }
        }
        #[cfg(target_os = "android")]
        ImportedLibraryAsset::Android(imported) => {
            if let Err(error) = crate::android::delete_imported_library_document(
                app,
                &imported.uri,
                &imported.display_name,
            ) {
                log::warn!("could not roll back imported Android Library bundle: {error}");
            }
        }
    }
}

pub(super) fn duplicate_materialized_assets(
    app: &mut AurawApp,
    assets: &[LibraryAsset],
    context: &egui::Context,
) {
    #[cfg(not(target_os = "android"))]
    {
        // Desktop can preserve its asynchronous filesystem implementation; it
        // already copies RAW+sidecar as a single storage operation.
        app.library.duplicate_raws_with_sidecars(desktop_paths(assets), context);
    }
    #[cfg(target_os = "android")]
    {
        let total = assets.len();
        let mut completed = 0usize;
        let mut failures = Vec::new();
        let android_app = app.library.android_app.clone();
        for asset in assets {
            let materialized = match materialize_library_asset(asset, &android_app) {
                Ok(materialized) => materialized,
                Err(error) => {
                    failures.push(format!("{}: {error}", asset.display_name));
                    continue;
                }
            };
            let result = import_materialized_library_asset(
                &materialized,
                &ImagePasteDestination::LocalLibrary { path: String::new() },
                &android_app,
            );
            let result = result.and_then(|imported| {
                preserve_imported_thumbnail(asset, &imported, &android_app).map_err(|error| {
                    rollback_imported_library_asset(imported, &android_app);
                    error
                })
            });
            materialized.cleanup();
            match result {
                Ok(()) => completed += 1,
                Err(error) => failures.push(format!("{}: {error}", asset.display_name)),
            }
        }
        app.library.status = if failures.is_empty() {
            format!("Duplicated {completed} selected {}", if completed == 1 { "image" } else { "images" })
        } else {
            format!("Duplicated {completed} of {total} selected images. {}", failures.join(" · "))
        };
        app.library.refresh(context);
    }
}
