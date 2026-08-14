use super::super::*;

pub(in crate::ui::library) fn duplicate_raw_and_sidecar(raw_path: &Path) -> Result<PathBuf, String> {
    let parent = raw_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let stem = raw_path
        .file_stem()
        .map(OsString::from)
        .or_else(|| raw_path.file_name().map(OsString::from))
        .ok_or_else(|| "RAW path has no file name".to_owned())?;
    let extension = raw_path.extension().map(OsString::from);

    for number in 1..=10_000usize {
        let mut file_name = stem.clone();
        if number == 1 {
            file_name.push(" copy");
        } else {
            file_name.push(format!(" copy {number}"));
        }
        if let Some(extension) = &extension {
            file_name.push(".");
            file_name.push(extension);
        }
        let destination = parent.join(file_name);
        if crate::sidecar::sidecar_path_for_raw(&destination).exists() {
            continue;
        }

        match copy_file_create_new(raw_path, &destination) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not duplicate {}: {error}",
                    raw_path.display()
                ));
            }
        }

        let source_sidecar = crate::sidecar::sidecar_path_for_raw(raw_path);
        if source_sidecar.is_file() {
            let destination_sidecar = crate::sidecar::sidecar_path_for_raw(&destination);
            if let Err(error) = copy_file_create_new(&source_sidecar, &destination_sidecar) {
                let _ = fs::remove_file(&destination);
                let _ = fs::remove_file(&destination_sidecar);
                return Err(format!(
                    "Duplicated RAW cleanup completed after the sidecar copy failed: {error}"
                ));
            }
        }
        if let Err(error) = crate::sidecar::copy_developed_thumbnail_cache(raw_path, &destination) {
            let _ = fs::remove_file(&destination);
            let _ = fs::remove_file(crate::sidecar::sidecar_path_for_raw(&destination));
            let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&destination);
            return Err(format!("Could not copy the developed thumbnail: {error}"));
        }
        return Ok(destination);
    }

    Err("Could not find an unused duplicate file name.".to_owned())
}

pub(in crate::ui::library) fn copy_file_create_new(source: &Path, destination: &Path) -> io::Result<()> {
    let mut input = OpenOptions::new().read(true).open(source)?;
    let mut output = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let result = io::copy(&mut input, &mut output).and_then(|_| output.sync_all());
    if result.is_err() {
        drop(output);
        let _ = fs::remove_file(destination);
    } else if let Ok(metadata) = fs::metadata(source) {
        let _ = fs::set_permissions(destination, metadata.permissions());
    }
    result.map(|_| ())
}

pub(in crate::ui::library) fn copy_raw_bundle_to_folder(
    source_raw: &Path,
    requested_name: &std::ffi::OsStr,
    destination_folder: &Path,
) -> Result<PathBuf, String> {
    let requested_path = Path::new(requested_name);
    if requested_path.file_name() != Some(requested_name)
        || !crate::pipeline::is_supported_raw_path(requested_path)
    {
        return Err("The RAW has an unsafe or unsupported filename.".to_owned());
    }
    if !source_raw.is_file() {
        return Err(format!("{} is no longer a file.", source_raw.display()));
    }
    if !destination_folder.is_dir() {
        return Err(format!(
            "The destination folder {} no longer exists.",
            destination_folder.display()
        ));
    }

    let stem = requested_path
        .file_stem()
        .map(OsString::from)
        .unwrap_or_else(|| requested_name.to_os_string());
    let extension = requested_path.extension().map(OsString::from);
    let source_sidecar = crate::sidecar::sidecar_path_for_raw(source_raw);
    for number in 0..=10_000usize {
        let file_name = if number == 0 {
            requested_name.to_os_string()
        } else {
            let mut candidate = stem.clone();
            candidate.push(format!(" ({number})"));
            if let Some(extension) = &extension {
                candidate.push(".");
                candidate.push(extension);
            }
            candidate
        };
        let destination_raw = destination_folder.join(file_name);
        let destination_sidecar = crate::sidecar::sidecar_path_for_raw(&destination_raw);
        if destination_raw.exists() || destination_sidecar.exists() {
            continue;
        }
        match copy_file_create_new(source_raw, &destination_raw) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!("Could not copy {}: {error}", source_raw.display()));
            }
        }
        if source_sidecar.is_file() {
            if let Err(error) = copy_file_create_new(&source_sidecar, &destination_sidecar) {
                let _ = fs::remove_file(&destination_raw);
                let _ = fs::remove_file(&destination_sidecar);
                return Err(format!("Could not copy the matching sidecar: {error}"));
            }
        }
        if let Err(error) =
            crate::sidecar::copy_developed_thumbnail_cache(source_raw, &destination_raw)
        {
            let _ = fs::remove_file(&destination_raw);
            let _ = fs::remove_file(&destination_sidecar);
            let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&destination_raw);
            return Err(format!("Could not copy the developed thumbnail: {error}"));
        }
        if let Err(error) = crate::file_ops::sync_parent_directory(destination_folder) {
            log::warn!(
                "could not sync image paste folder {} after copying {}: {error}",
                destination_folder.display(),
                destination_raw.display()
            );
        }
        return Ok(destination_raw);
    }
    Err(format!(
        "Could not find an unused name for {:?} in {}.",
        requested_name,
        destination_folder.display()
    ))
}

pub(in crate::ui::library) fn remove_local_raw_bundle(raw_path: &Path) -> Result<(), String> {
    fs::remove_file(raw_path).map_err(|error| {
        format!(
            "Could not remove {} after copying it: {error}",
            raw_path.display()
        )
    })?;
    let sidecar = crate::sidecar::sidecar_path_for_raw(raw_path);
    if let Err(error) = fs::remove_file(&sidecar) {
        if error.kind() != io::ErrorKind::NotFound {
            log::warn!(
                "could not remove the old sidecar {} after moving its RAW: {error}",
                sidecar.display()
            );
        }
    }
    if let Err(error) = crate::sidecar::invalidate_developed_thumbnail_cache(raw_path) {
        log::warn!("could not remove the old developed thumbnail after moving a RAW: {error}");
    }
    Ok(())
}

pub(in crate::ui::library) fn rename_raw_bundle(source_raw: &Path, requested_name: &str) -> Result<PathBuf, String> {
    validate_cloud_item_name(requested_name, true)?;
    let parent = source_raw
        .parent()
        .ok_or_else(|| "The RAW has no parent folder.".to_owned())?;
    let destination_raw = parent.join(requested_name);
    if destination_raw == source_raw {
        return Ok(destination_raw);
    }
    let source_sidecar = crate::sidecar::sidecar_path_for_raw(source_raw);
    let destination_sidecar = crate::sidecar::sidecar_path_for_raw(&destination_raw);
    if destination_raw.exists() || destination_sidecar.exists() {
        return Err(format!("{} already exists.", destination_raw.display()));
    }
    let developed_thumbnail = crate::sidecar::load_developed_thumbnail_cache(source_raw, 8192)?;
    fs::rename(source_raw, &destination_raw).map_err(|error| {
        format!(
            "Could not rename {} to {}: {error}",
            source_raw.display(),
            destination_raw.display()
        )
    })?;
    if source_sidecar.is_file() {
        if let Err(error) = fs::rename(&source_sidecar, &destination_sidecar) {
            let rollback = fs::rename(&destination_raw, source_raw);
            return Err(if rollback.is_ok() {
                format!("Could not rename the matching sidecar: {error}")
            } else {
                format!(
                    "The RAW was renamed to {}, but its sidecar could not be renamed: {error}",
                    destination_raw.display()
                )
            });
        }
    }
    if let Some(thumbnail) = developed_thumbnail {
        let thumbnail_result = crate::sidecar::desktop_sidecar_fingerprint(&destination_raw)
            .and_then(|fingerprint| {
                fingerprint.ok_or_else(|| "The renamed RAW's edit sidecar disappeared.".to_owned())
            })
            .and_then(|fingerprint| {
                crate::sidecar::save_developed_thumbnail_cache(
                    &destination_raw,
                    &thumbnail,
                    fingerprint,
                )
                .map(|_| ())
            });
        if let Err(error) = thumbnail_result {
            let sidecar_rollback = if destination_sidecar.is_file() {
                fs::rename(&destination_sidecar, &source_sidecar)
            } else {
                Ok(())
            };
            let raw_rollback = fs::rename(&destination_raw, source_raw);
            let _ = crate::sidecar::invalidate_developed_thumbnail_cache(&destination_raw);
            return Err(if sidecar_rollback.is_ok() && raw_rollback.is_ok() {
                format!("Could not preserve the developed thumbnail while renaming: {error}")
            } else {
                format!(
                    "The RAW was renamed to {}, but its developed thumbnail and rename rollback failed: {error}",
                    destination_raw.display()
                )
            });
        }
    }
    if let Err(error) = crate::sidecar::invalidate_developed_thumbnail_cache(source_raw) {
        log::warn!("could not clear the old thumbnail after renaming a RAW: {error}");
    }
    if let Err(error) = crate::file_ops::sync_parent_directory(parent) {
        log::warn!(
            "could not sync RAW folder {} after renaming {}: {error}",
            parent.display(),
            destination_raw.display()
        );
    }
    Ok(destination_raw)
}

pub(in crate::ui::library) fn import_raw_into_folder(source: &Path, folder: &Path) -> Result<RawImportOutcome, String> {
    let original_name = source
        .file_name()
        .map(OsString::from)
        .ok_or_else(|| format!("{} has no file name", source.display()))?;
    let stem = source
        .file_stem()
        .map(OsString::from)
        .unwrap_or_else(|| original_name.clone());
    let extension = source.extension().map(OsString::from);

    for number in 0..=10_000usize {
        let file_name = if number == 0 {
            original_name.clone()
        } else {
            let mut file_name = stem.clone();
            file_name.push(format!(" ({number})"));
            if let Some(extension) = &extension {
                file_name.push(".");
                file_name.push(extension);
            }
            file_name
        };
        let destination = folder.join(file_name);

        if destination.exists() {
            if same_existing_file(source, &destination) {
                return Ok(RawImportOutcome::AlreadyPresent);
            }
            continue;
        }

        match copy_file_create_new(source, &destination) {
            Ok(()) => {
                if let Err(error) = crate::file_ops::sync_parent_directory(folder) {
                    log::warn!(
                        "could not sync RAW import folder {} after copying {}: {error}",
                        folder.display(),
                        destination.display()
                    );
                }
                return Ok(RawImportOutcome::Imported(destination));
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(format!(
                    "Could not import {} into {}: {error}",
                    source.display(),
                    folder.display()
                ));
            }
        }
    }

    Err(format!(
        "Could not find an unused name for {} in {}",
        source.display(),
        folder.display()
    ))
}

pub(in crate::ui::library) fn same_existing_file(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub(in crate::ui::library) fn validate_folder_name(name: &str) -> Result<OsString, String> {
    if name.is_empty() || name.trim() != name {
        return Err("Folder names cannot be empty or start/end with whitespace.".to_owned());
    }
    let path = Path::new(name);
    let mut components = path.components();
    let Some(std::path::Component::Normal(component)) = components.next() else {
        return Err("Enter a single folder name, without a path.".to_owned());
    };
    if components.next().is_some() {
        return Err("Enter a single folder name, without a path.".to_owned());
    }
    Ok(component.to_os_string())
}

pub(in crate::ui::library) fn canonical_library_directory(
    root: &Path,
    path: &Path,
    allow_root: bool,
) -> Result<PathBuf, String> {
    let root = fs::canonicalize(root)
        .map_err(|error| format!("Could not resolve library root {}: {error}", root.display()))?;
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("Could not inspect folder {}: {error}", path.display()))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(format!("{} is not a real folder", path.display()));
    }
    let resolved = fs::canonicalize(path)
        .map_err(|error| format!("Could not resolve folder {}: {error}", path.display()))?;
    if !resolved.starts_with(&root) {
        return Err(format!(
            "Refusing to operate outside the library root: {}",
            path.display()
        ));
    }
    if !allow_root && resolved == root {
        return Err(
            "The top-level library folder cannot be moved, renamed, or deleted.".to_owned(),
        );
    }
    Ok(resolved)
}

pub(in crate::ui::library) fn path_entry_exists(path: &Path) -> bool {
    match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) => error.kind() != io::ErrorKind::NotFound,
    }
}

pub(in crate::ui::library) fn unique_folder_destination(parent: &Path, name: &std::ffi::OsStr) -> Result<PathBuf, String> {
    for number in 0..=10_000usize {
        let file_name = if number == 0 {
            OsString::from(name)
        } else {
            let mut copy_name = OsString::from(name);
            if number == 1 {
                copy_name.push(" copy");
            } else {
                copy_name.push(format!(" copy {number}"));
            }
            copy_name
        };
        let destination = parent.join(file_name);
        if !path_entry_exists(&destination) {
            return Ok(destination);
        }
    }
    Err(format!(
        "Could not find an unused folder name in {}",
        parent.display()
    ))
}

pub(in crate::ui::library) fn copy_directory_create_new(source: &Path, destination: &Path) -> Result<(), String> {
    let source_metadata = fs::symlink_metadata(source)
        .map_err(|error| format!("Could not inspect {}: {error}", source.display()))?;
    if source_metadata.file_type().is_symlink() || !source_metadata.is_dir() {
        return Err(format!("{} is not a real folder", source.display()));
    }
    let source_resolved = fs::canonicalize(source)
        .map_err(|error| format!("Could not resolve {}: {error}", source.display()))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| format!("{} has no parent folder", destination.display()))?;
    let destination_parent_resolved = fs::canonicalize(destination_parent).map_err(|error| {
        format!(
            "Could not resolve destination {}: {error}",
            destination_parent.display()
        )
    })?;
    if destination_parent_resolved.starts_with(&source_resolved) {
        return Err("A folder cannot be copied into itself or one of its subfolders.".to_owned());
    }

    fn copy_contents(source: &Path, destination: &Path) -> Result<(), String> {
        for entry in fs::read_dir(source)
            .map_err(|error| format!("Could not read {}: {error}", source.display()))?
        {
            let entry = entry.map_err(|error| {
                format!("Could not read an entry in {}: {error}", source.display())
            })?;
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let file_type = entry
                .file_type()
                .map_err(|error| format!("Could not inspect {}: {error}", source_path.display()))?;
            if file_type.is_symlink() {
                return Err(format!(
                    "Refusing to follow symbolic link {} while copying a folder",
                    source_path.display()
                ));
            }
            if file_type.is_dir() {
                fs::create_dir(&destination_path).map_err(|error| {
                    format!("Could not create {}: {error}", destination_path.display())
                })?;
                copy_contents(&source_path, &destination_path)?;
                if let Err(error) = crate::file_ops::sync_parent_directory(&destination_path) {
                    log::warn!(
                        "could not sync copied folder {}: {error}",
                        destination_path.display()
                    );
                }
            } else if file_type.is_file() {
                copy_file_create_new(&source_path, &destination_path).map_err(|error| {
                    format!("Could not copy {}: {error}", source_path.display())
                })?;
            } else {
                return Err(format!(
                    "Refusing to copy special filesystem entry {}",
                    source_path.display()
                ));
            }
        }
        Ok(())
    }

    fs::create_dir(destination)
        .map_err(|error| format!("Could not create {}: {error}", destination.display()))?;
    if let Err(error) = copy_contents(source, destination) {
        if let Err(cleanup_error) = fs::remove_dir_all(destination) {
            return Err(format!(
                "{error} Cleanup of incomplete folder {} also failed: {cleanup_error}",
                destination.display()
            ));
        }
        return Err(error);
    }
    if let Err(error) = crate::file_ops::sync_parent_directory(destination_parent) {
        log::warn!(
            "could not sync folder {} after copying {}: {error}",
            destination_parent.display(),
            destination.display()
        );
    }
    Ok(())
}

pub(in crate::ui::library) fn import_folder_into_library(source: &Path, folder: &Path) -> Result<PathBuf, String> {
    let source_name = source
        .file_name()
        .ok_or_else(|| format!("{} has no folder name", source.display()))?;
    let destination = unique_folder_destination(folder, source_name)?;
    copy_directory_create_new(source, &destination).map_err(|error| {
        format!(
            "Could not import folder {} into {}: {error}",
            source.display(),
            folder.display()
        )
    })?;
    Ok(destination)
}

pub(in crate::ui::library) fn folder_operation_progress_status(operation: &LibraryFolderOperation) -> String {
    match operation {
        LibraryFolderOperation::Create { parent, .. } => {
            format!("Creating a folder in {}…", parent.display())
        }
        LibraryFolderOperation::Copy { source, .. } => {
            format!("Copying folder {}…", source.display())
        }
        LibraryFolderOperation::Move { source, .. } => {
            format!("Moving folder {}…", source.display())
        }
        LibraryFolderOperation::Delete { target, .. } => {
            format!("Deleting folder {}…", target.display())
        }
    }
}

pub(in crate::ui::library) fn run_folder_operation(
    operation: LibraryFolderOperation,
) -> Result<LibraryFolderOperationResult, String> {
    match operation {
        LibraryFolderOperation::Create { root, parent, name } => {
            canonical_library_directory(&root, &parent, true)?;
            let name = validate_folder_name(&name)?;
            let destination = parent.join(name);
            fs::create_dir(&destination).map_err(|error| {
                if error.kind() == io::ErrorKind::AlreadyExists {
                    format!("A folder named {} already exists.", destination.display())
                } else {
                    format!("Could not create folder {}: {error}", destination.display())
                }
            })?;
            if let Err(error) = crate::file_ops::sync_parent_directory(&parent) {
                log::warn!("could not sync folder {}: {error}", parent.display());
            }
            Ok(LibraryFolderOperationResult::Created(destination))
        }
        LibraryFolderOperation::Copy {
            root,
            source,
            destination_parent,
        } => {
            canonical_library_directory(&root, &source, false)?;
            canonical_library_directory(&root, &destination_parent, true)?;
            let name = source
                .file_name()
                .ok_or_else(|| format!("{} has no folder name", source.display()))?;
            let destination = unique_folder_destination(&destination_parent, name)?;
            copy_directory_create_new(&source, &destination)?;
            Ok(LibraryFolderOperationResult::Copied {
                source,
                destination,
            })
        }
        LibraryFolderOperation::Move {
            root,
            source,
            destination_parent,
            new_name,
        } => {
            let source_resolved = canonical_library_directory(&root, &source, false)?;
            let destination_parent_resolved =
                canonical_library_directory(&root, &destination_parent, true)?;
            if destination_parent_resolved.starts_with(&source_resolved) {
                return Err(
                    "A folder cannot be moved into itself or one of its subfolders.".to_owned(),
                );
            }
            let name = match new_name {
                Some(name) => validate_folder_name(&name)?,
                None => source
                    .file_name()
                    .map(OsString::from)
                    .ok_or_else(|| format!("{} has no folder name", source.display()))?,
            };
            let destination = destination_parent.join(name);
            if destination == source {
                return Err("The folder is already in that location.".to_owned());
            }
            if path_entry_exists(&destination) {
                return Err(format!(
                    "A folder named {} already exists; nothing was overwritten.",
                    destination.display()
                ));
            }
            fs::rename(&source, &destination).map_err(|error| {
                format!(
                    "Could not move {} to {}: {error}",
                    source.display(),
                    destination.display()
                )
            })?;
            if let Some(parent) = source.parent() {
                if let Err(error) = crate::file_ops::sync_parent_directory(parent) {
                    log::warn!("could not sync source folder {}: {error}", parent.display());
                }
            }
            if let Err(error) = crate::file_ops::sync_parent_directory(&destination_parent) {
                log::warn!(
                    "could not sync destination folder {}: {error}",
                    destination_parent.display()
                );
            }
            Ok(LibraryFolderOperationResult::Moved {
                source,
                destination,
            })
        }
        LibraryFolderOperation::Delete { root, target } => {
            canonical_library_directory(&root, &target, false)?;
            fs::remove_dir_all(&target).map_err(|error| {
                format!("Could not delete folder {}: {error}", target.display())
            })?;
            if let Some(parent) = target.parent() {
                if let Err(error) = crate::file_ops::sync_parent_directory(parent) {
                    log::warn!(
                        "could not sync folder {} after deletion: {error}",
                        parent.display()
                    );
                }
            }
            Ok(LibraryFolderOperationResult::Deleted(target))
        }
    }
}

pub(in crate::ui::library) fn raw_import_status(result: &RawImportResult) -> String {
    let mut parts = Vec::new();
    if !result.imported.is_empty() {
        parts.push(format!(
            "Imported {} RAW {}",
            result.imported.len(),
            if result.imported.len() == 1 {
                "file"
            } else {
                "files"
            }
        ));
    }
    if !result.imported_folders.is_empty() {
        parts.push(format!(
            "Imported {} {}",
            result.imported_folders.len(),
            if result.imported_folders.len() == 1 {
                "folder"
            } else {
                "folders"
            }
        ));
    }
    if result.already_present > 0 {
        parts.push(format!("{} already in this folder", result.already_present));
    }
    if result.ignored > 0 {
        parts.push(format!(
            "ignored {} unsupported or inaccessible {}",
            result.ignored,
            if result.ignored == 1 { "item" } else { "items" }
        ));
    }
    if !result.failures.is_empty() {
        parts.push(format!("{} failed", result.failures.len()));
        parts.push(result.failures.join(" · "));
    }
    if parts.is_empty() {
        "No RAW files or folders were imported.".to_owned()
    } else {
        format!("{}.", parts.join("; "))
    }
}
