use super::super::*;

impl LibraryState {
    pub(crate) fn android_folder(&self) -> &str {
        &self.android_folder
    }

    pub(crate) fn select_android_folder(
        &mut self,
        folder: String,
        context: &egui::Context,
    ) -> bool {
        if self.view == LibraryView::Local && self.android_folder == folder {
            return false;
        }
        if let Err(error) = crate::android::select_library_folder(&self.android_app, &folder) {
            self.status = error;
            return false;
        }
        self.view = LibraryView::Local;
        self.android_folder = folder;
        self.android_expanded_folders
            .extend(android_folder_ancestors(&self.android_folder));
        let location =
            android_library_location_label(&self.android_root_location, &self.android_folder);
        self.location = Some(location.clone());
        self.local_location = Some(location);
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        true
    }

    pub(crate) fn android_loading_thumbnail_for_uri(
        &mut self,
        uri: &str,
        context: &egui::Context,
    ) -> Option<(egui::TextureHandle, [u32; 2])> {
        let index = self.entries.iter().position(|entry| {
            matches!(
                &entry.info.source,
                LibrarySource::Android { uri: entry_uri, .. } if entry_uri == uri
            )
        })?;
        self.restore_resident_thumbnail_texture(index, context);
        self.loading_thumbnail_for_index(index)
    }
}
