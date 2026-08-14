use super::*;

impl LibraryState {
    pub(super) fn refresh_cloud_trash(&mut self, context: &egui::Context) {
        if self.cloud_action_receiver.is_some() {
            self.status = "Wait for the current Trash action to finish.".to_owned();
            return;
        }
        let config = self.cloud_config.clone();
        let repaint = context.clone();
        let (sender, receiver) = mpsc::channel();
        self.cloud_trash_receiver = Some(receiver);
        self.catalog_ready = false;
        self.status = "Refreshing AuRaw Cloud Trash…".to_owned();
        let spawn = std::thread::Builder::new()
            .name("auraw-cloud-trash".to_owned())
            .spawn(move || {
                let _ = sender.send(crate::cloud::list_trash(&config));
                repaint.request_repaint();
            });
        if let Err(error) = spawn {
            self.cloud_trash_receiver = None;
            self.catalog_ready = true;
            self.status = format!("Could not start the Trash refresh: {error}");
        }
    }

    pub(super) fn poll_cloud_trash(&mut self) {
        let received = self
            .cloud_trash_receiver
            .as_ref()
            .map(mpsc::Receiver::try_recv);
        match received {
            Some(Ok(Ok(catalog))) => {
                self.cloud_trash_receiver = None;
                self.cloud_trash_server_time = catalog.server_time;
                self.cloud_trash_retention_days = catalog.retention_days;
                self.cloud_trash_items = catalog.items;
                self.cloud_trash_selection
                    .retain(|id| self.cloud_trash_items.iter().any(|item| &item.id == id));
                self.catalog_ready = true;
                self.status = format!(
                    "Trash · {} item{} · retained for {} days",
                    self.cloud_trash_items.len(),
                    if self.cloud_trash_items.len() == 1 {
                        ""
                    } else {
                        "s"
                    },
                    catalog.retention_days
                );
            }
            Some(Ok(Err(error))) => {
                self.cloud_trash_receiver = None;
                self.catalog_ready = true;
                self.status = error;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.cloud_trash_receiver = None;
                self.catalog_ready = true;
                self.status = "The AuRaw Cloud Trash refresh stopped unexpectedly.".to_owned();
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
    }

}

pub(super) fn trash_age_label(seconds: u64) -> String {
    if seconds < 60 {
        "just now".to_owned()
    } else if seconds < 60 * 60 {
        format!("{} min ago", seconds / 60)
    } else if seconds < 24 * 60 * 60 {
        format!("{} h ago", seconds / (60 * 60))
    } else {
        let days = seconds / (24 * 60 * 60);
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

pub(super) fn trash_remaining_label(seconds: u64) -> String {
    if seconds == 0 {
        "expires now".to_owned()
    } else if seconds < 24 * 60 * 60 {
        let hours = seconds.div_ceil(60 * 60);
        format!("{hours} h remaining")
    } else {
        let days = seconds.div_ceil(24 * 60 * 60);
        format!("{days} day{} remaining", if days == 1 { "" } else { "s" })
    }
}

pub(super) fn trash_size_label(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{bytes} B")
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KiB", bytes as f64 / 1024.0)
    } else if bytes < 1024 * 1024 * 1024 {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:.1} GiB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

pub(super) fn show_cloud_trash_panel(ui: &mut Ui, app: &mut AurawApp) {
    let action_enabled =
        !app.library.cloud_action_in_progress() && app.library.cloud_trash_receiver.is_none();
    let items = app.library.cloud_trash_items.clone();
    let selected = items
        .iter()
        .filter(|item| app.library.cloud_trash_selection.contains(&item.id))
        .cloned()
        .collect::<Vec<_>>();
    let mut request = None;
    let mut request_delete = None;
    ui.horizontal_wrapped(|ui| {
        ui.heading("Trash");
        ui.label(
            egui::RichText::new(format!(
                "Deleted bundles are retained for {} days.",
                app.library.cloud_trash_retention_days
            ))
            .small()
            .color(ui.visuals().weak_text_color()),
        );
        ui.separator();
        if ui
            .add_enabled(
                action_enabled && !selected.is_empty(),
                egui::Button::new(format!("Restore selected ({})", selected.len())),
            )
            .clicked()
        {
            request = Some(CloudActionRequest::RestoreTrash {
                items: selected.clone(),
            });
        }
        if ui
            .add_enabled(
                action_enabled && !selected.is_empty(),
                egui::Button::new("Permanently delete selected…"),
            )
            .clicked()
        {
            request_delete = Some(CloudTrashDeleteTarget::Selected(selected.clone()));
        }
        if ui
            .add_enabled(
                action_enabled && !items.is_empty(),
                egui::Button::new("Empty Trash…"),
            )
            .clicked()
        {
            request_delete = Some(CloudTrashDeleteTarget::Empty);
        }
    });
    ui.separator();

    if app.library.catalog_ready && items.is_empty() {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Trash is empty");
                ui.label("Deleted cloud RAWs and folders will appear here.");
            });
        });
    } else {
        egui::ScrollArea::vertical().show(ui, |ui| {
            for item in &items {
                let mut checked = app.library.cloud_trash_selection.contains(&item.id);
                ui.group(|ui| {
                    ui.horizontal(|ui| {
                        if ui.checkbox(&mut checked, "").changed() {
                            if checked {
                                app.library.cloud_trash_selection.insert(item.id.clone());
                            } else {
                                app.library.cloud_trash_selection.remove(&item.id);
                            }
                        }
                        let icon = if item.kind == "folder" {
                            egui_phosphor::regular::FOLDER
                        } else {
                            egui_phosphor::regular::IMAGE
                        };
                        ui.strong(format!("{icon}  {}", item.name));
                        ui.label(trash_size_label(item.bytes));
                        if item.kind == "folder" {
                            ui.label(format!(
                                "{} bundled item{}",
                                item.item_count,
                                if item.item_count == 1 { "" } else { "s" }
                            ));
                        }
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui
                                .add_enabled(action_enabled, egui::Button::new("Restore"))
                                .clicked()
                            {
                                request = Some(CloudActionRequest::RestoreTrash {
                                    items: vec![item.clone()],
                                });
                            }
                        });
                    });
                    let age = app
                        .library
                        .cloud_trash_server_time
                        .saturating_sub(item.deleted_seconds);
                    let remaining = item
                        .expires_seconds
                        .saturating_sub(app.library.cloud_trash_server_time);
                    ui.label(
                        egui::RichText::new(format!(
                            "Deleted {} · {}",
                            trash_age_label(age),
                            trash_remaining_label(remaining)
                        ))
                        .small()
                        .color(ui.visuals().weak_text_color()),
                    );
                });
                ui.add_space(4.0);
            }
        });
    }
    if let Some(target) = request_delete {
        app.library.cloud_trash_delete_confirmation = Some(target);
    }

    let confirmation = app.library.cloud_trash_delete_confirmation.clone();
    let mut close_confirmation = false;
    if let Some(target) = confirmation {
        let (count, empty) = match &target {
            CloudTrashDeleteTarget::Selected(items) => (items.len(), false),
            CloudTrashDeleteTarget::Empty => (items.len(), true),
        };
        egui::Window::new(if empty {
            "Empty Cloud Trash?"
        } else {
            "Permanently delete selected items?"
        })
        .id(egui::Id::new("cloud-trash-permanent-confirmation"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ui.ctx(), |ui| {
            ui.label(format!(
                "Permanently delete {count} Trash item{}?",
                if count == 1 { "" } else { "s" }
            ));
            ui.label(
                egui::RichText::new("This cannot be undone.")
                    .strong()
                    .color(ui.visuals().warn_fg_color),
            );
            ui.horizontal(|ui| {
                if ui.button("Cancel").clicked() {
                    close_confirmation = true;
                }
                if ui.button("Permanently delete").clicked() {
                    request = Some(match target.clone() {
                        CloudTrashDeleteTarget::Selected(items) => {
                            CloudActionRequest::PermanentlyDeleteTrash { items }
                        }
                        CloudTrashDeleteTarget::Empty => CloudActionRequest::EmptyTrash,
                    });
                    close_confirmation = true;
                }
            });
        });
    }
    if close_confirmation {
        app.library.cloud_trash_delete_confirmation = None;
    }
    if let Some(request) = request {
        app.library.cloud_trash_selection.clear();
        app.library.start_cloud_action(request, ui.ctx());
    }
}

