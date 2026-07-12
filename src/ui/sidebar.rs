use crate::app::{AurawApp, SidebarTab, ToneCurveTab};
use crate::pipeline::{DemosaicMode, ExportResizeMode, ExposureParams, SigmoidColorProcessing};
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::components::tone_curve_editor::tone_curve_editor;
use crate::ui::layout::ScreenLayout;
use eframe::egui::{self, Ui};

pub struct Sidebar;

impl Sidebar {
    const SCROLLBAR_GUTTER: f32 = 18.0;

    pub fn show(
        ui: &mut Ui,
        app: &mut AurawApp,
        _layout: ScreenLayout,
        frame: &eframe::Frame,
    ) {
        let content_width = (ui.available_width() - Self::SCROLLBAR_GUTTER).max(220.0);
        ui.set_width(content_width);
        ui.set_max_width(content_width);
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (SidebarTab::Adjustments, "Adjustments"),
                (SidebarTab::Masks, "Masks"),
                (SidebarTab::Inpainting, "Inpainting"),
                (SidebarTab::Export, "Export"),
            ] {
                ui.selectable_value(&mut app.sidebar_tab, tab, label);
            }
        });
        ui.add_space(2.0);
        ui.separator();

        match app.sidebar_tab {
            SidebarTab::Adjustments => Self::show_adjustments(ui, app),
            SidebarTab::Masks => Self::show_placeholder(
                ui,
                "Masks",
                "Local adjustment masks will appear here in a future update.",
            ),
            SidebarTab::Inpainting => Self::show_placeholder(
                ui,
                "Inpainting",
                "Healing, object removal, and generative inpainting controls are coming later.",
            ),
            SidebarTab::Export => Self::show_export(ui, app, frame),
        }
    }

    fn show_adjustments(ui: &mut Ui, app: &mut AurawApp) {
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

        let mut changed = false;
        changed |= Self::show_basic(ui, &mut app.exposure);
        changed |= Self::show_tone_curve(ui, &mut app.exposure, &mut app.tone_curve_tab);
        changed |= Self::show_color(ui, &mut app.exposure);
        changed |= Self::show_presence(ui, &mut app.exposure, app.expert_mode);
        changed |= Self::show_hsl(ui, &mut app.exposure);
        if app.expert_mode {
            changed |= Self::show_rendering(ui, &mut app.exposure);
            changed |= Self::show_raw(ui, &mut app.exposure);
        }

        if changed {
            app.exposure.sanitize_tone_curves();
            app.mark_pipeline_dirty();
        }
    }

    fn show_placeholder(ui: &mut Ui, title: &str, message: &str) {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.heading(title);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(message)
                    .color(ui.visuals().weak_text_color()),
            );
        });
    }

    fn show_export(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        ui.heading("Export");
        ui.label(
            egui::RichText::new("PNG · sRGB · high-quality processing")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(6.0);

        let source_dimensions = app
            .loaded_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height));
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong("Image sizing");
            egui::ComboBox::from_label("Resize to fit")
                .selected_text(app.export_settings.resize_mode.label())
                .show_ui(ui, |ui| {
                    for mode in [
                        ExportResizeMode::Original,
                        ExportResizeMode::LongEdge,
                        ExportResizeMode::ShortEdge,
                        ExportResizeMode::Width,
                        ExportResizeMode::Height,
                        ExportResizeMode::Percentage,
                    ] {
                        ui.selectable_value(
                            &mut app.export_settings.resize_mode,
                            mode,
                            mode.label(),
                        );
                    }
                });

            match app.export_settings.resize_mode {
                ExportResizeMode::Original => {
                    ui.label("Exports the complete processed image.");
                }
                ExportResizeMode::Percentage => {
                    ui.horizontal(|ui| {
                        ui.label("Scale");
                        ui.add(
                            egui::DragValue::new(&mut app.export_settings.percentage)
                                .range(1.0..=400.0)
                                .speed(1.0)
                                .suffix("%"),
                        );
                    });
                }
                mode => {
                    ui.horizontal(|ui| {
                        ui.label(mode.label());
                        ui.add(
                            egui::DragValue::new(&mut app.export_settings.edge_or_dimension)
                                .range(64..=65_535)
                                .speed(10.0)
                                .suffix(" px"),
                        );
                    });
                }
            }

            if app.export_settings.resize_mode != ExportResizeMode::Original {
                ui.checkbox(&mut app.export_settings.allow_upscale, "Allow upscaling")
                    .on_hover_text("Disabled by default to avoid enlarging beyond the source dimensions.");
            }

            if let Some((width, height)) = source_dimensions {
                let (output_width, output_height) =
                    app.export_settings.output_dimensions(width, height);
                ui.label(format!(
                    "Source: {width}×{height}  →  Export: {output_width}×{output_height}"
                ));
            } else {
                ui.label("Open a RAW file to calculate export dimensions.");
            }
        });

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong("Metadata");
            ui.checkbox(&mut app.export_settings.keep_metadata, "Keep metadata")
                .on_hover_text(
                    "Embeds available camera, source-file, original-size, software, and orientation metadata in the PNG.",
                );
        });

        ui.add_space(10.0);
        let button = egui::Button::new("Export PNG…").min_size(egui::vec2(ui.available_width(), 30.0));
        if ui.add_enabled(app.can_export(), button).clicked() {
            app.export_png(frame);
        }
        if !app.can_export() {
            ui.label(
                egui::RichText::new("Export becomes available after a RAW image has finished loading.")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
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

    fn show_tone_curve(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        selected_tab: &mut ToneCurveTab,
    ) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Tone Curve")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label, color) in [
                        (ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE),
                        (ToneCurveTab::Red, "R", egui::Color32::from_rgb(238, 84, 84)),
                        (ToneCurveTab::Green, "G", egui::Color32::from_rgb(92, 210, 116)),
                        (ToneCurveTab::Blue, "B", egui::Color32::from_rgb(88, 150, 245)),
                    ] {
                        let text = egui::RichText::new(label).color(color);
                        ui.selectable_value(selected_tab, tab, text);
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.small_button("Reset curve").clicked() {
                            match selected_tab {
                                ToneCurveTab::Rgb => exposure.tone_curve.reset(),
                                ToneCurveTab::Red => exposure.tone_curve_red.reset(),
                                ToneCurveTab::Green => exposure.tone_curve_green.reset(),
                                ToneCurveTab::Blue => exposure.tone_curve_blue.reset(),
                            }
                            changed = true;
                        }
                    });
                });

                let (curve, color, description) = match selected_tab {
                    ToneCurveTab::Rgb => (
                        &mut exposure.tone_curve,
                        egui::Color32::WHITE,
                        "Composite luminance curve",
                    ),
                    ToneCurveTab::Red => (
                        &mut exposure.tone_curve_red,
                        egui::Color32::from_rgb(238, 84, 84),
                        "Red channel curve",
                    ),
                    ToneCurveTab::Green => (
                        &mut exposure.tone_curve_green,
                        egui::Color32::from_rgb(92, 210, 116),
                        "Green channel curve",
                    ),
                    ToneCurveTab::Blue => (
                        &mut exposure.tone_curve_blue,
                        egui::Color32::from_rgb(88, 150, 245),
                        "Blue channel curve",
                    ),
                };
                ui.label(
                    egui::RichText::new(description)
                        .size(11.5)
                        .color(ui.visuals().weak_text_color()),
                );
                changed |= tone_curve_editor(ui, curve, color);
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

    fn show_presence(ui: &mut Ui, exposure: &mut ExposureParams, expert_mode: bool) -> bool {
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
                    Some("Enhances or softens fine surface detail without changing overall exposure."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Clarity",
                    &mut exposure.clarity,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Changes edge-aware midtone local contrast while protecting highlights and deep shadows."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Dehaze",
                    &mut exposure.dehaze,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Removes or adds atmospheric veil while preserving color relationships."),
                );

                ui.separator();
                ui.push_id("glow", |ui| {
                    ui.strong("Glow");
                    changed |= adjustment_slider(
                        ui,
                        "Amount",
                        &mut exposure.glow_amount,
                        0.0..=100.0,
                        0,
                        1.0,
                        Some("Softens and blooms bright light sources without lifting the entire image."),
                    );
                    if expert_mode {
                        changed |= adjustment_slider(
                            ui,
                            "Radius",
                            &mut exposure.glow_radius,
                            0.0..=100.0,
                            0,
                            1.0,
                            Some("Controls the spatial spread of the highlight bloom."),
                        );
                        changed |= adjustment_slider(
                            ui,
                            "Threshold",
                            &mut exposure.glow_threshold,
                            0.0..=100.0,
                            0,
                            1.0,
                            Some("Higher values restrict glow to brighter highlights."),
                        );
                    }
                });

                ui.separator();
                ui.push_id("vignette", |ui| {
                    ui.strong("Vignette");
                    changed |= adjustment_slider(
                        ui,
                        "Amount",
                        &mut exposure.vignette_amount,
                        -100.0..=100.0,
                        0,
                        1.0,
                        Some("Darkens negative values or brightens positive values toward the image edges."),
                    );
                    changed |= adjustment_slider(
                        ui,
                        "Midpoint",
                        &mut exposure.vignette_midpoint,
                        0.0..=100.0,
                        0,
                        1.0,
                        Some("Moves the vignette transition inward or confines it to the outermost edge."),
                    );
                    changed |= adjustment_slider(
                        ui,
                        "Roundness",
                        &mut exposure.vignette_roundness,
                        -100.0..=100.0,
                        0,
                        1.0,
                        Some("Changes the vignette shape from frame-like to circular."),
                    );
                    changed |= adjustment_slider(
                        ui,
                        "Feather",
                        &mut exposure.vignette_feather,
                        0.0..=100.0,
                        0,
                        1.0,
                        Some("Controls the softness of the vignette transition."),
                    );
                    changed |= adjustment_slider(
                        ui,
                        "Highlights",
                        &mut exposure.vignette_highlights,
                        0.0..=100.0,
                        0,
                        1.0,
                        Some("Restores bright edge highlights when using a dark vignette."),
                    );
                });
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
