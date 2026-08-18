use super::super::*;
use std::collections::BinaryHeap;
use std::time::UNIX_EPOCH;

pub(in crate::ui::library) struct RankedLibraryAsset {
    asset: LibraryAsset,
    lowercase_name: String,
}

impl RankedLibraryAsset {
    fn new(asset: LibraryAsset) -> Self {
        let lowercase_name = asset.display_name.to_lowercase();
        Self {
            asset,
            lowercase_name,
        }
    }
}

impl PartialEq for RankedLibraryAsset {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}

impl Eq for RankedLibraryAsset {}

impl PartialOrd for RankedLibraryAsset {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for RankedLibraryAsset {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // This is also the final display order: newest first, then stable
        // lexical tie-breakers. BinaryHeap therefore keeps the worst retained
        // item at its root, where a better candidate can replace it.
        other
            .asset
            .metadata
            .modified_seconds
            .cmp(&self.asset.metadata.modified_seconds)
            .then_with(|| self.lowercase_name.cmp(&other.lowercase_name))
            .then_with(|| self.asset.display_path.cmp(&other.asset.display_path))
            .then_with(|| self.asset.id.cmp(&other.asset.id))
    }
}

pub(in crate::ui::library) type FolderScan = (Vec<LibraryAsset>, usize, bool);

pub(in crate::ui::library) fn scan_folder_tree(
    root: &Path,
    is_cancelled: impl Fn() -> bool,
) -> Option<LibraryFolderNode> {
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

    let mut assets = BinaryHeap::with_capacity(maximum_files);
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
        if !file_type.is_file() || !is_supported_raw_path(&entry.path()) {
            continue;
        }
        let path = entry.path();
        let file_metadata = entry.metadata().ok();
        let modified_seconds = file_metadata
            .as_ref()
            .and_then(|metadata| metadata.modified().ok())
            .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |duration| duration.as_secs());
        let asset = LibraryAsset::from_desktop_path(
            path,
            file_metadata.as_ref().map_or(0, std::fs::Metadata::len),
            modified_seconds,
            None,
        );
        let candidate = RankedLibraryAsset::new(asset);
        if assets.len() < maximum_files {
            assets.push(candidate);
        } else {
            truncated = true;
            if assets
                .peek()
                .is_some_and(|worst_retained| candidate < *worst_retained)
            {
                assets.pop();
                assets.push(candidate);
            }
        }
    }
    let mut assets = assets.into_vec();
    assets.sort();
    let mut assets = assets
        .into_iter()
        .map(|ranked| ranked.asset)
        .collect::<Vec<_>>();

    // Reserve stable gallery geometry before preview pixels arrive.
    for asset in &mut assets {
        if is_cancelled() {
            return Ok(None);
        }
        if let Some(path) = asset.desktop_path() {
            asset.metadata.dimensions_hint = load_raw_display_dimensions(path).ok();
        }
    }

    Ok(Some((assets, warning_count, truncated)))
}
