use crate::pipeline::{load_raw_file, ExposureParams, GpuParams, LoadedRaw, RawGpuPipeline};
use crate::ui::layout::ScreenLayout;
use crate::ui::library::Library;
use crate::ui::preview::Preview;
use crate::ui::settings::Settings;
use crate::ui::sidebar::Sidebar;
use crate::ui::top_bar::TopBar;
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
        let accent = egui::Color32::from_rgb(56, 139, 253);

        visuals.panel_fill = egui::Color32::from_rgb(24, 26, 29);
        visuals.window_fill = egui::Color32::from_rgb(27, 29, 33);
        visuals.faint_bg_color = egui::Color32::from_rgb(35, 38, 43);
        visuals.extreme_bg_color = egui::Color32::from_rgb(16, 18, 20);
        visuals.selection.bg_fill = accent;
        visuals.hyperlink_color = accent;
        visuals.widgets.inactive.bg_fill = egui::Color32::from_rgb(42, 45, 50);
        visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(54, 58, 65);
        visuals.widgets.active.bg_fill = accent;
        ctx.set_visuals(visuals);

        let mut style = (*ctx.style_of(egui::Theme::Dark)).clone();
        style.text_styles.insert(
            egui::TextStyle::Body,
            egui::FontId::proportional(13.0),
        );
        style.text_styles.insert(
            egui::TextStyle::Button,
            egui::FontId::proportional(12.5),
        );
        style.text_styles.insert(
            egui::TextStyle::Small,
            egui::FontId::proportional(11.5),
        );
        style.spacing.slider_width = 220.0;
        style.spacing.item_spacing = egui::vec2(7.0, 4.0);
        style.spacing.button_padding = egui::vec2(9.0, 4.0);
        style.spacing.interact_size.y = 24.0;
        style.spacing.indent = 12.0;
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

        // Demosaic selection is likewise a raw-processing preference rather
        // than a Develop adjustment. Resetting exposure/tone controls must not
        // silently change the reconstruction algorithm.
        self.exposure.demosaic_mode = previous.demosaic_mode;
        self.exposure.dual_threshold = previous.dual_threshold;
        self.exposure.frequency_chroma = previous.frequency_chroma;

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
}

impl eframe::App for AurawApp {
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        #[cfg(target_os = "android")]
        self.poll_android_picker(frame);

        let viewport_size = ui.max_rect().size();
        let layout = ScreenLayout::from_size(viewport_size);
        let sidebar_size = layout.sidebar_default_size(viewport_size);

        egui::Panel::top("top_bar").show(ui, |ui| TopBar::show(ui, self, frame));

        if self.active_tab == AppTab::Develop {
            match layout {
                ScreenLayout::Horizontal => {
                    egui::Panel::right("develop_sidebar_right")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_HORIZONTAL_SIDEBAR_WIDTH)
                        .default_size(sidebar_size)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| Sidebar::show(ui, self, layout));
                        });
                }
                ScreenLayout::Vertical => {
                    egui::Panel::bottom("develop_sidebar_bottom")
                        .resizable(true)
                        .min_size(ScreenLayout::MIN_VERTICAL_SIDEBAR_HEIGHT)
                        .default_size(sidebar_size)
                        .show(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .auto_shrink([false, false])
                                .show(ui, |ui| Sidebar::show(ui, self, layout));
                        });
                }
            }
        }

        egui::CentralPanel::default().show(ui, |ui| match self.active_tab {
            AppTab::Library => Library::show(ui),
            AppTab::Develop => Preview::show(ui, self),
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
