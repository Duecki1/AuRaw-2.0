use crate::app::AurawApp;
use eframe::egui::{self, Ui};

pub struct Preview;

impl Preview {
    pub fn show(ui: &mut Ui, app: &AurawApp) {
        let Some(pipeline) = &app.gpu_pipeline else {
            ui.centered_and_justified(|ui| {
                ui.vertical_centered(|ui| {
                    ui.heading("No image open");
                    ui.label("Use Open RAW… to load an image.");
                });
            });
            return;
        };
        let Some(texture_id) = pipeline.egui_texture_id else {
            ui.centered_and_justified(|ui| {
                ui.spinner();
                ui.label("Preparing preview…");
            });
            return;
        };

        let available = ui.available_size();
        if available.x <= 0.0 || available.y <= 0.0 || pipeline.height == 0 {
            return;
        }

        let image_aspect = pipeline.width as f32 / pipeline.height as f32;
        let available_aspect = available.x / available.y;
        let size = if available_aspect > image_aspect {
            egui::vec2(available.y * image_aspect, available.y)
        } else {
            egui::vec2(available.x, available.x / image_aspect)
        };

        ui.centered_and_justified(|ui| {
            ui.add(
                egui::Image::new(egui::load::SizedTexture::new(
                    texture_id,
                    size,
                ))
                .fit_to_exact_size(size),
            );
        });
    }
}
