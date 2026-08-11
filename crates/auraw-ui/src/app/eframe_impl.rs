impl eframe::App for AurawApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if auraw_gpu::take_gpu_out_of_memory() {
            // Optional zoom/navigation pipelines and UI-only mask textures are
            // safe to discard between frames. Keep the main preview alive so
            // the user can save work or lower preview quality before retrying.
            let retired = [
                self.preview_detail
                    .take()
                    .and_then(|preview| preview.pipeline.egui_texture_id),
                self.preview_navigation
                    .take()
                    .and_then(|preview| preview.pipeline.egui_texture_id),
            ];
            for texture_id in retired.into_iter().flatten() {
                self.retire_egui_texture(texture_id);
            }
            self.preview_detail_pending_stage = None;
            self.navigation_pending_stage = None;
            self.preview_detail_urgent = false;
            self.preview_zoom = 1.0;
            self.preview_center = [0.5, 0.5];
            self.mask_overlay_texture = None;
            self.mask_overlay_texture_key = None;
            self.mask_thumbnail_group_textures.clear();
            self.mask_thumbnail_component_textures.clear();
            self.mask_thumbnail_component_mask = None;
            self.inpaint_texture = None;
            self.inpaint_texture_key = None;
            self.inpaint_stroke_texture = None;
            self.inpaint_stroke_texture_key = None;
            self.inpaint_focus_texture = None;
            self.inpaint_focus_texture_key = None;
            self.notice = Some(
                "GPU memory was exhausted. AuRaw cancelled the operation and released optional preview textures. Close other GPU-heavy apps or lower Preview Quality before retrying."
                    .to_owned(),
            );
        }
        // Flush IDs retired by the previous frame before this frame emits any
        // meshes. Freeing them later in `ui` would invalidate texture references
        // that egui has already recorded for the pending render pass.
        self.release_retired_egui_textures(frame);
        #[cfg(not(target_os = "android"))]
        let raw_drop_hovered = ui.ctx().input(|input| !input.raw.hovered_files.is_empty());
        #[cfg(not(target_os = "android"))]
        {
            let dropped_paths = ui.ctx().input(|input| {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.clone())
                    .collect::<Vec<_>>()
            });
            if !dropped_paths.is_empty() {
                self.library.import_dropped_raws(dropped_paths, ui.ctx());
            }
            self.library.poll_dropped_raw_import(ui.ctx());
        }

        #[cfg(not(target_os = "android"))]
        self.poll_desktop_picker(frame);
        #[cfg(target_os = "android")]
        {
            self.poll_android_picker(frame);
            self.poll_android_export_publish();
            if crate::android::take_back_request() {
                if self.android_foreground_task_active() {
                    // Long-running Android operations are modal. Ignore system
                    // Back until the foreground task completes or is cancelled.
                    ui.ctx().request_repaint();
                } else if self.active_tab == AppTab::Library
                    && self.library.folder_sidebar_open()
                {
                    self.set_library_folder_sidebar_open(false);
                } else if self.active_tab == AppTab::Library && self.library.has_selection() {
                    self.library.clear_selection();
                    crate::android::set_back_navigation_active(false);
                } else {
                    self.activate_tab(AppTab::Library);
                }
            }

            let [left, top, right, bottom] =
                crate::android::system_bar_insets_points(ui.ctx().pixels_per_point());
            if top > 0.0 {
                egui::Panel::top("android_status_bar_safe_area")
                    .resizable(false)
                    .exact_size(top)
                    .show(ui, |_| {});
            }
            if bottom > 0.0 {
                egui::Panel::bottom("android_navigation_bar_safe_area")
                    .resizable(false)
                    .exact_size(bottom)
                    .show(ui, |_| {});
            }
            if left > 0.0 {
                egui::Panel::left("android_left_system_safe_area")
                    .resizable(false)
                    .exact_size(left)
                    .show(ui, |_| {});
            }
            if right > 0.0 {
                egui::Panel::right("android_right_system_safe_area")
                    .resizable(false)
                    .exact_size(right)
                    .show(ui, |_| {});
            }
        }

        self.drive_background_tasks(frame);
        self.poll_load_worker(frame);
        self.poll_preview_rebuild_worker(frame);
        #[cfg(not(target_os = "android"))]
        self.sync_display_color_management(ui.ctx(), frame);
        #[cfg(not(target_os = "android"))]
        self.poll_library_batch_export_worker();
        self.poll_export_worker(frame);
        #[cfg(target_os = "android")]
        self.resume_android_library_batch_export_if_possible(frame);
        self.poll_subject_worker();
        self.poll_object_worker();
        self.poll_landscape_worker();
        self.poll_library_ai_mask_refresh(frame);
        self.poll_inpaint_worker();
        self.poll_ai_denoise_worker();
        self.resume_pending_ai_denoise(frame);
        self.drive_background_tasks(frame);
        #[cfg(target_os = "android")]
        self.sync_android_task_notification();
        #[cfg(not(target_os = "android"))]
        {
            self.handle_edit_history_shortcuts(ui.ctx());
            self.handle_sidecar_shortcut(ui.ctx());
            if self.active_tab == AppTab::Develop {
                Develop::handle_image_navigation_shortcuts(ui.ctx(), self, frame);
            }
        }
        #[cfg(target_os = "android")]
        if !self.android_foreground_task_active() {
            self.handle_edit_history_shortcuts(ui.ctx());
            self.handle_sidecar_shortcut(ui.ctx());
        }

        #[cfg(target_os = "android")]
        if self.android_foreground_task_active() {
            show_android_foreground_task_blocker(ui.ctx());
        }

        let viewport_size = ui.max_rect().size();
        let layout = ScreenLayout::from_size(viewport_size);
        let sidebar_size = layout.sidebar_default_size(viewport_size);

        self.refresh_status();
        #[cfg(not(target_os = "android"))]
        egui::Panel::top("top_bar")
            .frame(crate::ui::theme::toolbar_frame(ui))
            .show(ui, |ui| TopBar::show(ui, self, frame));
        #[cfg(target_os = "android")]
        if self.active_tab == AppTab::Develop {
            egui::Panel::top("top_bar")
                .frame(crate::ui::theme::toolbar_frame(ui))
                .show(ui, |ui| TopBar::show(ui, self, frame));
        }

        if self.active_tab == AppTab::Develop {
            match layout {
                ScreenLayout::Horizontal => {
                    #[cfg(not(target_os = "android"))]
                    {
                        egui::Panel::right("develop_tool_rail")
                            .resizable(false)
                            .exact_size(Sidebar::DESKTOP_TOOL_RAIL_WIDTH)
                            .show(ui, |ui| Sidebar::show_desktop_tool_rail(ui, self));

                        if self.develop_sidebar_open {
                            let panel_max = (viewport_size.x * 0.48).clamp(
                                ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH,
                                ScreenLayout::MAX_HORIZONTAL_SIDEBAR_WIDTH,
                            );
                            // `Panel` persists its own drag size. Feeding its
                            // content response back into `default_size` creates
                            // a width feedback loop: a wide child becomes the
                            // next frame's default and the panel springs open
                            // again after the user shrinks it.
                            egui::Panel::right("develop_sidebar_right")
                                .resizable(true)
                                .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                                .max_size(panel_max)
                                .default_size(sidebar_size.min(panel_max))
                                .show(ui, |ui| Sidebar::show(ui, self, layout, frame));
                        }
                    }

                    #[cfg(target_os = "android")]
                    egui::Panel::right("develop_android_landscape_primary_tabs")
                        .resizable(false)
                        .exact_size(Sidebar::ANDROID_LANDSCAPE_TOOL_RAIL_WIDTH)
                        .show(ui, |ui| {
                            Sidebar::show_android_landscape_primary_tabs(ui, self)
                        });

                    #[cfg(target_os = "android")]
                    egui::Panel::right("develop_sidebar_right")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                        .default_size(sidebar_size)
                        .show(ui, |ui| Sidebar::show(ui, self, layout, frame));

                    // Panel call order places the mask strip to the left of the
                    // resizable properties panel, with the tool rail outermost.
                    // On desktop it follows the sidebar visibility toggle so the
                    // icon-rail button truly collapses the whole editing sidebar.
                    #[cfg(not(target_os = "android"))]
                    if self.develop_sidebar_open && self.sidebar_tab == SidebarTab::Masks {
                        egui::Panel::right("develop_horizontal_mask_strip")
                            .resizable(false)
                            .exact_size(Sidebar::HORIZONTAL_MASK_STRIP_WIDTH)
                            .show(ui, |ui| {
                                Sidebar::show_horizontal_mask_strip(ui, self, frame)
                            });
                    }
                    #[cfg(target_os = "android")]
                    if self.sidebar_tab == SidebarTab::Masks {
                        egui::Panel::right("develop_horizontal_mask_strip")
                            .resizable(false)
                            .exact_size(Sidebar::HORIZONTAL_MASK_STRIP_WIDTH)
                            .show(ui, |ui| {
                                Sidebar::show_horizontal_mask_strip(ui, self, frame)
                            });
                    }
                }
                ScreenLayout::Vertical => {
                    egui::Panel::bottom("develop_sidebar_bottom")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_VERTICAL_SIDEBAR_HEIGHT)
                        .default_size(sidebar_size)
                        .show(ui, |ui| Sidebar::show(ui, self, layout, frame));

                    // Panels are laid out in call order. Showing this fixed-height
                    // panel after the resizable bottom sidebar places it directly
                    // above that sidebar, leaving the full sidebar height available
                    // for sliders and mask properties.
                    if self.sidebar_tab == SidebarTab::Masks {
                        egui::Panel::bottom("develop_vertical_mask_strip")
                            .resizable(false)
                            .exact_size(Sidebar::VERTICAL_MASK_STRIP_HEIGHT)
                            .show(ui, |ui| Sidebar::show_vertical_mask_strip(ui, self, frame));
                    }
                }
            }
        }

        #[cfg(not(target_os = "android"))]
        if self.active_tab == AppTab::Develop && self.develop_filmstrip_open {
            // Side panels are installed first, so the filmstrip spans only the
            // remaining Develop preview width and ends at the sidebar/tool-rail
            // edge. When hidden it consumes no bottom-panel space; reopening is
            // handled by the persistent button in the desktop tool rail.
            egui::Panel::bottom("filmstrip")
                .resizable(false)
                .exact_size(crate::ui::develop::FILMSTRIP_HEIGHT)
                .show(ui, |ui| Develop::show_filmstrip(ui, self, frame));
        }

        if self.active_tab == AppTab::Library && self.library.folder_sidebar_open() {
            #[cfg(not(target_os = "android"))]
            egui::Panel::left("library_folder_sidebar")
                .resizable(true)
                .min_size(220.0)
                .max_size((viewport_size.x * 0.45).max(220.0))
                .default_size(260.0)
                .show(ui, |ui| Library::show_folder_sidebar(ui, self));
            #[cfg(target_os = "android")]
            egui::Panel::left("library_folder_sidebar")
                .resizable(false)
                .exact_size(
                    (viewport_size.x * 0.84)
                        .clamp(220.0, 380.0)
                        .min(viewport_size.x.max(1.0)),
                )
                .show(ui, |ui| Library::show_folder_sidebar(ui, self));
        }

        let _central = egui::CentralPanel::default().show(ui, |ui| match self.active_tab {
            AppTab::Library => Library::show(ui, self, frame),
            AppTab::Develop => {
                #[cfg(not(target_os = "android"))]
                Develop::show_preview(ui, self, frame);
                #[cfg(target_os = "android")]
                Preview::show(ui, self, frame);
            }
            AppTab::Settings => {
                let settings_scroll_source = if slider_scroll_locked(ui.ctx()) {
                    egui::scroll_area::ScrollSource::NONE
                } else {
                    egui::scroll_area::ScrollSource::default()
                };
                egui::ScrollArea::vertical()
                    .scroll_source(settings_scroll_source)
                    .auto_shrink([false, false])
                    .show(ui, |ui| Settings::show(ui, self, layout));
            }
        });
        // Some internal library workflows assign `active_tab` directly while
        // borrowing other app state. Reconcile the model policy once per frame
        // as well as in the ordinary tab handlers so every way of leaving an AI
        // tool promptly releases its cached session.
        self.sync_ai_model_cache_policy();

        #[cfg(not(target_os = "android"))]
        if self.active_tab == AppTab::Develop {
            crate::ui::library::show_desktop_image_action_overlays(ui, self, frame);
        }

        self.apply_pending_lens_correction(frame);
        self.apply_pending_preview_quality(frame);
        self.sync_original_preview(frame);
        if !self.original_preview_requested {
            self.advance_navigation_preview(frame);
            self.advance_preview_detail(frame);
            self.advance_processing(frame);
        }
        self.refresh_status();

        if self.preview_processing_pending() {
            ui.ctx().request_repaint();
        }
        if self.has_background_tasks()
            || self.export_receiver.is_some()
            || self.export_publish_pending
            || self.inpaint_receiver.is_some()
            || self.ai_denoise_receiver.is_some()
            || self.preview_rebuild_receiver.is_some()
        {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        #[cfg(not(target_os = "android"))]
        if self.desktop_picker_receiver.is_some() {
            // Native dialogs are asynchronous. Keep the app event loop visibly
            // alive while the operating-system picker is open.
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
        #[cfg(target_os = "android")]
        if self.active_tab != AppTab::Library || self.library.folder_sidebar_open() {
            // JNI back callbacks can arrive while NativeActivity's render loop is
            // idle. Keep a low-frequency wake-up while an in-app Back destination
            // exists so the request is consumed promptly on every device.
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
        #[cfg(target_os = "android")]
        if self.picker_pending {
            // Android's SAF result can be followed by an asynchronous copy (DCP
            // folders in particular may contain thousands of files). A repaint
            // requested from the Java/JNI worker is not guaranteed to wake every
            // vendor's NativeActivity event loop after the external picker closes,
            // so keep a tiny polling heartbeat until the terminal result arrives.
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
        #[cfg(not(target_os = "android"))]
        if raw_drop_hovered {
            show_raw_drop_overlay(ui, self.library.folder());
        }
        self.show_subject_dialogs(ui.ctx());
        self.show_inpainting_dialogs(ui.ctx());
        self.show_ai_denoise_dialogs(ui.ctx(), frame);
        self.poll_cloud_sidecar_conflict_resolution(frame);
        self.show_sidecar_save_error_dialog(ui.ctx());
        self.show_background_task_detail_windows(ui.ctx());
        let edit_interaction_active = sidecar_interaction_active(ui.ctx());
        self.observe_edit_history(ui.ctx());
        self.schedule_sidecar_autosave(ui.ctx(), edit_interaction_active);
        // Poll after edit observation so an autosave that waited behind an
        // interaction can be coalesced to its final committed value before
        // the next worker starts.
        self.poll_sidecar_save();
        self.poll_developed_thumbnail(frame);
        #[cfg(target_os = "android")]
        crate::android::set_back_navigation_active(
            self.active_tab != AppTab::Library
                || self.library.has_selection()
                || self.library.folder_sidebar_open(),
        );
    }

    fn on_exit(&mut self) {
        crate::ai_masks::set_model_cache_enabled(false);
        crate::inpainting::set_model_cache_enabled(false);
        #[cfg(target_os = "android")]
        if let Err(error) = crate::android::clear_background_task_notification(&self.android_app) {
            log::warn!("{error}");
        }
        self.persist_performance_settings();
        self.flush_sidecar_on_exit();
    }
}

#[cfg(target_os = "android")]
fn show_android_foreground_task_blocker(ctx: &egui::Context) {
    let content_rect = ctx.content_rect();
    egui::Area::new(egui::Id::new("android-foreground-task-input-blocker"))
        .order(egui::Order::Middle)
        .fixed_pos(content_rect.min)
        .movable(false)
        .interactable(true)
        .show(ctx, |ui| {
            let (rect, _response) =
                ui.allocate_exact_size(content_rect.size(), egui::Sense::click_and_drag());
            ui.painter()
                .rect_filled(rect, 0.0, egui::Color32::from_black_alpha(96));
            ui.painter().text(
                egui::pos2(rect.center().x, rect.bottom() - 24.0),
                egui::Align2::CENTER_BOTTOM,
                "Keep AuRaw open in the foreground until the operation finishes. Leaving or closing the app may stop it.",
                egui::FontId::proportional(13.0),
                egui::Color32::from_white_alpha(210),
            );
        });
}

#[cfg(not(target_os = "android"))]
fn show_raw_drop_overlay(ui: &egui::Ui, folder: Option<&std::path::Path>) {
    let rect = ui.max_rect().shrink(18.0);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("auraw-raw-drop-overlay"),
    ));
    painter.rect_filled(rect, 12.0, egui::Color32::from_black_alpha(210));
    painter.rect_stroke(
        rect,
        12.0,
        egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
        egui::StrokeKind::Inside,
    );
    let message = folder.map_or_else(
        || "Open a library folder before dropping RAW files".to_owned(),
        |folder| {
            format!(
                "Drop RAW files to import them into\n{}\n\nFolders are copied here too",
                folder.display()
            )
        },
    );
    painter.text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        message,
        egui::FontId::proportional(18.0),
        egui::Color32::WHITE,
    );
}
