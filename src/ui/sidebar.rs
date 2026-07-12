use crate::app::AurawApp;
use crate::pipeline::{DemosaicMode, ExposureParams, SigmoidColorProcessing};
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::components::tone_curve_editor::tone_curve_editor;
use crate::ui::layout::ScreenLayout;
use eframe::egui::{self, Ui};

pub struct Sidebar;

impl Sidebar {
    const SCROLLBAR_GUTTER: f32 = 18.0;

    pub fn show(ui: &mut Ui, app: &mut AurawApp, _layout: ScreenLayout) {
        let content_width = (ui.available_width() - Self::SCROLLBAR_GUTTER).max(220.0);
        ui.set_width(content_width);
        ui.set_max_width(content_width);
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        ui.horizontal(|ui| {
            ui.heading("Adjustments");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset all").clicked() {
                    app.reset_develop_adjustments();
                }
            });
        });
        ui.label(
            egui::RichText::new("Scene-referred RAW controls")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);
        ui.separator();

        // Keep the visible order identical on desktop and mobile so muscle
        // memory matches Lightroom: Light, Curve, Color, then optional tools.
        let mut changed = false;
        changed |= Self::show_basic(ui, &mut app.exposure);
        changed |= Self::show_tone_curve(ui, &mut app.exposure);
        changed |= Self::show_color(ui, &mut app.exposure);
        changed |= Self::show_presence(ui, &mut app.exposure);
        changed |= Self::show_hsl(ui, &mut app.exposure);
        changed |= Self::show_rendering(ui, &mut app.exposure);
        changed |= Self::show_raw(ui, &mut app.exposure);

        if changed {
            app.exposure.tone_curve.sanitize();
            app.mark_pipeline_dirty();
        }
    }

    fn show_basic(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Light")
            .default_open(true)
            .show(ui, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Exposure",
                    &mut exposure.exposure,
                    -5.0..=5.0,
                    2,
                    0.05,
                    Some("Overall scene-linear brightness in exposure stops."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Contrast",
                    &mut exposure.contrast,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Expands or compresses tones around photographic middle gray."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Highlights",
                    &mut exposure.highlights,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Recovers or brightens the upper tonal range without hard clipping."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Shadows",
                    &mut exposure.shadows,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Opens or deepens the lower tonal range."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Whites",
                    &mut exposure.whites,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Moves the bright endpoint and specular range."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Blacks",
                    &mut exposure.blacks,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Moves the dark endpoint while preserving sensor black calibration."),
                );
            });
        changed
    }

    fn show_tone_curve(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Tone Curve")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new("Point Curve")
                            .size(11.5)
                            .color(ui.visuals().weak_text_color()),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Reset curve").clicked() {
                            exposure.tone_curve.reset();
                            changed = true;
                        }
                    });
                });
                changed |= tone_curve_editor(ui, &mut exposure.tone_curve);
            });
        changed
    }

    fn show_color(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Color")
            .default_open(true)
            .show(ui, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Temperature",
                    &mut exposure.temperature,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Relative blue-yellow adaptation; zero preserves the camera as-shot white balance."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Tint",
                    &mut exposure.tint,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Relative green-magenta adaptation."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Vibrance",
                    &mut exposure.vibrance,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Perceptual colorfulness with protection for saturated colors and skin hues."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Saturation",
                    &mut exposure.saturation,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Uniform perceptual chroma scaling."),
                );
            });
        changed
    }

    fn show_presence(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Effects")
            .default_open(false)
            .show(ui, |ui| {
                changed |= adjustment_slider(
                    ui,
                    "Texture",
                    &mut exposure.texture,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Clarity",
                    &mut exposure.clarity,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Dehaze",
                    &mut exposure.dehaze,
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
            });
        changed
    }

    fn show_hsl(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        const COLORS: [&str; 8] = [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ];

        let mut changed = false;
        egui::CollapsingHeader::new("Color Mixer")
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
                        );
                        changed |= adjustment_slider(
                            ui,
                            "Saturation",
                            &mut exposure.hsl_saturation[index],
                            -100.0..=100.0,
                            0,
                            1.0,
                            None,
                        );
                        changed |= adjustment_slider(
                            ui,
                            "Luminance",
                            &mut exposure.hsl_luminance[index],
                            -100.0..=100.0,
                            0,
                            1.0,
                            None,
                        );
                    });

                    if index + 1 < COLORS.len() {
                        ui.separator();
                    }
                }
            });
        changed
    }

    fn show_rendering(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Advanced Rendering")
            .default_open(false)
            .show(ui, |ui| {
                ui.label(
                    egui::RichText::new("darktable sigmoid view transform")
                        .strong()
                        .size(11.5),
                );
                changed |= adjustment_slider(
                    ui,
                    "View contrast",
                    &mut exposure.sigmoid.contrast,
                    0.1..=10.0,
                    3,
                    0.01,
                    Some("Advanced darktable sigmoid slope; separate from the Lightroom-style Contrast slider."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Skew",
                    &mut exposure.sigmoid.skew,
                    -1.0..=1.0,
                    3,
                    0.01,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Target white (%)",
                    &mut exposure.sigmoid.display_white_target,
                    20.0..=1600.0,
                    1,
                    1.0,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Target black (%)",
                    &mut exposure.sigmoid.display_black_target,
                    0.0..=15.0,
                    4,
                    0.0001,
                    None,
                );

                let old_method = exposure.sigmoid.color_processing;
                egui::ComboBox::from_label("Color processing")
                    .selected_text(exposure.sigmoid.color_processing.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut exposure.sigmoid.color_processing,
                            SigmoidColorProcessing::PerChannel,
                            SigmoidColorProcessing::PerChannel.label(),
                        );
                        ui.selectable_value(
                            &mut exposure.sigmoid.color_processing,
                            SigmoidColorProcessing::RgbRatio,
                            SigmoidColorProcessing::RgbRatio.label(),
                        );
                    });
                changed |= old_method != exposure.sigmoid.color_processing;

                if exposure.sigmoid.color_processing == SigmoidColorProcessing::PerChannel {
                    changed |= adjustment_slider(
                        ui,
                        "Preserve hue (%)",
                        &mut exposure.sigmoid.hue_preservation,
                        0.0..=100.0,
                        1,
                        1.0,
                        None,
                    );
                }
            });
        changed
    }

    fn show_raw(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
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
                );
                changed |= adjustment_slider(
                    ui,
                    "Blue CA",
                    &mut exposure.ca_blue,
                    -2.0..=2.0,
                    2,
                    0.01,
                    None,
                );
            });
        changed
    }
}
