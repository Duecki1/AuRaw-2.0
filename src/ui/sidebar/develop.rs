impl Sidebar {
    fn show_optics(ui: &mut Ui, app: &mut AurawApp, foldable: bool) -> bool {
        let mut rebuild = false;
        let capture = app.original_raw.as_ref().map(|raw| {
            let lens = match (raw.lens_make.trim(), raw.lens_model.trim()) {
                ("", "") => "Not reported".to_owned(),
                ("", model) => model.to_owned(),
                (maker, "") => maker.to_owned(),
                (maker, model) => format!("{maker} {model}"),
            };
            let focal = (raw.focal_length > 0.0).then(|| format!("{:.1} mm", raw.focal_length));
            let aperture = (raw.aperture > 0.0).then(|| format!("f/{:.1}", raw.aperture));
            (lens, focal, aperture)
        });

        Self::adjustment_section(ui, "Lens Corrections", false, foldable, |ui| {
            ui.label(
                egui::RichText::new("Lensfun profile correction for distortion, chromatic aberration, and vignetting")
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
            );

            let state = &mut app.lens_correction;
            let has_selection = state.selected_lens().is_some();
            let enabled_response = ui.add_enabled(
                state.catalog.available && has_selection,
                egui::Checkbox::new(&mut state.enabled, "Enabled"),
            );
            if enabled_response.changed() {
                rebuild = true;
            }
            if !state.catalog.available {
                state.enabled = false;
                state.applied = false;
            }

            ui.add_space(2.0);
            egui::Grid::new("lens-correction-capture-metadata")
                .num_columns(2)
                .spacing(egui::vec2(10.0, 3.0))
                .show(ui, |ui| {
                    ui.label("Camera");
                    ui.label(if state.catalog.camera_label.is_empty() {
                        "Not matched"
                    } else {
                        state.catalog.camera_label.as_str()
                    });
                    ui.end_row();
                    if let Some((lens, focal, aperture)) = &capture {
                        ui.label("RAW lens");
                        ui.label(lens);
                        ui.end_row();
                        if let Some(focal) = focal {
                            ui.label("Focal length");
                            ui.label(focal);
                            ui.end_row();
                        }
                        if let Some(aperture) = aperture {
                            ui.label("Aperture");
                            ui.label(aperture);
                            ui.end_row();
                        }
                    }
                });

            ui.add_space(4.0);
            let makers = state.makers();
            let previous_maker = state.selected_maker.clone();
            ui.add_enabled_ui(state.catalog.available && !makers.is_empty(), |ui| {
                ui.label("Brand");
                egui::ComboBox::from_id_salt("lens-correction-brand")
                    .selected_text(if state.selected_maker.is_empty() {
                        if state.selected_model.is_empty() {
                            "Select a brand"
                        } else {
                            "Unknown"
                        }
                    } else {
                        state.selected_maker.as_str()
                    })
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for maker in &makers {
                            ui.selectable_value(
                                &mut state.selected_maker,
                                maker.clone(),
                                if maker.is_empty() { "Unknown" } else { maker },
                            );
                        }
                    });
            });
            let mut selection_changed = state.selected_maker != previous_maker;
            if selection_changed {
                let first_model = state
                    .models_for_maker(&state.selected_maker)
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                state.selected_model = first_model;
            }

            let models = state.models_for_maker(&state.selected_maker);
            let previous_model = state.selected_model.clone();
            ui.add_enabled_ui(state.catalog.available && !models.is_empty(), |ui| {
                ui.label("Lens");
                egui::ComboBox::from_id_salt("lens-correction-model")
                    .selected_text(if state.selected_model.is_empty() {
                        "Select a lens"
                    } else {
                        state.selected_model.as_str()
                    })
                    .width(ui.available_width())
                    .show_ui(ui, |ui| {
                        for model in &models {
                            ui.selectable_value(&mut state.selected_model, model.clone(), model);
                        }
                    });
            });
            selection_changed |= state.selected_model != previous_model;
            if selection_changed {
                state.applied = false;
                if let Some(selection) = state.selected_lens() {
                    state.catalog.status = if state.enabled {
                        format!("Applying {}…", selection.label())
                    } else {
                        format!(
                            "Selected {}. Enable correction to apply it.",
                            selection.label()
                        )
                    };
                }
                if state.enabled {
                    rebuild = true;
                }
            }

            ui.add_space(4.0);
            let status_color = if state.applied {
                ui.visuals().selection.bg_fill
            } else {
                ui.visuals().weak_text_color()
            };
            let status = if state.catalog.status.is_empty() {
                "Open a RAW file to inspect available lens profiles."
            } else {
                state.catalog.status.as_str()
            };
            ui.label(egui::RichText::new(status).size(11.5).color(status_color));
        });
        rebuild
    }

    fn show_basic(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Light", true, foldable, |ui| {
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
                Some("Moves the display black/toe endpoint while preserving sensor black calibration."),
            );
        });
        changed
    }

    fn show_tone_curve(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        selected_tab: &mut ToneCurveTab,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Tone Curve", true, foldable, |ui| {
            ui.horizontal(|ui| {
                for (tab, label, color) in [
                    (ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE),
                    (ToneCurveTab::Red, "R", egui::Color32::from_rgb(238, 84, 84)),
                    (
                        ToneCurveTab::Green,
                        "G",
                        egui::Color32::from_rgb(92, 210, 116),
                    ),
                    (
                        ToneCurveTab::Blue,
                        "B",
                        egui::Color32::from_rgb(88, 150, 245),
                    ),
                ] {
                    let text = egui::RichText::new(label).color(color);
                    ui.selectable_value(selected_tab, tab, text);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if crate::ui::icons::phosphor_icon_button(
                        ui,
                        egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                        egui::vec2(28.0, 22.0),
                        "Reset the selected tone curve",
                    )
                    .clicked()
                    {
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

    fn show_color(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        as_shot_temperature: Option<f32>,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Color", true, foldable, |ui| {
            if let Some(base_kelvin) = as_shot_temperature {
                let mut kelvin = crate::pipeline::temperature_kelvin_from_offset(
                    base_kelvin,
                    exposure.temperature,
                );
                let kelvin_changed = ui
                    // The slider's double-click reset is cached by widget id.
                    // Include this image's neutral so loading another camera
                    // white balance also updates the reset value.
                    .push_id(base_kelvin.to_bits(), |ui| {
                        adjustment_slider(
                            ui,
                            "Temperature (K)",
                            &mut kelvin,
                            crate::pipeline::MIN_TEMPERATURE_KELVIN
                                ..=crate::pipeline::MAX_TEMPERATURE_KELVIN,
                            0,
                            10.0,
                            Some("Scene illuminant color temperature in Kelvin; the as-shot camera white balance is the reset value."),
                        )
                    })
                    .inner;
                if kelvin_changed {
                    exposure.temperature =
                        crate::pipeline::temperature_offset_from_kelvin(base_kelvin, kelvin);
                    changed = true;
                }
            } else {
                ui.label("Temperature");
                ui.label(
                    egui::RichText::new(
                        "Unavailable: this image has no usable white-balance metadata",
                    )
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
                );
            }
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

    fn show_color_grading(
        ui: &mut Ui,
        grading: &mut crate::pipeline::ColorGrading,
        selected_tab: &mut ColorGradeTab,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        let mut contents = |ui: &mut Ui| {
            ui.label(
                egui::RichText::new("Perceptual four-way grading in scene-linear Rec.2020")
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
            );
            changed |= color_grading_editor(ui, grading, selected_tab);
        };
        if foldable {
            egui::CollapsingHeader::new("Color Grading")
                .default_open(false)
                .show(ui, contents);
        } else {
            contents(ui);
        }
        changed
    }

    fn show_detail(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Detail", false, foldable, |ui| {
            ui.label(
                egui::RichText::new("Sensor-profiled noise reduction")
                    .strong()
                    .size(11.5),
            );
            ui.label(
                egui::RichText::new(
                    "Signal-dependent multiscale filtering before tone, texture, and sharpening",
                )
                .size(10.5)
                .color(ui.visuals().weak_text_color()),
            );
            changed |= adjustment_slider(
                ui,
                "Luminance",
                &mut exposure.luminance_denoise,
                0.0..=100.0,
                0,
                1.0,
                Some("Reduces shot/read noise using the RAW's estimated a·signal+b sensor model. Higher values can smooth fine texture."),
            );
            let mut color_percent = exposure.chroma_denoise.clamp(0.0, 1.0) * 100.0;
            if adjustment_slider(
                ui,
                "Color",
                &mut color_percent,
                0.0..=100.0,
                0,
                1.0,
                Some("Reduces color speckling while keeping luminance structure comparatively intact."),
            ) {
                exposure.chroma_denoise = color_percent / 100.0;
                changed = true;
            }
            changed |= adjustment_slider(
                ui,
                "Denoise Detail",
                &mut exposure.denoise_detail,
                0.0..=100.0,
                0,
                1.0,
                Some("Higher values protect edges and microtexture more strongly; lower values permit smoother denoising."),
            );
            let previous_quality = exposure.denoise_quality;
            egui::ComboBox::from_label("Denoise Quality")
                .selected_text(exposure.denoise_quality.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut exposure.denoise_quality,
                        DenoiseQuality::Fast,
                        DenoiseQuality::Fast.label(),
                    );
                    ui.selectable_value(
                        &mut exposure.denoise_quality,
                        DenoiseQuality::Balanced,
                        DenoiseQuality::Balanced.label(),
                    );
                    ui.selectable_value(
                        &mut exposure.denoise_quality,
                        DenoiseQuality::High,
                        DenoiseQuality::High.label(),
                    );
                });
            ui.label(
                egui::RichText::new("Fast: 8 taps · Balanced: 16 taps · High: 24 taps")
                    .size(10.0)
                    .color(ui.visuals().weak_text_color()),
            );
            changed |= previous_quality != exposure.denoise_quality;
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Edge-aware capture sharpening for fine RAW detail")
                    .strong()
                    .size(11.5),
            );
            changed |= adjustment_slider(
                ui,
                "Amount",
                &mut exposure.sharpen_amount,
                0.0..=150.0,
                0,
                1.0,
                Some("Controls overall capture sharpening strength. Zero is an exact no-op."),
            );
            changed |= adjustment_slider(
                ui,
                "Radius",
                &mut exposure.sharpen_radius,
                0.5..=3.0,
                2,
                0.05,
                Some("Controls the edge width being sharpened. Smaller values favor fine detail; larger values strengthen broader edges."),
            );
            changed |= adjustment_slider(
                ui,
                "Detail",
                &mut exposure.sharpen_detail,
                0.0..=100.0,
                0,
                1.0,
                Some("Raises the contribution of the finest texture and lowers fine-detail suppression."),
            );
            changed |= adjustment_slider(
                ui,
                "Masking",
                &mut exposure.sharpen_masking,
                0.0..=100.0,
                0,
                1.0,
                Some("Restricts sharpening to stronger luminance edges as the value increases, protecting flat areas and noise."),
            );
        });
        changed
    }

    fn show_presence(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        expert_mode: bool,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Effects", false, foldable, |ui| {
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
                    Some(
                        "Softens and blooms bright light sources without lifting the entire image.",
                    ),
                );
                if expert_mode {
                    ui.push_id("advanced-glow", |ui| {
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
                    });
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

    fn show_hsl(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        const COLORS: [&str; 8] = [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ];

        let mut changed = false;
        Self::adjustment_section(ui, "Color Mixer", false, foldable, |ui| {
            for (index, color) in COLORS.iter().enumerate() {
                ui.push_id(index, |ui| {
                    ui.strong(*color);
                    changed |= adjustment_slider(
                        ui,
                        "Hue",
                        &mut exposure.hsl_hue[index],
                        -HSL_HUE_LIMIT..=HSL_HUE_LIMIT,
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

    fn show_rendering(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Advanced Rendering", false, foldable, |ui| {
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

    fn show_raw(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Raw", false, foldable, |ui| {
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
