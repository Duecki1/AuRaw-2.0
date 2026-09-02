use super::*;

impl CalibRawApp {
    pub(crate) fn set_ui_design(&mut self, design: crate::ui::theme::UiDesign) {
        if self.preferences.ui_design == design {
            return;
        }
        self.preferences.ui_design = design;
        crate::ui::theme::apply(&self.egui_ctx, design);
        #[cfg(target_os = "android")]
        if let Err(error) =
            crate::android::set_light_system_bars(&self.android.android_app, !design.is_dark())
        {
            log::warn!("{error}");
        }
        self.persist_performance_settings();
    }

    pub(crate) fn set_preview_backdrop(&mut self, backdrop: crate::ui::theme::PreviewBackdrop) {
        if self.preferences.preview_backdrop == backdrop {
            return;
        }
        self.preferences.preview_backdrop = backdrop;
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn preview_backdrop_color(&self) -> egui::Color32 {
        self.preferences
            .preview_backdrop
            .color(self.ui.adaptive_preview_backdrop)
    }

    pub(crate) fn set_raw_cache_limit(&mut self, limit: usize) {
        let limit = limit.min(maximum_raw_cache_limit());
        if self.develop.raw_cache_limit == limit {
            return;
        }
        self.develop.raw_cache_limit = limit;
        self.trim_raw_cache();
        self.persist_performance_settings();
    }

    pub(crate) fn thumbnail_worker_count(&self) -> usize {
        self.library.thumbnail_worker_count()
    }

    pub(crate) fn set_thumbnail_worker_count(&mut self, workers: usize) {
        let context = self.egui_ctx.clone();
        let previous = self.library.thumbnail_worker_count();
        self.library.set_thumbnail_worker_count(workers, &context);
        if self.library.thumbnail_worker_count() != previous {
            self.persist_performance_settings();
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_render_edited_thumbnails_during_indexing(&mut self, enabled: bool) {
        let context = self.egui_ctx.clone();
        if self
            .library
            .set_render_edited_thumbnails_during_indexing(enabled, &context)
        {
            self.persist_performance_settings();
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_ai_gpu_acceleration(&mut self, enabled: bool) {
        if self.ai.gpu_acceleration == enabled {
            return;
        }
        self.ai.gpu_acceleration = enabled;
        calibraw_ai::set_ai_acceleration_enabled(enabled);
        self.sync_ai_model_runtime_context();
        self.persist_performance_settings();
        self.ui.notice = Some(if enabled {
            "AI GPU acceleration enabled. New AI model sessions will use it when available."
                .to_owned()
        } else {
            "AI GPU acceleration disabled. New AI model sessions will run on CPU.".to_owned()
        });
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_discord_rich_presence(&mut self, enabled: bool) {
        if self.preferences.discord_rich_presence == enabled {
            return;
        }
        if let Err(error) = self.discord_presence.set_enabled(enabled) {
            self.ui.notice = Some(error);
            return;
        }

        self.preferences.discord_rich_presence = enabled;
        self.sync_discord_presence();
        self.persist_performance_settings();
        self.ui.notice = Some(if enabled {
            "Discord Rich Presence enabled. Discord will show Library browsing or the current edit timer when its desktop client is running."
                .to_owned()
        } else {
            "Discord Rich Presence disabled and cleared.".to_owned()
        });
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn discord_rich_presence_configured(&self) -> bool {
        self.discord_presence.is_configured()
    }

    #[cfg(not(target_os = "android"))]
    pub(in crate::app) fn sync_discord_presence(&mut self) {
        let document_id = self
            .develop
            .loaded_raw
            .as_ref()
            .map(|_| self.persistence.sidecar_generation);
        self.discord_presence.sync(self.ui.active_tab, document_id);
    }

    pub(crate) fn thumbnail_cache_size_label(&mut self) -> String {
        let update = self
            .ui
            .thumbnail_cache_size_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match update {
            Some(Ok(result)) => {
                if let Err(error) = &result {
                    log::warn!("could not measure thumbnail cache: {error}");
                }
                self.ui.thumbnail_cache_size = Some(result);
                self.ui.thumbnail_cache_size_receiver = None;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.ui.thumbnail_cache_size = Some(Err(
                    "thumbnail cache size worker stopped unexpectedly".to_owned(),
                ));
                self.ui.thumbnail_cache_size_receiver = None;
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }

        if self.ui.thumbnail_cache_size.is_none() && self.ui.thumbnail_cache_size_receiver.is_none()
        {
            let (sender, receiver) = mpsc::channel();
            let repaint = self.egui_ctx.clone();
            #[cfg(target_os = "android")]
            let android_app = self.android.android_app.clone();
            let spawn = std::thread::Builder::new()
                .name("calibraw-thumbnail-cache-size".to_owned())
                .spawn(move || {
                    #[cfg(not(target_os = "android"))]
                    let result = crate::thumbnail_cache::desktop_thumbnail_cache_size_bytes();
                    #[cfg(target_os = "android")]
                    let result = crate::android::thumbnail_cache_size_bytes(&android_app);
                    let _ = sender.send(result);
                    repaint.request_repaint();
                });
            match spawn {
                Ok(_) => self.ui.thumbnail_cache_size_receiver = Some(receiver),
                Err(error) => {
                    self.ui.thumbnail_cache_size =
                        Some(Err(format!("could not start cache size worker: {error}")));
                }
            }
        }

        match self.ui.thumbnail_cache_size.as_ref() {
            Some(Ok(0)) => "0 MB used".to_owned(),
            Some(Ok(bytes)) if *bytes < 100_000 => "<0.1 MB used".to_owned(),
            Some(Ok(bytes)) => format!("{:.1} MB used", *bytes as f64 / 1_000_000.0),
            Some(Err(_)) => "Size unavailable".to_owned(),
            None => "Calculating size…".to_owned(),
        }
    }

    pub(crate) fn clear_thumbnail_cache(&mut self) {
        self.ui.thumbnail_cache_size = None;
        self.ui.thumbnail_cache_size_receiver = None;
        self.library.prepare_for_thumbnail_cache_clear();
        let decode_gate = self.library.decode_gate();
        let result = match decode_gate.write() {
            Ok(_decode_guard) => {
                #[cfg(not(target_os = "android"))]
                let cleared = crate::thumbnail_cache::clear_desktop_thumbnail_cache();
                #[cfg(target_os = "android")]
                let cleared = crate::android::clear_thumbnail_cache(&self.android.android_app);
                cleared
            }
            Err(_) => Err("thumbnail decode gate was poisoned".to_owned()),
        };

        match result {
            Ok(()) => {
                self.ui.thumbnail_cache_size = Some(Ok(0));
                self.ui.notice = Some("Thumbnail cache cleared. Rebuilding previews…".to_owned());
                self.library.refresh(&self.egui_ctx);
            }
            Err(error) => {
                self.ui.notice = Some(format!("Could not clear thumbnail cache: {error}"));
                self.library
                    .set_status("Could not clear the thumbnail cache.");
            }
        }
    }

    pub(crate) fn set_adjustment_copy_settings(&mut self, settings: AdjustmentCopySettings) {
        if self.preferences.adjustment_copy_settings == settings {
            return;
        }
        self.preferences.adjustment_copy_settings = settings;
        self.persist_performance_settings();
    }

    pub(crate) fn set_library_thumbnail_size(
        &mut self,
        thumbnail_size: crate::ui::library::LibraryThumbnailSize,
    ) {
        if self.library.set_thumbnail_size(thumbnail_size) {
            self.persist_performance_settings();
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn set_library_sort_order(
        &mut self,
        sort_order: crate::ui::library::LibrarySortOrder,
    ) {
        if self.library.set_sort_order(sort_order) {
            self.persist_performance_settings();
            self.egui_ctx.request_repaint();
        }
    }

    pub(crate) fn set_library_folder_sidebar_open(&mut self, open: bool) {
        if self.library.set_folder_sidebar_open(open) {
            #[cfg(not(target_os = "android"))]
            self.persist_performance_settings();
            #[cfg(target_os = "android")]
            crate::android::set_back_navigation_active(
                open || self.library.has_selection() || self.ui.active_tab != AppTab::Library,
            );
            self.egui_ctx.request_repaint();
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn select_android_library_folder(&mut self, folder: String) {
        if self.library.select_android_folder(folder, &self.egui_ctx) {
            self.persist_performance_settings();
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_develop_filmstrip_open(&mut self, open: bool) {
        if self.develop_ui.filmstrip_open == open {
            return;
        }
        self.develop_ui.filmstrip_open = open;
        if open {
            self.develop_ui.filmstrip_centered_path = None;
        }
        self.persist_performance_settings();
        self.egui_ctx.request_repaint();
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn select_library_folder(&mut self, folder: PathBuf) {
        if self.library.select_folder(folder, &self.egui_ctx) {
            self.persist_performance_settings();
        }
    }

    pub(crate) fn persist_performance_settings(&self) -> bool {
        let settings = crate::performance_settings::PerformanceSettings {
            raw_cache_files: self.develop.raw_cache_limit,
            thumbnail_workers: self.library.thumbnail_worker_count(),
            render_edited_thumbnails_during_indexing: self
                .library
                .renders_edited_thumbnails_during_indexing(),
            library_thumbnail_size: self.library.thumbnail_size(),
            library_sort_order: self.library.sort_order(),
            preview_quality: self.preview.quality,
            image_relative_brush_size: self.preferences.image_relative_brush_size,
            show_develop_navigation_labels: self.preferences.show_develop_navigation_labels,
            ui_design: self.preferences.ui_design,
            preview_backdrop: self.preferences.preview_backdrop,
            onboarding_completed: self.preferences.onboarding_completed,
            auto_check_updates: self.preferences.auto_check_updates,
            ignored_update_version: self.preferences.ignored_update_version.clone(),
            birefnet_quality: self.ai.birefnet_quality,
            #[cfg(not(target_os = "android"))]
            subject_crop_refinement: self.ai.subject_crop_refinement,
            #[cfg(not(target_os = "android"))]
            ai_gpu_acceleration: self.ai.gpu_acceleration,
            #[cfg(not(target_os = "android"))]
            onnx_runtime_mode: self.ai.runtime_mode,
            #[cfg(not(target_os = "android"))]
            discord_rich_presence: self.preferences.discord_rich_presence,
            camera_profile_mode: self.preferences.camera_profile_mode,
            camera_profile_folder: self.preferences.camera_profile_folder.clone(),
            camera_profile_folder_label: self.preferences.camera_profile_folder_label.clone(),
            camera_profile_auto_detect: self.preferences.camera_profile_auto_detect,
            last_camera_profile: self.preferences.last_camera_profile.clone(),
            adjustment_copy_settings: self.preferences.adjustment_copy_settings,
            #[cfg(target_os = "android")]
            last_android_library_folder: self.library.android_folder().to_owned(),
            #[cfg(not(target_os = "android"))]
            last_library_folder: self
                .library
                .root_folder()
                .map(|folder| folder.to_path_buf()),
            #[cfg(not(target_os = "android"))]
            last_library_selected_folder: self.library.folder().map(|folder| folder.to_path_buf()),
            #[cfg(not(target_os = "android"))]
            library_folder_sidebar_open: self.library.folder_sidebar_open(),
            #[cfg(not(target_os = "android"))]
            develop_filmstrip_open: self.develop_ui.filmstrip_open,
            ..Default::default()
        };
        let Some(settings_path) = self.preferences.performance_settings_path.as_deref() else {
            return false;
        };
        match crate::performance_settings::save(Some(settings_path), settings) {
            Ok(()) => true,
            Err(error) => {
                log::warn!("{error}");
                false
            }
        }
    }
}
