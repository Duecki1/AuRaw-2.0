#[cfg(any(target_os = "android", test))]
use std::collections::HashSet;

#[cfg(not(target_os = "android"))]
mod desktop_catalog;
#[cfg(not(target_os = "android"))]
mod desktop_files;

#[cfg(target_os = "android")]
mod android_local;
#[cfg(not(target_os = "android"))]
mod desktop_local;

#[cfg(target_os = "android")]
mod android;
#[cfg(not(target_os = "android"))]
mod desktop;

#[cfg(not(target_os = "android"))]
pub(super) use desktop_catalog::*;
#[cfg(not(target_os = "android"))]
pub(super) use desktop_files::*;

#[cfg(target_os = "android")]
use android as selected;
#[cfg(not(target_os = "android"))]
use desktop as selected;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum LocalFolderToolbarAction {
    Refresh,
    New,
}

pub(super) use selected::{
    apply_local_toolbar_action, can_create_local_folder, default_thumbnail_worker_count,
    local_action_in_progress, local_folders_available, maximum_thumbnail_worker_count,
    show_local_folder_tree, show_page_dialogs, show_sidebar_dialogs,
    start_local_library_ai_mask_refresh, start_local_library_export,
};

#[cfg(any(target_os = "android", test))]
pub(super) fn android_library_location_label(root: &str, folder: &str) -> String {
    if folder.is_empty() {
        root.to_owned()
    } else {
        format!("{root}/{folder}")
    }
}

#[cfg(any(target_os = "android", test))]
pub(super) fn android_folder_parent(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(parent, _)| parent)
}

#[cfg(any(target_os = "android", test))]
pub(super) fn android_folder_ancestors(path: &str) -> HashSet<String> {
    let mut expanded = HashSet::from([String::new()]);
    let mut current = path.to_owned();
    while !current.is_empty() {
        let parent = android_folder_parent(&current).to_owned();
        expanded.insert(parent.clone());
        current = parent;
    }
    expanded
}
