impl AurawApp {
    fn handle_android_tab_swipe(&mut self, ctx: &egui::Context, content_rect: egui::Rect) {
        if !cfg!(target_os = "android") {
            self.android_tab_swipe = None;
            return;
        }

        const HORIZONTAL_DOMINANCE: f32 = 1.35;
        const VERTICAL_CANCEL_POINTS: f32 = 18.0;

        let (primary_pressed, primary_released, primary_down, pointer_pos, multi_touch) =
            ctx.input(|input| {
                (
                    input.pointer.primary_pressed(),
                    input.pointer.primary_released(),
                    input.pointer.primary_down(),
                    input.pointer.interact_pos(),
                    input.multi_touch().is_some(),
                )
            });

        let editing_blocks_swipe = slider_scroll_locked(ctx)
            || multi_touch
            || self.preview_touch_navigation_active
            || (self.active_tab == AppTab::Develop
                && (self.sidebar_tab == SidebarTab::Masks || self.preview_zoom > 1.01));

        if primary_pressed {
            self.android_tab_swipe = pointer_pos
                .filter(|position| content_rect.contains(*position))
                .map(|position| AndroidTabSwipeState {
                    origin: position,
                    latest: position,
                    start_tab: self.active_tab,
                    cancelled: editing_blocks_swipe,
                });
        }

        if let (Some(state), Some(position)) = (self.android_tab_swipe.as_mut(), pointer_pos) {
            state.latest = position;
            if editing_blocks_swipe || !content_rect.expand(24.0).contains(position) {
                state.cancelled = true;
            }

            let delta = state.latest - state.origin;
            if delta.y.abs() >= VERTICAL_CANCEL_POINTS
                && delta.y.abs() > delta.x.abs() / HORIZONTAL_DOMINANCE
            {
                state.cancelled = true;
            }
        }

        if primary_released {
            let Some(state) = self.android_tab_swipe.take() else {
                return;
            };
            if state.cancelled || state.start_tab != self.active_tab || editing_blocks_swipe {
                return;
            }

            let delta = state.latest - state.origin;
            let swipe_distance = (content_rect.width() * 0.18).clamp(56.0, 96.0);
            if delta.x.abs() < swipe_distance
                || delta.x.abs() < delta.y.abs() * HORIZONTAL_DOMINANCE
            {
                return;
            }

            let destination = if delta.x < 0.0 {
                self.active_tab.next()
            } else {
                self.active_tab.previous()
            };
            if let Some(destination) = destination {
                let previous_tab = self.active_tab;
                self.active_tab = destination;
                TopBar::prepare_tab_transition(self, previous_tab);
                ctx.request_repaint();
            }
        } else if !primary_down {
            self.android_tab_swipe = None;
        }
    }
}

impl eframe::App for AurawApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        #[cfg(target_os = "android")]
        {
            self.poll_android_picker(frame);
            self.poll_android_export_publish();
        }

        self.poll_load_worker(frame);
        self.poll_export_worker();
        self.poll_subject_worker();
        self.poll_object_worker();
        self.handle_edit_history_shortcuts(ui.ctx());
        self.handle_sidecar_shortcut(ui.ctx());

        let viewport_size = ui.max_rect().size();
        let layout = ScreenLayout::from_size(viewport_size);
        let sidebar_size = layout.sidebar_default_size(viewport_size);

        self.refresh_status();
        egui::Panel::top("top_bar").show(ui, |ui| TopBar::show(ui, self, frame));

        if self.active_tab == AppTab::Develop {
            match layout {
                ScreenLayout::Horizontal => {
                    egui::Panel::right("develop_sidebar_right")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                        .default_size(sidebar_size)
                        .show(ui, |ui| Sidebar::show(ui, self, layout, frame));

                    // As with the portrait bottom panels, panel call order keeps
                    // the fixed strip immediately beside the resizable sidebar.
                    // This second right panel is therefore placed to its left.
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

        let central_panel = egui::CentralPanel::default().show(ui, |ui| match self.active_tab {
            AppTab::Library => Library::show(ui, self, frame),
            AppTab::Develop => Preview::show(ui, self),
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
        self.handle_android_tab_swipe(ui.ctx(), central_panel.response.rect);

        self.apply_pending_lens_correction(frame);
        self.apply_pending_preview_quality(frame);
        // Keep the tiny full-frame navigation proxy current before rendering a
        // visible high-resolution crop. Detail Dehaze/adaptive-tone output can
        // then inherit one stable set of full-image statistics while panning.
        self.advance_navigation_preview(frame);
        self.advance_preview_detail(frame);
        self.advance_processing(frame);
        self.refresh_status();

        if self.preview_detail_pending_stage.is_some()
            || self.navigation_pending_stage.is_some()
            || (self.preview_zoom <= DETAIL_ZOOM_START && self.pending_stage.is_some())
        {
            ui.ctx().request_repaint();
        }
        if self.export_receiver.is_some() || self.export_publish_pending {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        self.show_subject_dialogs(ui.ctx());
        let edit_interaction_active = sidecar_interaction_active(ui.ctx());
        self.observe_edit_history(ui.ctx());
        self.schedule_sidecar_autosave(ui.ctx(), edit_interaction_active);
        // Poll after edit observation so an autosave that waited behind an
        // interaction can be coalesced to its final committed value before
        // the next worker starts.
        self.poll_sidecar_save();
        self.poll_developed_thumbnail(frame);
    }

    fn on_exit(&mut self) {
        self.flush_sidecar_on_exit();
    }
}
