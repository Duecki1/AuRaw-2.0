use super::*;

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

pub(super) fn reset_asset_adjustments(
    app: &mut AurawApp,
    asset: &LibraryAsset,
) -> Result<bool, String> {
    #[cfg(not(target_os = "android"))]
    let _ = app;

    #[cfg(not(target_os = "android"))]
    {
        let path = asset
            .desktop_path()
            .ok_or_else(|| "Library asset is not available from desktop storage".to_owned())?;
        crate::sidecar::reset_desktop_adjustments(path)
    }
    #[cfg(target_os = "android")]
    {
        let uri = asset
            .android_uri()
            .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
        app.reset_android_library_adjustments(uri, &asset.display_name)?;
        Ok(true)
    }
}

pub(super) fn rename_asset(
    app: &mut AurawApp,
    asset: &LibraryAsset,
    requested_name: &str,
) -> Result<LibraryAsset, String> {
    #[cfg(not(target_os = "android"))]
    let _ = app;

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

pub(super) fn delete_library_asset(app: &mut AurawApp, asset: &LibraryAsset) -> Result<(), String> {
    #[cfg(not(target_os = "android"))]
    {
        let path = asset
            .desktop_path()
            .ok_or_else(|| "Library asset is not available from desktop storage".to_owned())?;
        let was_current = app.develop.current_path.as_deref() == Some(path);
        if was_current {
            app.detach_current_file_for_library_action(path);
        }
        remove_local_raw_bundle(path)?;
        if was_current {
            app.develop.current_path = None;
        }
        Ok(())
    }
    #[cfg(target_os = "android")]
    {
        let uri = asset
            .android_uri()
            .ok_or_else(|| "Library asset is not available from Android storage".to_owned())?;
        app.delete_android_library_item(uri, &asset.display_name)
    }
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
                .map_err(|error| {
                    format!("could not stage {} sidecar: {error}", asset.display_name)
                });
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
    destination: &LibraryTransferDestination,
) -> bool {
    #[cfg(not(target_os = "android"))]
    {
        let LibraryTransferDestination::LocalFolder(folder) = destination;
        asset
            .desktop_path()
            .and_then(Path::parent)
            .is_some_and(|parent| parent == folder.as_path())
    }
    #[cfg(target_os = "android")]
    {
        let LibraryTransferDestination::LocalLibrary { path } = destination;
        Path::new(&asset.display_path)
            .parent()
            .is_some_and(|parent| parent == Path::new(path))
    }
}

pub(super) fn duplicate_destination(
    asset: &LibraryAsset,
) -> Result<LibraryTransferDestination, String> {
    #[cfg(not(target_os = "android"))]
    {
        let parent = asset
            .desktop_path()
            .and_then(Path::parent)
            .ok_or_else(|| "Library asset has no destination folder".to_owned())?;
        Ok(LibraryTransferDestination::LocalFolder(parent.to_owned()))
    }
    #[cfg(target_os = "android")]
    {
        let path = Path::new(&asset.display_path)
            .parent()
            .map(|parent| parent.display().to_string())
            .unwrap_or_default();
        Ok(LibraryTransferDestination::LocalLibrary { path })
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
    destination: &LibraryTransferDestination,
    #[cfg(target_os = "android")] app: &auraw_ffi::AndroidApp,
) -> Result<ImportedLibraryAsset, String> {
    #[cfg(not(target_os = "android"))]
    {
        let LibraryTransferDestination::LocalFolder(folder) = destination;
        let destination = copy_raw_bundle_to_folder(
            &materialized.raw_path,
            std::ffi::OsStr::new(&materialized.display_name),
            folder,
        )?;
        Ok(ImportedLibraryAsset::Desktop(destination))
    }
    #[cfg(target_os = "android")]
    {
        let LibraryTransferDestination::LocalLibrary { .. } = destination;
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
                log::warn!(
                    "could not roll back imported Library bundle {}: {error}",
                    path.display()
                );
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
