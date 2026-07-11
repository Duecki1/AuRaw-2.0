use crate::app::AurawApp;
use crate::pipeline::{DemosaicMode, ExposureParams};
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::layout::ScreenLayout;
use eframe::egui::{self, Ui};

pub struct GeneralTab;

impl GeneralTab {
    pub fn show(
        ui: &mut Ui,
        app: &mut AurawApp,
        layout: ScreenLayout,
        touch_safe: bool,
    ) {
        ui.horizontal(|ui| {
            ui.heading("Adjustments");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset all").clicked() {
                    app.reset_develop_adjustments();
                }
            });
        });
        ui.label(
            egui::RichText::new("Scene-referred controls")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);
        ui.separator();

        let mut changed = false;
        let use_columns = layout.is_vertical() && ui.available_width() >= 720.0;

        if use_columns {
            ui.columns(2, |columns| {
                changed |= Self::show_basic(&mut columns[0], &mut app.exposure, touch_safe);
                changed |= Self::show_hsl(&mut columns[0], &mut app.exposure, touch_safe);

                changed |= Self::show_presence(&mut columns[1], &mut app.exposure, touch_safe);
                changed |= Self::show_raw(&mut columns[1], &mut app.exposure, touch_safe);
            });
        } else {
            changed |= Self::show_basic(ui, &mut app.exposure, touch_safe);
            changed |= Self::show_presence(ui, &mut app.exposure, touch_safe);
            changed |= Self::show_hsl(ui, &mut app.exposure, touch_safe);
            changed |= Self::show_raw(ui, &mut app.exposure, touch_safe);
        }

        if changed {
            app.mark_pipeline_dirty();
        }
    }

    fn show_basic(ui: &mut Ui, exposure: &mut ExposureParams, touch_safe: bool) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Basic (Tone & Exposure)")
            .default_open(true)
            .show(ui, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Exposure (EV)",
                    &mut exposure.exposure,
                    -5.0..=5.0,
                    2,
                    0.05,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Contrast",
                    &mut exposure.contrast,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Highlights",
                    &mut exposure.highlights,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Shadows",
                    &mut exposure.shadows,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Whites",
                    &mut exposure.whites,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Blacks",
                    &mut exposure.blacks,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
            });
        changed
    }

    fn show_presence(ui: &mut Ui, exposure: &mut ExposureParams, touch_safe: bool) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Presence")
            .default_open(true)
            .show(ui, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Texture",
                    &mut exposure.texture,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Clarity",
                    &mut exposure.clarity,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Dehaze",
                    &mut exposure.dehaze,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Vibrance",
                    &mut exposure.vibrance,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Saturation",
                    &mut exposure.saturation,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                    touch_safe,
                );
            });
        changed
    }

    fn show_hsl(ui: &mut Ui, exposure: &mut ExposureParams, touch_safe: bool) -> bool {
        const COLORS: [&str; 8] = [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ];

        let mut changed = false;
        egui::CollapsingHeader::new("HSL / Color Mixer")
            .default_open(false)
            .show(ui, |ui| {
                for (index, color) in COLORS.iter().enumerate() {
                    ui.push_id(index, |ui| {
                        ui.strong(*color);
                        changed |= adjustment_slider(
                            ui,
                            "Hue",
                            &mut exposure.hsl_hue[index],
                            -100.0..=100.0,
                            0,
                            1.0,
                            None,
                            touch_safe,
                        );
                        changed |= adjustment_slider(
                            ui,
                            "Saturation",
                            &mut exposure.hsl_saturation[index],
                            -100.0..=100.0,
                            0,
                            1.0,
                            None,
                            touch_safe,
                        );
                        changed |= adjustment_slider(
                            ui,
                            "Luminance",
                            &mut exposure.hsl_luminance[index],
                            -100.0..=100.0,
                            0,
                            1.0,
                            None,
                            touch_safe,
                        );
                    });

                    if index + 1 < COLORS.len() {
                        ui.separator();
                    }
                }
            });
        changed
    }

    fn show_raw(ui: &mut Ui, exposure: &mut ExposureParams, touch_safe: bool) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Raw")
            .default_open(false)
            .show(ui, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Raw Black Point",
                    &mut exposure.black_point,
                    -0.25..=0.25,
                    3,
                    0.01,
                    None,
                    touch_safe,
                );
                let previous_mode = exposure.demosaic_mode;
                egui::ComboBox::from_label("Demosaic")
                    .selected_text(exposure.demosaic_mode.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut exposure.demosaic_mode,
                            DemosaicMode::Reference,
                            DemosaicMode::Reference.label(),
                        );
                        ui.selectable_value(
                            &mut exposure.demosaic_mode,
                            DemosaicMode::FrequencyDomainChroma,
                            DemosaicMode::FrequencyDomainChroma.label(),
                        );
                        ui.selectable_value(
                            &mut exposure.demosaic_mode,
                            DemosaicMode::Dual,
                            DemosaicMode::Dual.label(),
                        );
                    });
                changed |= previous_mode != exposure.demosaic_mode;

                changed |= adjustment_slider(
                    ui,
                    "Chroma Denoise",
                    &mut exposure.chroma_denoise,
                    0.0..=1.0,
                    2,
                    0.01,
                    None,
                    touch_safe,
                );
                if exposure.demosaic_mode == DemosaicMode::FrequencyDomainChroma {
                    changed |= adjustment_slider(
                        ui,
                        "Frequency Chroma",
                        &mut exposure.frequency_chroma,
                        0.0..=1.0,
                        2,
                        0.01,
                        None,
                        touch_safe,
                    );
                }
                if exposure.demosaic_mode == DemosaicMode::Dual {
                    changed |= adjustment_slider(
                        ui,
                        "Dual Detail Threshold",
                        &mut exposure.dual_threshold,
                        0.0..=100.0,
                        1,
                        1.0,
                        None,
                        touch_safe,
                    );
                }
                changed |= adjustment_slider(
                    ui,
                    "Red CA",
                    &mut exposure.ca_red,
                    -2.0..=2.0,
                    2,
                    0.01,
                    None,
                    touch_safe,
                );
                changed |= adjustment_slider(
                    ui,
                    "Blue CA",
                    &mut exposure.ca_blue,
                    -2.0..=2.0,
                    2,
                    0.01,
                    None,
                    touch_safe,
                );
            });
        changed
    }
}
