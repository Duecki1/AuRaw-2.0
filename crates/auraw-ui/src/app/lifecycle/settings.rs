use super::*;

impl AurawApp {
    pub(crate) fn raw_cache_limit(&self) -> usize {
        self.raw_cache_limit
    }

    pub(crate) fn set_raw_cache_limit(&mut self, limit: usize) {
        let limit = limit.min(maximum_raw_cache_limit());
        if self.raw_cache_limit == limit {
            return;
        }
        self.raw_cache_limit = limit;
        self.trim_raw_cache();
        self.persist_performance_settings();
    }

    pub(crate) fn set_cloud_settings(
        &mut self,
        enabled: bool,
        server_url: String,
        access_token: String,
    ) {
        let config = crate::cloud::CloudConfig {
            enabled,
            server_url,
            access_token,
        };
        let cache_root = crate::cloud::cache_root(self.performance_settings_path.as_deref());
        self.library
            .configure_cloud(config, cache_root, &self.egui_ctx);
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
    pub(crate) fn set_ai_gpu_acceleration(&mut self, enabled: bool) {
        if self.ai_gpu_acceleration == enabled {
            return;
        }
        self.ai_gpu_acceleration = enabled;
        auraw_ai::set_ai_acceleration_enabled(enabled);
        // Settings is outside Develop, so these calls unload any idle cached
        // sessions without waiting for a native inference that is winding down.
        self.sync_ai_model_cache_policy();
        self.persist_performance_settings();
        self.notice = Some(if enabled {
            "AI GPU acceleration enabled. New AI model sessions will use it when available."
                .to_owned()
        } else {
            "AI GPU acceleration disabled. New AI model sessions will run on CPU.".to_owned()
        });
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn thumbnail_cache_size_label(&mut self) -> String {
        let update = self
            .thumbnail_cache_size_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match update {
            Some(Ok(result)) => {
                if let Err(error) = &result {
                    log::warn!("could not measure thumbnail cache: {error}");
                }
                self.thumbnail_cache_size = Some(result);
                self.thumbnail_cache_size_receiver = None;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.thumbnail_cache_size = Some(Err(
                    "thumbnail cache size worker stopped unexpectedly".to_owned(),
                ));
                self.thumbnail_cache_size_receiver = None;
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }

        if self.thumbnail_cache_size.is_none() && self.thumbnail_cache_size_receiver.is_none() {
            let (sender, receiver) = mpsc::channel();
            let repaint = self.egui_ctx.clone();
            #[cfg(target_os = "android")]
            let android_app = self.android_app.clone();
            let spawn = std::thread::Builder::new()
                .name("auraw-thumbnail-cache-size".to_owned())
                .spawn(move || {
                    #[cfg(not(target_os = "android"))]
                    let result = crate::thumbnail_cache::desktop_thumbnail_cache_size_bytes();
                    #[cfg(target_os = "android")]
                    let result = crate::android::thumbnail_cache_size_bytes(&android_app);
                    let _ = sender.send(result);
                    repaint.request_repaint();
                });
            match spawn {
                Ok(_) => self.thumbnail_cache_size_receiver = Some(receiver),
                Err(error) => {
                    self.thumbnail_cache_size =
                        Some(Err(format!("could not start cache size worker: {error}")));
                }
            }
        }

        match self.thumbnail_cache_size.as_ref() {
            Some(Ok(0)) => "0 MB used".to_owned(),
            Some(Ok(bytes)) if *bytes < 100_000 => "<0.1 MB used".to_owned(),
            Some(Ok(bytes)) => format!("{:.1} MB used", *bytes as f64 / 1_000_000.0),
            Some(Err(_)) => "Size unavailable".to_owned(),
            None => "Calculating size…".to_owned(),
        }
    }

    pub(crate) fn clear_thumbnail_cache(&mut self) {
        self.thumbnail_cache_size = None;
        self.thumbnail_cache_size_receiver = None;
        self.library.prepare_for_thumbnail_cache_clear();
        let decode_gate = self.library.decode_gate();
        let result = match decode_gate.write() {
            Ok(_decode_guard) => {
                #[cfg(not(target_os = "android"))]
                let cleared = crate::thumbnail_cache::clear_desktop_thumbnail_cache();
                #[cfg(target_os = "android")]
                let cleared = crate::android::clear_thumbnail_cache(&self.android_app);
                cleared
            }
            Err(_) => Err("thumbnail decode gate was poisoned".to_owned()),
        };

        match result {
            Ok(()) => {
                self.thumbnail_cache_size = Some(Ok(0));
                self.notice = Some("Thumbnail cache cleared. Rebuilding previews…".to_owned());
                self.library.refresh(&self.egui_ctx);
            }
            Err(error) => {
                self.notice = Some(format!("Could not clear thumbnail cache: {error}"));
                self.library
                    .set_status("Could not clear the thumbnail cache.");
            }
        }
    }

    pub(crate) fn set_adjustment_copy_settings(&mut self, settings: AdjustmentCopySettings) {
        if self.adjustment_copy_settings == settings {
            return;
        }
        self.adjustment_copy_settings = settings;
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

    pub(crate) fn show_library_view(&mut self, view: crate::ui::library::LibraryView) {
        let changed = match view {
            crate::ui::library::LibraryView::Local => self.library.show_local(&self.egui_ctx),
            crate::ui::library::LibraryView::Cloud => self.library.show_cloud(&self.egui_ctx),
        };
        if changed {
            self.persist_performance_settings();
        }
    }

    pub(crate) fn select_cloud_library_folder(&mut self, folder_id: String) {
        if self
            .library
            .select_cloud_folder(folder_id, &self.egui_ctx)
        {
            self.persist_performance_settings();
        }
    }

    pub(crate) fn show_cloud_library_trash(&mut self) {
        if self.library.show_cloud_trash(&self.egui_ctx) {
            self.persist_performance_settings();
        }
    }

    pub(crate) fn remember_cloud_library_folder(&mut self, folder_id: String) {
        if self
            .library
            .remember_cloud_folder_without_refresh(folder_id)
        {
            self.persist_performance_settings();
        }
    }

    pub(crate) fn set_library_folder_sidebar_open(&mut self, open: bool) {
        if self.library.set_folder_sidebar_open(open) {
            #[cfg(not(target_os = "android"))]
            self.persist_performance_settings();
            #[cfg(target_os = "android")]
            crate::android::set_back_navigation_active(
                open || self.library.has_selection() || self.active_tab != AppTab::Library,
            );
            self.egui_ctx.request_repaint();
        }
    }

    #[cfg(target_os = "android")]
    pub(crate) fn select_android_library_folder(&mut self, folder: String) {
        if self
            .library
            .select_android_folder(folder, &self.egui_ctx)
        {
            self.persist_performance_settings();
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn set_develop_filmstrip_open(&mut self, open: bool) {
        if self.develop_filmstrip_open == open {
            return;
        }
        self.develop_filmstrip_open = open;
        if open {
            self.develop_filmstrip_centered_path = None;
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
            raw_cache_files: self.raw_cache_limit,
            thumbnail_workers: self.library.thumbnail_worker_count(),
            library_thumbnail_size: self.library.thumbnail_size(),
            library_sort_order: self.library.sort_order(),
            preview_quality: self.preview_quality,
            image_relative_brush_size: self.image_relative_brush_size,
            birefnet_quality: self.birefnet_quality,
            #[cfg(not(target_os = "android"))]
            ai_gpu_acceleration: self.ai_gpu_acceleration,
            camera_profile_mode: self.camera_profile_mode,
            camera_profile_folder: self.camera_profile_folder.clone(),
            camera_profile_folder_label: self.camera_profile_folder_label.clone(),
            camera_profile_auto_detect: self.camera_profile_auto_detect,
            last_camera_profile: self.last_camera_profile.clone(),
            #[cfg(not(target_os = "android"))]
            display_color_management: self.display_color_management,
            #[cfg(not(target_os = "android"))]
            display_profile_override: self.display_profile_override.clone(),
            adjustment_copy_settings: self.adjustment_copy_settings,
            cloud_enabled: self.library.cloud_config().enabled,
            cloud_server_url: self.library.cloud_config().server_url.clone(),
            cloud_access_token: self.library.cloud_config().access_token.clone(),
            last_library_view: self.library.view(),
            last_cloud_library_folder: self.library.cloud_folder_id().to_owned(),
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
            develop_filmstrip_open: self.develop_filmstrip_open,
            ..Default::default()
        };
        let Some(settings_path) = self.performance_settings_path.as_deref() else {
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
