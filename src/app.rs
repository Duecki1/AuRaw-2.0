use crate::pipeline::{load_raw_file, ExposureParams, GpuParams, LoadedRaw, RawGpuPipeline};
use crate::ui::sidebar::Sidebar;
use eframe::egui;
use std::path::PathBuf;

pub struct AurawApp {
    pub current_path: Option<PathBuf>,
    pub loaded_raw: Option<LoadedRaw>,
    pub gpu_pipeline: Option<RawGpuPipeline>,
    pub exposure: ExposureParams,

    pub dirty: bool,

    pub status: String,
}

impl Default for AurawApp {
    fn default() -> Self {
        Self {
            current_path: None,
            loaded_raw: None,
            gpu_pipeline: None,
            exposure: ExposureParams::default(),
            dirty: false,
            status: "Open a RAW file to get started.".to_owned(),
        }
    }
}

impl AurawApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self::default()
    }

    pub fn open_file_dialog(&mut self, frame: &eframe::Frame) {
        let Some(path) = rfd::FileDialog::new()
            .add_filter(
                "RAW images",
                &[
                    "cr2", "CR2", "cr3", "CR3", "nef", "NEF", "arw", "ARW", "raf", "RAF", "rw2",
                    "RW2", "orf", "ORF", "dng", "DNG", "pef", "PEF", "srw", "SRW",
                ],
            )
            .pick_file()
        else {
            return;
        };

        self.open_path(path, frame);
    }

    pub fn open_path(&mut self, path: PathBuf, frame: &eframe::Frame) {
        self.status = format!("Decoding {}…", path.display());

        let raw = match load_raw_file(&path) {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Failed to decode {}: {e:#}", path.display());
                log::error!("{e:#}");
                return;
            }
        };

        let Some(render_state) = frame.wgpu_render_state() else {
            self.status = "eframe is not running with the wgpu backend.".to_owned();
            return;
        };

        let device = &render_state.device;
        let queue = &render_state.queue;
        let mut renderer = render_state.renderer.write();

        if let Some(old) = self.gpu_pipeline.take() {
            renderer.free_texture(&old.egui_texture_id);
        }

        let params = GpuParams::new(&self.exposure, &raw);

        match RawGpuPipeline::new(device, queue, &mut renderer, &raw, &params) {
            Ok(pipeline) => {
                self.status = format!(
                    "{} {} — {}x{}",
                    raw.camera_make, raw.camera_model, raw.width, raw.height
                );
                self.gpu_pipeline = Some(pipeline);
                self.loaded_raw = Some(raw);
                self.current_path = Some(path);
                self.dirty = true;
            }
            Err(e) => {
                self.status = format!("GPU pipeline setup failed: {e:#}");
                log::error!("{e:#}");
            }
        }
    }

    fn recompute_if_dirty(&mut self, frame: &eframe::Frame) {
        if !self.dirty {
            return;
        }
        let (Some(raw), Some(pipeline)) = (&self.loaded_raw, &self.gpu_pipeline) else {
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        let params = GpuParams::new(&self.exposure, raw);
        pipeline.recompute(&render_state.queue, &render_state.device, &params);
        self.dirty = false;
    }
}

impl eframe::App for AurawApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        egui::Panel::top("top_bar").show(ui, |ui| {
            ui.horizontal(|ui| {
                if ui.button("Open RAW…").clicked() {
                    self.open_file_dialog(frame);
                }
                ui.separator();
                ui.label(&self.status);
            });
        });

        egui::Panel::right("sidebar")
            .resizable(true)
            .default_size(280.0)
            .show(ui, |ui| {
                Sidebar::show(ui, self);
            });

        egui::CentralPanel::default().show(ui, |ui| {
            if let Some(pipeline) = &self.gpu_pipeline {
                let avail = ui.available_size();
                let img_aspect = pipeline.width as f32 / pipeline.height as f32;
                let avail_aspect = avail.x / avail.y;

                let size = if avail_aspect > img_aspect {
                    egui::vec2(avail.y * img_aspect, avail.y)
                } else {
                    egui::vec2(avail.x, avail.x / img_aspect)
                };

                ui.centered_and_justified(|ui| {
                    ui.add(
                        egui::Image::new(egui::load::SizedTexture::new(
                            pipeline.egui_texture_id,
                            size,
                        ))
                        .fit_to_exact_size(size),
                    );
                });
            } else {
                ui.centered_and_justified(|ui| {
                    ui.label("No image open. Use \"Open RAW…\" above.");
                });
            }
        });

        self.recompute_if_dirty(frame);

        if self.dirty {
            ui.ctx().request_repaint();
        }
    }
}
