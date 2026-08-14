use super::super::*;
use std::collections::BinaryHeap;

pub(in crate::ui::library) struct RankedLibraryFile {
    info: LibraryFileInfo,
    lowercase_name: String,
}

impl RankedLibraryFile {
    fn new(info: LibraryFileInfo) -> Self {
        let lowercase_name = info.name.to_lowercase();
        Self {
            info,
            lowercase_name,
        }
    }
}

impl PartialEq for RankedLibraryFile {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}

impl Eq for RankedLibraryFile {}

impl PartialOrd for RankedLibraryFile {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedLibraryFile {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // This is also the final display order: newest first, then stable
        // lexical tie-breakers. BinaryHeap therefore keeps the worst retained
        // item at its root, where a better candidate can replace it.
        other
            .info
            .modified
            .cmp(&self.info.modified)
            .then_with(|| self.lowercase_name.cmp(&other.lowercase_name))
            .then_with(|| self.info.display_path.cmp(&other.info.display_path))
            .then_with(|| match (&self.info.source, &other.info.source) {
                (LibrarySource::File(left), LibrarySource::File(right)) => left.cmp(right),
                _ => self.info.display_path.cmp(&other.info.display_path),
            })
    }
}

pub(in crate::ui::library) type FolderScan = (Vec<LibraryFileInfo>, usize, bool);

pub(in crate::ui::library) fn scan_folder_tree(root: &Path, is_cancelled: impl Fn() -> bool) -> Option<LibraryFolderNode> {
    fn visit(path: PathBuf, is_cancelled: &impl Fn() -> bool) -> Option<LibraryFolderNode> {
        if is_cancelled() {
            return None;
        }

        let mut node = LibraryFolderNode::empty(path.clone());
        let entries = match std::fs::read_dir(&path) {
            Ok(entries) => entries,
            Err(error) => {
                log::warn!(
                    "could not read library folder hierarchy at {}: {error}",
                    path.display()
                );
                return Some(node);
            }
        };

        for entry in entries {
            if is_cancelled() {
                return None;
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    log::warn!("could not read a library folder entry: {error}");
                    continue;
                }
            };
            let file_type = match entry.file_type() {
                Ok(file_type) => file_type,
                Err(error) => {
                    log::warn!("could not inspect {}: {error}", entry.path().display());
                    continue;
                }
            };
            // Directory symlinks are not followed, so a cycle cannot make the
            // desktop folder hierarchy recurse forever.
            if !file_type.is_dir() {
                continue;
            }
            if let Some(child) = visit(entry.path(), is_cancelled) {
                node.children.push(child);
            } else {
                return None;
            }
        }

        node.children.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.path.cmp(&right.path))
        });
        Some(node)
    }

    visit(root.to_path_buf(), &is_cancelled)
}

pub(in crate::ui::library) fn scan_folder(
    folder: &Path,
    is_cancelled: impl Fn() -> bool,
) -> Result<Option<FolderScan>, String> {
    scan_folder_with_limit(folder, MAX_LIBRARY_FILES, is_cancelled)
}

pub(in crate::ui::library) fn scan_folder_with_limit(
    folder: &Path,
    maximum_files: usize,
    is_cancelled: impl Fn() -> bool,
) -> Result<Option<FolderScan>, String> {
    if is_cancelled() {
        return Ok(None);
    }
    let metadata = std::fs::metadata(folder)
        .map_err(|error| format!("Could not read {}: {error}", folder.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }

    let mut files = BinaryHeap::with_capacity(maximum_files);
    let mut warning_count = 0usize;
    let mut truncated = false;
    let entries = std::fs::read_dir(folder)
        .map_err(|error| format!("Could not scan {}: {error}", folder.display()))?;
    for entry in entries {
        if is_cancelled() {
            return Ok(None);
        }
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                warning_count += 1;
                log::warn!("could not read a library directory entry: {error}");
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                warning_count += 1;
                log::warn!("could not inspect {}: {error}", entry.path().display());
                continue;
            }
        };
        // Only direct children of the selected folder belong to this view.
        // Symlinks and subdirectories are deliberately ignored.
        if !file_type.is_file() || !is_supported_raw_path(&entry.path()) {
            continue;
        }
        let path = entry.path();
        let file_metadata = entry.metadata().ok();
        let candidate = RankedLibraryFile::new(LibraryFileInfo {
            source: LibrarySource::File(path.clone()),
            display_path: path.display().to_string(),
            name: entry.file_name().to_string_lossy().into_owned(),
            bytes: file_metadata.as_ref().map_or(0, std::fs::Metadata::len),
            dimensions_hint: None,
            cloud_downloaded: false,
            cloud_sync_state: crate::cloud::CloudSyncState::Synced,
            modified: file_metadata.and_then(|metadata| metadata.modified().ok()),
        });
        if files.len() < maximum_files {
            files.push(candidate);
        } else {
            truncated = true;
            if files
                .peek()
                .is_some_and(|worst_retained| candidate < *worst_retained)
            {
                files.pop();
                files.push(candidate);
            }
        }
    }
    let mut files = files.into_vec();
    files.sort();
    let mut files = files
        .into_iter()
        .map(|ranked| ranked.info)
        .collect::<Vec<_>>();

    // Reserve the final gallery geometry before any preview pixels arrive.
    // LibRaw can expose display-oriented active dimensions from the header
    // after open_file/identify without unpacking the sensor or decoding a
    // thumbnail, so placeholders start at the same aspect ratio as the image.
    for info in &mut files {
        if is_cancelled() {
            return Ok(None);
        }
        if let LibrarySource::File(path) = &info.source {
            info.dimensions_hint = load_raw_display_dimensions(path).ok();
        }
    }

    Ok(Some((files, warning_count, truncated)))
}
