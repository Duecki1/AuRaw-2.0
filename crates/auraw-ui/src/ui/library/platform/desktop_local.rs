use super::super::*;

impl LibraryState {
    pub(crate) fn folder(&self) -> Option<&Path> {
        self.folder.as_deref()
    }

    pub(crate) fn root_folder(&self) -> Option<&Path> {
        self.root_folder.as_deref()
    }

    pub(crate) fn filmstrip_len(&self) -> usize {
        self.entries.len()
    }

    pub(crate) fn filmstrip_item(&self, index: usize) -> Option<DesktopFilmstripItem> {
        let entry = self.entries.get(index)?;
        let path = entry.asset.desktop_path()?.to_owned();
        Some(DesktopFilmstripItem {
            asset: entry.asset.clone(),
            path,
            texture: entry.texture.clone(),
            thumbnail_size: entry.thumbnail_size,
        })
    }

    pub(crate) fn filmstrip_index_for_path(&self, path: &Path) -> Option<usize> {
        self.entry_indices
            .get(&LibraryAssetId::Desktop(path.to_owned()))
            .copied()
    }

    pub(crate) fn desktop_loading_thumbnail_for_path(
        &mut self,
        path: &Path,
        context: &egui::Context,
    ) -> Option<(egui::TextureHandle, [u32; 2])> {
        let index = self.filmstrip_index_for_path(path)?;
        self.restore_resident_thumbnail_texture(index, context);
        self.loading_thumbnail_for_index(index)
    }

    pub(in crate::ui::library) fn file_action_in_progress(&self) -> bool {
        self.local_mutation_in_progress()
    }

    pub(in crate::ui::library) fn start_folder_operation(
        &mut self,
        operation: LibraryFolderOperation,
        context: &egui::Context,
    ) {
        if self.file_action_in_progress() {
            self.status = "Another library file action is still running.".to_owned();
            return;
        }

        let status = folder_operation_progress_status(&operation);
        let operation_root = match &operation {
            LibraryFolderOperation::Create { root, .. }
            | LibraryFolderOperation::Copy { root, .. }
            | LibraryFolderOperation::Move { root, .. }
            | LibraryFolderOperation::Delete { root, .. } => root.clone(),
        };
        let (sender, receiver) = mpsc::channel();
        self.folder_operation_receiver = Some(receiver);
        self.status = status;
        let repaint = context.clone();
        let spawn = std::thread::Builder::new()
            .name("auraw-library-folder-operation".to_owned())
            .spawn(move || {
                let result = run_folder_operation(operation);
                let _ = sender.send(LibraryFolderOperationCompletion {
                    root: operation_root,
                    result,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.folder_operation_receiver = None;
            self.status = format!("Could not start folder operation: {error}");
        }
    }

    pub(in crate::ui::library) fn apply_folder_operation_result(
        &mut self,
        result: LibraryFolderOperationResult,
        context: &egui::Context,
    ) {
        match result {
            LibraryFolderOperationResult::Created(path) => {
                if let Some(parent) = path.parent() {
                    self.expanded_folders.insert(parent.to_path_buf());
                }
                self.status = format!("Created folder {}", path.display());
            }
            LibraryFolderOperationResult::Copied {
                source,
                destination,
            } => {
                if let Some(parent) = destination.parent() {
                    self.expanded_folders.insert(parent.to_path_buf());
                }
                self.status = format!("Copied {} to {}", source.display(), destination.display());
            }
            LibraryFolderOperationResult::Moved {
                source,
                destination,
            } => {
                if let Some(folder) = self.folder.as_mut() {
                    if let Ok(suffix) = folder.strip_prefix(&source) {
                        *folder = destination.join(suffix);
                        self.location = Some(folder.display().to_string());
                    }
                }
                let remapped_expanded = self
                    .expanded_folders
                    .drain()
                    .map(|folder| {
                        folder
                            .strip_prefix(&source)
                            .map(|suffix| destination.join(suffix))
                            .unwrap_or(folder)
                    })
                    .collect();
                self.expanded_folders = remapped_expanded;
                if let Some(parent) = destination.parent() {
                    self.expanded_folders.insert(parent.to_path_buf());
                }
                if self.folder_clipboard.as_ref().is_some_and(|clipboard| {
                    clipboard.mode == LibraryFolderClipboardMode::Cut && clipboard.path == source
                }) {
                    self.folder_clipboard = None;
                } else if let Some(clipboard) = self.folder_clipboard.as_mut() {
                    if let Ok(suffix) = clipboard.path.strip_prefix(&source) {
                        clipboard.path = destination.join(suffix);
                    }
                }
                self.status = format!("Moved {} to {}", source.display(), destination.display());
            }
            LibraryFolderOperationResult::Deleted(path) => {
                if let Some(folder) = self.folder.as_ref() {
                    if folder.starts_with(&path) {
                        let root = self.root_folder.as_deref();
                        let fallback = path
                            .parent()
                            .filter(|parent| root.is_some_and(|root| parent.starts_with(root)))
                            .map(Path::to_path_buf)
                            .or_else(|| self.root_folder.clone());
                        self.folder = fallback;
                        self.location = self
                            .folder
                            .as_ref()
                            .map(|folder| folder.display().to_string());
                        self.entries.clear();
                        self.entry_indices.clear();
                        self.clear_selection();
                        self.catalog_ready = false;
                    }
                }
                self.expanded_folders
                    .retain(|folder| !folder.starts_with(&path));
                if self
                    .folder_clipboard
                    .as_ref()
                    .is_some_and(|clipboard| clipboard.path.starts_with(&path))
                {
                    self.folder_clipboard = None;
                }
                self.status = format!("Deleted folder {}", path.display());
            }
        }
        self.refresh(context);
    }

    pub(crate) fn import_dropped_raws(
        &mut self,
        source_paths: Vec<PathBuf>,
        context: &egui::Context,
    ) {
        if self.file_action_in_progress() {
            self.status = "Another library file action is still running.".to_owned();
            return;
        }
        let Some(folder) = self.folder.clone() else {
            self.status = "Open a library folder before dropping RAW files.".to_owned();
            return;
        };
        let Some(root_folder) = self.root_folder.clone() else {
            self.status = "Open a top-level library folder before dropping items.".to_owned();
            return;
        };
        if source_paths.is_empty() {
            return;
        }

        let item_count = source_paths.len();
        let (sender, receiver) = mpsc::channel();
        self.raw_import_receiver = Some(receiver);
        self.status = format!(
            "Importing {item_count} dropped {} into {}…",
            if item_count == 1 { "item" } else { "items" },
            folder.display()
        );
        let repaint = context.clone();
        let spawn = std::thread::Builder::new()
            .name("auraw-library-drop-import".to_owned())
            .spawn(move || {
                let mut imported = Vec::new();
                let mut imported_folders = Vec::new();
                let mut already_present = 0usize;
                let mut ignored = 0usize;
                let mut failures = Vec::new();
                let mut seen_sources = HashSet::new();

                if let Err(error) = canonical_library_directory(&root_folder, &folder, true) {
                    let _ = sender.send(RawImportResult {
                        imported,
                        imported_folders,
                        already_present,
                        ignored,
                        failures: vec![error],
                    });
                    repaint.request_repaint();
                    return;
                }

                for source in source_paths {
                    let source_key = fs::canonicalize(&source).unwrap_or_else(|_| source.clone());
                    if !seen_sources.insert(source_key) {
                        ignored += 1;
                        continue;
                    }
                    if source.is_dir() {
                        match import_folder_into_library(&source, &folder) {
                            Ok(destination) => imported_folders.push(destination),
                            Err(error) => failures.push(error),
                        }
                        continue;
                    }
                    if !source.is_file() || !is_supported_raw_path(&source) {
                        ignored += 1;
                        continue;
                    }

                    match import_raw_into_folder(&source, &folder) {
                        Ok(RawImportOutcome::Imported(destination)) => imported.push(destination),
                        Ok(RawImportOutcome::AlreadyPresent) => already_present += 1,
                        Err(error) => failures.push(error),
                    }
                }

                let _ = sender.send(RawImportResult {
                    imported,
                    imported_folders,
                    already_present,
                    ignored,
                    failures,
                });
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.raw_import_receiver = None;
            self.status = format!("Could not start RAW import: {error}");
        }
    }

    pub(crate) fn open_folder(&mut self, folder: PathBuf, context: &egui::Context) {
        self.folder_sidebar_open = true;
        self.open_folder_at(folder.clone(), folder, context);
    }

    pub(crate) fn restore_folder(
        &mut self,
        root: PathBuf,
        selected: Option<PathBuf>,
        context: &egui::Context,
    ) {
        let selected = selected
            .filter(|folder| folder.is_dir() && folder.starts_with(&root))
            .unwrap_or_else(|| root.clone());
        self.open_folder_at(root, selected, context);
    }

    pub(in crate::ui::library) fn open_folder_at(&mut self, root: PathBuf, folder: PathBuf, context: &egui::Context) {
        let folder_changed = self.folder.as_ref() != Some(&folder);
        let root_changed = self.root_folder.as_ref() != Some(&root);
        if root_changed {
            self.folder_clipboard = None;
            self.folder_name_dialog = None;
            self.folder_delete_confirmation = None;
        }
        self.root_folder = Some(root.clone());
        self.location = Some(folder.display().to_string());
        self.folder = Some(folder.clone());
        self.folder_tree = Some(LibraryFolderNode::empty(root.clone()));
        self.expanded_folders.clear();
        let mut ancestor = Some(folder.as_path());
        while let Some(path) = ancestor.filter(|path| path.starts_with(&root)) {
            self.expanded_folders.insert(path.to_path_buf());
            if path == root {
                break;
            }
            ancestor = path.parent();
        }
        if folder_changed {
            self.entries.clear();
            self.entry_indices.clear();
            self.clear_selection();
            self.catalog_ready = false;
        }
        self.refresh(context);
    }

    pub(crate) fn select_folder(&mut self, folder: PathBuf, context: &egui::Context) -> bool {
        let Some(root) = self.root_folder.as_ref() else {
            return false;
        };
        if !folder.starts_with(root)
            || self.folder.as_ref() == Some(&folder)
        {
            return false;
        }

        self.location = Some(folder.display().to_string());
        self.folder = Some(folder);
        self.entries.clear();
        self.entry_indices.clear();
        self.clear_selection();
        self.catalog_ready = false;
        self.refresh(context);
        true
    }

}
