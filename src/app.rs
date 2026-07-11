use crate::pipeline::{load_raw_file, ExposureParams, GpuParams, LoadedRaw, RawGpuPipeline};
use crate::ui::settings::Settings;
use crate::ui::sidebar::Sidebar;
use eframe::egui;
use std::path::PathBuf;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AppTab {
    Library,
    #[default]
    Develop,
    Settings,
}

pub struct AurawApp {
    pub current_path: Option<PathBuf>,
    pub loaded_raw: Option<LoadedRaw>,
    pub gpu_pipeline: Option<RawGpuPipeline>,
    pub exposure: ExposureParams,
    pub active_tab: AppTab,

    pub dirty: bool,

    pub status: String,

    #[cfg(target_os = "android")]
    android_app: android_activity::AndroidApp,
    #[cfg(target_os = "android")]
    picker_pending: bool,
}

impl AurawApp {
    fn install_lightroom_visuals(ctx: &egui::Context) {
        // Start from egui's robust dark palette, then make the editor panels a
        // little calmer and denser for a Lightroom-like darkroom layout.
        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = egui::Color32::from_rgb(30, 32, 35);
        visuals.faint_bg_color = egui::Color32::from_rgb(42, 45, 49);
        visuals.extreme_bg_color = egui::Color32::from_rgb(18, 20, 22);
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
        style.spacing.slider_width = 170.0;
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        ctx.set_style_of(egui::Theme::Dark, style);
    }

    #[cfg(not(target_os = "android"))]
    fn empty() -> Self {
        Self {
            current_path: None,
            loaded_raw: None,
            gpu_pipeline: None,
            exposure: ExposureParams::default(),
            active_tab: AppTab::default(),
            dirty: false,
            status: "Open a RAW file to get started.".to_owned(),
        }
    }

    #[cfg(not(target_os = "android"))]
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::install_lightroom_visuals(&cc.egui_ctx);
        Self::empty()
    }

    #[cfg(target_os = "android")]
    pub fn new_android(
        cc: &eframe::CreationContext<'_>,
        android_app: android_activity::AndroidApp,
    ) -> Self {
        crate::android::install_context(&cc.egui_ctx);
        Self::install_lightroom_visuals(&cc.egui_ctx);
        Self {
            current_path: None,
            loaded_raw: None,
            gpu_pipeline: None,
            exposure: ExposureParams::default(),
            active_tab: AppTab::default(),
            dirty: false,
            status: "Open a RAW file to get started.".to_owned(),
            android_app,
            picker_pending: false,
        }
    }

    #[cfg(not(target_os = "android"))]
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

    #[cfg(target_os = "android")]
    pub fn open_file_dialog(&mut self, _frame: &eframe::Frame) {
        if self.picker_pending {
            return;
        }
        match crate::android::open_raw_document(&self.android_app) {
            Ok(()) => {
                self.picker_pending = true;
                self.status = "Choose a RAW file…".to_owned();
            }
            Err(error) => self.status = error,
        }
    }

    pub fn open_path(&mut self, path: PathBuf, frame: &eframe::Frame) {
        let label = path.display().to_string();
        self.open_path_labeled(path, label, false, frame);
    }

    fn open_path_labeled(
        &mut self,
        path: PathBuf,
        label: String,
        delete_after_decode: bool,
        frame: &eframe::Frame,
    ) {
        self.status = format!("Decoding {label}…");

        let decoded = load_raw_file(&path);
        if delete_after_decode {
            if let Err(error) = std::fs::remove_file(&path) {
                log::warn!("could not remove imported Android RAW cache file: {error}");
            }
        }

        let raw = match decoded {
            Ok(r) => r,
            Err(e) => {
                self.status = format!("Failed to decode {label}: {e:#}");
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
                self.current_path = (!delete_after_decode).then_some(path);
                self.dirty = true;
            }
            Err(e) => {
                self.status = format!("GPU pipeline setup failed: {e:#}");
                log::error!("{e:#}");
            }
        }
    }

    #[cfg(target_os = "android")]
    fn poll_android_picker(&mut self, frame: &eframe::Frame) {
        while let Some(result) = crate::android::take_picker_result() {
            self.picker_pending = false;
            match result {
                crate::android::PickerResult::Picked(document) => {
                    self.open_path_labeled(document.path, document.display_name, true, frame)
                }
                crate::android::PickerResult::Cancelled => {
                    self.status = "No RAW file selected.".to_owned();
                }
                crate::android::PickerResult::Failed(error) => {
                    self.status = format!("Could not import the selected file: {error}");
                }
            }
        }
    }

    fn recompute_if_dirty(&mut self, frame: &eframe::Frame) {
        if !self.dirty {
            return;
        }
        let (Some(raw), Some(pipeline)) = (&self.loaded_raw, &self.gpu_pipeline) else {
            self.dirty = false;
            return;
        };
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };

        let params = GpuParams::new(&self.exposure, raw);
        pipeline.recompute(&render_state.queue, &render_state.device, &params);
        self.dirty = false;
    }

    pub(crate) fn mark_pipeline_dirty(&mut self) {
        if self.gpu_pipeline.is_some() {
            self.dirty = true;
        }
    }

    pub(crate) fn reset_develop_adjustments(&mut self) {
        let previous = self.exposure;
        self.exposure = ExposureParams::default();

        // Highlight reconstruction is an application-level processing preference,
        // not one of the Lightroom-style Develop adjustments.
        self.exposure.highlight_method = previous.highlight_method;
        self.exposure.highlight_clip = previous.highlight_clip;
        self.exposure.highlight_reconstruction = previous.highlight_reconstruction;
        self.exposure.highlight_iterations = previous.highlight_iterations;
        self.exposure.highlight_color_adaptation = previous.highlight_color_adaptation;

        self.mark_pipeline_dirty();
    }

    pub(crate) fn reset_highlight_reconstruction_settings(&mut self) {
        let defaults = ExposureParams::default();
        self.exposure.highlight_method = defaults.highlight_method;
        self.exposure.highlight_clip = defaults.highlight_clip;
        self.exposure.highlight_reconstruction = defaults.highlight_reconstruction;
        self.exposure.highlight_iterations = defaults.highlight_iterations;
        self.exposure.highlight_color_adaptation = defaults.highlight_color_adaptation;
        self.mark_pipeline_dirty();
    }

    fn show_top_bar(&mut self, ui: &mut egui::Ui, frame: &eframe::Frame) {
        ui.horizontal(|ui| {
            ui.selectable_value(&mut self.active_tab, AppTab::Library, "Library");
            ui.selectable_value(&mut self.active_tab, AppTab::Develop, "Develop");
            ui.selectable_value(&mut self.active_tab, AppTab::Settings, "Settings");
        });

        ui.separator();
        ui.horizontal_wrapped(|ui| {
            if ui.button("Open RAW…").clicked() {
                self.open_file_dialog(frame);
            }
            ui.separator();
            ui.label(&self.status);
        });
    }

    fn show_library(ui: &mut egui::Ui) {
        ui.centered_and_justified(|ui| {
            ui.vertical_centered(|ui| {
                ui.heading("Library");
                ui.label("Library management is coming soon.");
                ui.add_space(4.0);
                ui.label("Use Open RAW… above, then switch to Develop to edit an image.");
            });
        });
    }

    fn show_develop(&self, ui: &mut egui::Ui) {
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
    }
}

impl eframe::App for AurawApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        #[cfg(target_os = "android")]
        self.poll_android_picker(frame);

        egui::Panel::top("top_bar").show(ui, |ui| self.show_top_bar(ui, frame));

        #[cfg(not(target_os = "android"))]
        if self.active_tab == AppTab::Develop {
            egui::Panel::right("sidebar")
                .resizable(true)
                .default_size(320.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .auto_shrink([false, false])
                        .show(ui, |ui| Sidebar::show(ui, self));
                });
        }

        #[cfg(target_os = "android")]
        if self.active_tab == AppTab::Develop {
            egui::Panel::bottom("sidebar")
                .resizable(true)
                .default_size(280.0)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| Sidebar::show(ui, self));
                });
        }

        egui::CentralPanel::default().show(ui, |ui| match self.active_tab {
            AppTab::Library => Self::show_library(ui),
            AppTab::Develop => self.show_develop(ui),
            AppTab::Settings => {
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| Settings::show(ui, self));
            }
        });

        self.recompute_if_dirty(frame);

        if self.dirty {
            ui.ctx().request_repaint();
        }
    }
}
