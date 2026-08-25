use super::*;

impl LibraryState {
    pub(crate) fn folder_sidebar_open(&self) -> bool {
        self.folder_sidebar_open
    }

    pub(crate) fn set_folder_sidebar_open(&mut self, open: bool) -> bool {
        if self.folder_sidebar_open == open {
            return false;
        }
        self.folder_sidebar_open = open;
        true
    }

    pub(super) fn loading_thumbnail_for_index(
        &self,
        index: usize,
    ) -> Option<(egui::TextureHandle, [u32; 2], Option<egui::Color32>)> {
        let entry = self.entries.get(index)?;
        let texture = entry.texture.clone()?;
        let size = entry.thumbnail_size.unwrap_or_else(|| {
            let [width, height] = texture.size();
            [width as u32, height as u32]
        });
        let adaptive_backdrop = entry
            .resident_thumbnail
            .as_ref()
            .map(|thumbnail| crate::ui::theme::adaptive_backdrop_from_rgba(&thumbnail.rgba));
        Some((texture, size, adaptive_backdrop))
    }
}
