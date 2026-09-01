use super::*;

/// Returns the width selected with the sidebar resize handle.
///
/// `egui::Panel` normally records the size of its contents. That is useful for
/// small utility panels, but not for the Develop sidebar: revealing controls
/// such as brush or subject-refinement settings must not widen a sidebar the
/// user has already sized. Keep a separate persisted value that is updated
/// only while the panel resize handle is being dragged.
fn develop_sidebar_user_width(
    ctx: &egui::Context,
    panel_id: egui::Id,
    default_width: f32,
    min_width: f32,
    max_width: f32,
) -> f32 {
    ctx.data_mut(|data| {
        data.get_persisted::<f32>(panel_id.with("user-width"))
            .or_else(|| {
                data.get_persisted::<egui::PanelState>(panel_id)
                    .map(|state| state.size().x)
            })
            .unwrap_or(default_width)
            .clamp(min_width, max_width)
    })
}

fn set_develop_sidebar_panel_width(ctx: &egui::Context, panel_id: egui::Id, width: f32) {
    ctx.data_mut(|data| {
        data.insert_persisted(panel_id.with("user-width"), width);

        // A Panel reads its own persisted state before it draws. Restore that
        // state to the user-selected width before the next frame so content
        // expansion can never become the new sidebar width.
        let mut outer_rect = data
            .get_persisted::<egui::PanelState>(panel_id)
            .map(|state| state.outer_rect)
            .unwrap_or_else(|| egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 1.0)));
        // The Develop sidebar is attached to the right edge, so retain that
        // fixed edge when changing its stored width.
        outer_rect.min.x = outer_rect.max.x - width;
        data.insert_persisted(panel_id, egui::PanelState { outer_rect });
    });
}

fn dragged_develop_sidebar_width(
    ctx: &egui::Context,
    panel_id: egui::Id,
    min_width: f32,
    max_width: f32,
) -> Option<f32> {
    ctx.read_response(panel_id.with("__resize"))
        .filter(|response| response.dragged() || response.drag_stopped())
        .and_then(|response| response.interact_pointer_pos())
        .and_then(|pointer| {
            egui::PanelState::load(ctx, panel_id)
                .map(|state| (state.outer_rect.right() - pointer.x).clamp(min_width, max_width))
        })
}

impl CalibRawApp {
    fn release_optional_gpu_memory(&mut self) {
        let retired = [
            self.preview
                .detail
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
            self.preview
                .navigation
                .take()
                .and_then(|preview| preview.pipeline.egui_texture_id),
        ];
        for texture_id in retired.into_iter().flatten() {
            self.retire_egui_texture(texture_id);
        }
        self.preview.detail_rebuild_receiver = None;
        self.preview.detail_pending_stage = None;
        self.preview.navigation_pending_stage = None;
        self.preview.detail_urgent = false;
        self.preview.zoom = 1.0;
        self.preview.center = [0.5, 0.5];
        self.masks.overlay_texture = None;
        self.masks.overlay_texture_key = None;
        self.masks.thumbnail_group_textures.clear();
        self.masks.thumbnail_component_textures.clear();
        self.masks.thumbnail_component_mask = None;
    }

    fn stop_ai_after_gpu_memory_failure(&mut self) {
        self.cancel_foreground_operation();
        if self.ai.mask_update_active {
            self.cancel_ai_mask_update();
        }
        self.masks.source_cache = None;
        self.ai.object_cache = None;
        #[cfg(not(target_os = "android"))]
        if self.ai.gpu_acceleration {
            self.set_ai_gpu_acceleration(false);
        }
        self.release_optional_gpu_memory();
    }
}

impl eframe::App for CalibRawApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if calibraw_ai::take_ai_gpu_memory_failure() {
            self.stop_ai_after_gpu_memory_failure();
            self.ui.notice = Some(
                "An AI model ran out of GPU memory. Its job was stopped, GPU AI was disabled, and optional preview textures were released. You can re-enable GPU AI in Settings after reducing Subject mask quality."
                    .to_owned(),
            );
        }
        if calibraw_gpu::take_gpu_out_of_memory() {
            self.stop_ai_after_gpu_memory_failure();
            self.ui.notice = Some(
                "GPU memory was exhausted. CalibRaw stopped AI work, disabled GPU AI, and released optional preview textures. Close other GPU-heavy apps or lower Preview Quality before retrying."
                    .to_owned(),
            );
        }
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
                    ui.ctx().request_repaint();
                } else if self.ui.active_tab == AppTab::Library
                    && self.library.folder_sidebar_open()
                {
                    self.set_library_folder_sidebar_open(false);
                } else if self.ui.active_tab == AppTab::Library && self.library.has_selection() {
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

        self.poll_load_worker(frame);
        self.poll_preview_rebuild_worker(frame);
        self.poll_preview_detail_rebuild_worker(frame);
        #[cfg(not(target_os = "android"))]
        self.poll_library_batch_export_worker();
        self.poll_export_worker(frame);
        #[cfg(target_os = "android")]
        self.resume_android_library_batch_export_if_possible(frame);
        self.poll_foreground_operation(frame);
        self.poll_library_ai_mask_refresh(frame);
        self.resume_pending_ai_denoise(frame);
        #[cfg(target_os = "android")]
        self.sync_android_export_notification();
        #[cfg(not(target_os = "android"))]
        {
            self.handle_edit_history_shortcuts(ui.ctx());
            self.handle_sidecar_shortcut(ui.ctx());
            if self.ui.active_tab == AppTab::Develop {
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
        if self.ui.active_tab == AppTab::Develop {
            egui::Panel::top("top_bar")
                .frame(crate::ui::theme::toolbar_frame(ui))
                .show(ui, |ui| TopBar::show(ui, self, frame));
        }

        if self.ui.active_tab == AppTab::Develop {
            match layout {
                ScreenLayout::Horizontal => {
                    #[cfg(not(target_os = "android"))]
                    {
                        egui::Panel::right("develop_tool_rail")
                            .resizable(false)
                            .exact_size(Sidebar::DESKTOP_TOOL_RAIL_WIDTH)
                            .frame(crate::ui::theme::panel_frame(ui))
                            .show(ui, |ui| Sidebar::show_desktop_tool_rail(ui, self));

                        if self.develop_ui.sidebar_open {
                            let panel_id = egui::Id::new("develop_sidebar_right");
                            let panel_max = (viewport_size.x * 0.48).clamp(
                                ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH,
                                ScreenLayout::MAX_HORIZONTAL_SIDEBAR_WIDTH,
                            );
                            let panel_width = develop_sidebar_user_width(
                                ui.ctx(),
                                panel_id,
                                sidebar_size.min(panel_max),
                                ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH,
                                panel_max,
                            );
                            let panel_width = dragged_develop_sidebar_width(
                                ui.ctx(),
                                panel_id,
                                ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH,
                                panel_max,
                            )
                            .unwrap_or(panel_width);
                            set_develop_sidebar_panel_width(ui.ctx(), panel_id, panel_width);
                            egui::Panel::right(panel_id)
                                .resizable(true)
                                .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                                // Keep the sidebar at the user's chosen width
                                // even while it is being resized. This prevents
                                // newly shown mask controls from widening the
                                // panel through their layout.
                                .max_size(panel_width)
                                .default_size(panel_width)
                                .frame(crate::ui::theme::panel_frame(ui))
                                .show(ui, |ui| Sidebar::show(ui, self, layout, frame));
                        }
                    }

                    #[cfg(target_os = "android")]
                    egui::Panel::right("develop_android_landscape_primary_tabs")
                        .resizable(false)
                        .exact_size(Sidebar::ANDROID_LANDSCAPE_TOOL_RAIL_WIDTH)
                        .frame(crate::ui::theme::panel_frame(ui))
                        .show(ui, |ui| {
                            Sidebar::show_android_landscape_primary_tabs(ui, self)
                        });

                    #[cfg(target_os = "android")]
                    egui::Panel::right("develop_sidebar_right")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                        .default_size(sidebar_size)
                        .frame(crate::ui::theme::panel_frame(ui))
                        .show(ui, |ui| Sidebar::show(ui, self, layout, frame));

                    #[cfg(not(target_os = "android"))]
                    if self.develop_ui.sidebar_open && self.ui.sidebar_tab == SidebarTab::Masks {
                        egui::Panel::right("develop_horizontal_mask_strip")
                            .resizable(false)
                            .exact_size(Sidebar::HORIZONTAL_MASK_STRIP_WIDTH)
                            .frame(crate::ui::theme::panel_frame(ui))
                            .show(ui, |ui| {
                                Sidebar::show_horizontal_mask_strip(ui, self, frame)
                            });
                    }
                    #[cfg(target_os = "android")]
                    if self.ui.sidebar_tab == SidebarTab::Masks {
                        egui::Panel::right("develop_horizontal_mask_strip")
                            .resizable(false)
                            .exact_size(Sidebar::HORIZONTAL_MASK_STRIP_WIDTH)
                            .frame(crate::ui::theme::panel_frame(ui))
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
                        .frame(crate::ui::theme::panel_frame(ui))
                        .show(ui, |ui| Sidebar::show(ui, self, layout, frame));

                    if self.ui.sidebar_tab == SidebarTab::Masks {
                        egui::Panel::bottom("develop_vertical_mask_strip")
                            .resizable(false)
                            .exact_size(Sidebar::VERTICAL_MASK_STRIP_HEIGHT)
                            .frame(crate::ui::theme::panel_frame(ui))
                            .show(ui, |ui| Sidebar::show_vertical_mask_strip(ui, self, frame));
                    }
                }
            }
        }

        #[cfg(not(target_os = "android"))]
        if self.ui.active_tab == AppTab::Develop && self.develop_ui.filmstrip_open {
            egui::Panel::bottom("filmstrip")
                .resizable(false)
                .exact_size(crate::ui::develop::FILMSTRIP_HEIGHT)
                .show(ui, |ui| Develop::show_filmstrip(ui, self, frame));
        }

        if self.ui.active_tab == AppTab::Library && self.library.folder_sidebar_open() {
            #[cfg(not(target_os = "android"))]
            {
                let panel_max = (viewport_size.x * 0.48).clamp(
                    ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH,
                    ScreenLayout::MAX_HORIZONTAL_SIDEBAR_WIDTH,
                );
                egui::Panel::left("library_folder_sidebar")
                    .resizable(true)
                    .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                    .max_size(panel_max)
                    .default_size(sidebar_size.min(panel_max))
                    .frame(crate::ui::theme::panel_frame(ui))
                    .show(ui, |ui| Library::show_folder_sidebar(ui, self));
            }
            #[cfg(target_os = "android")]
            egui::Panel::left("library_folder_sidebar")
                .resizable(false)
                .exact_size(
                    (viewport_size.x * 0.84)
                        .clamp(220.0, 380.0)
                        .min(viewport_size.x.max(1.0)),
                )
                .frame(crate::ui::theme::panel_frame(ui))
                .show(ui, |ui| Library::show_folder_sidebar(ui, self));
        }

        let central_panel = if self.ui.active_tab == AppTab::Develop {
            egui::CentralPanel::default().frame(
                egui::Frame::new()
                    .fill(self.preview_backdrop_color())
                    .inner_margin(egui::Margin::same(0)),
            )
        } else {
            egui::CentralPanel::default().frame(crate::ui::theme::workspace_frame(ui))
        };
        let _central = central_panel.show(ui, |ui| match self.ui.active_tab {
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
        self.sync_ai_model_runtime_context();
        #[cfg(not(target_os = "android"))]
        self.sync_discord_presence();

        #[cfg(not(target_os = "android"))]
        if self.ui.active_tab == AppTab::Develop {
            crate::ui::library::show_library_action_overlays(ui, self, frame);
        }

        self.advance_remove_worker(frame);
        self.apply_pending_lens_correction(frame);
        self.apply_pending_preview_quality(frame);
        self.sync_original_preview(frame);
        if !self.preview.original_requested {
            self.advance_navigation_preview(frame);
            self.advance_preview_detail(frame);
            self.advance_processing(frame);
        }
        self.refresh_status();

        if self.preview.processing_pending() {
            ui.ctx().request_repaint();
        }
        if self.foreground_operation_active()
            || self.export.task.is_some()
            || self.export.publish_pending
            || self.preview.rebuild_receiver.is_some()
            || self.preview.detail_rebuild_receiver.is_some()
            || self.inpaint.processing()
        {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        #[cfg(not(target_os = "android"))]
        if self.ui.desktop_picker_receiver.is_some() {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
        #[cfg(target_os = "android")]
        if self.android.picker_pending {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
        }
        #[cfg(not(target_os = "android"))]
        if raw_drop_hovered {
            show_raw_drop_overlay(ui, self.library.folder());
        }
        crate::ui::onboarding::show(ui.ctx(), self);
        self.show_subject_dialogs(ui.ctx());
        self.show_remove_model_dialog(ui.ctx(), frame);
        self.show_ai_denoise_dialogs(ui.ctx(), frame);
        self.show_sidecar_save_error_dialog(ui.ctx());
        self.show_foreground_operation_dialog(ui.ctx());
        self.show_export_task_dialog(ui.ctx());
        let edit_interaction_active = sidecar_interaction_active(ui.ctx());
        self.observe_edit_history(ui.ctx());
        self.schedule_sidecar_autosave(ui.ctx(), edit_interaction_active);
        self.poll_sidecar_save();
        self.poll_developed_thumbnail(frame);
        #[cfg(target_os = "android")]
        crate::android::set_back_navigation_active(
            self.ui.active_tab != AppTab::Library
                || self.library.has_selection()
                || self.library.folder_sidebar_open(),
        );
    }

    fn on_exit(&mut self) {
        calibraw_ai::set_active_ai_context(None);
        #[cfg(not(target_os = "android"))]
        self.discord_presence.shutdown();
        #[cfg(target_os = "android")]
        {
            if let Err(error) =
                crate::android::clear_background_task_notification(&self.android.android_app)
            {
                log::warn!("{error}");
            }
            crate::android::uninstall_context();
        }
        self.persist_performance_settings();
        self.flush_sidecar_on_exit();
    }
}

#[cfg(target_os = "android")]
pub(super) fn show_android_foreground_task_blocker(ctx: &egui::Context) {
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
                "Keep CalibRaw open in the foreground until the operation finishes. Leaving or closing the app may stop it.",
                egui::FontId::proportional(13.0),
                egui::Color32::from_white_alpha(210),
            );
        });
}

#[cfg(not(target_os = "android"))]
pub(super) fn show_raw_drop_overlay(ui: &egui::Ui, folder: Option<&std::path::Path>) {
    let rect = ui.max_rect().shrink(18.0);
    let painter = ui.ctx().layer_painter(egui::LayerId::new(
        egui::Order::Foreground,
        egui::Id::new("calibraw-raw-drop-overlay"),
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
