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
                            .show(ui, |ui| {
                                Sidebar::show_vertical_mask_strip(ui, self, frame)
                            });
                    }
                }
            }
        }

        egui::CentralPanel::default().show(ui, |ui| match self.active_tab {
            AppTab::Library => Library::show(ui),
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

        self.apply_pending_lens_correction(frame);
        self.apply_pending_preview_quality(frame);
        self.advance_processing(frame);
        self.advance_preview_detail(frame);
        self.refresh_status();

        if self.pending_stage.is_some() {
            ui.ctx().request_repaint();
        }
        if self.export_receiver.is_some() || self.export_publish_pending {
            ui.ctx().request_repaint_after(Duration::from_millis(80));
        }
        self.show_subject_dialogs(ui.ctx());
    }
}
