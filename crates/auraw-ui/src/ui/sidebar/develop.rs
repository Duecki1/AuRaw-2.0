impl Sidebar {
    fn show_optics(ui: &mut Ui, app: &mut AurawApp, foldable: bool) -> bool {
        let mut rebuild = false;
        let capture = app.develop.original_raw.as_ref().map(|raw| {
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
            let lens_correction_busy = app.lens_correction_busy();
            let state = &mut app.develop.lens_correction;
            let has_selection = state.selected_lens().is_some();
            let enabled_response = ui.add_enabled(
                state.catalog.available && has_selection && !lens_correction_busy,
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
            ui.add_enabled_ui(
                state.catalog.available && !makers.is_empty() && !lens_correction_busy,
                |ui| {
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
                        .width(ui.available_width().clamp(1.0, 240.0))
                        .truncate()
                        .show_ui(ui, |ui| {
                            for maker in &makers {
                                ui.selectable_value(
                                    &mut state.selected_maker,
                                    maker.clone(),
                                    if maker.is_empty() { "Unknown" } else { maker },
                                );
                            }
                        });
                },
            );
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
            ui.add_enabled_ui(
                state.catalog.available && !models.is_empty() && !lens_correction_busy,
                |ui| {
                    ui.label("Lens");
                    egui::ComboBox::from_id_salt("lens-correction-model")
                        .selected_text(if state.selected_model.is_empty() {
                            "Select a lens"
                        } else {
                            state.selected_model.as_str()
                        })
                        .width(ui.available_width().clamp(1.0, 240.0))
                        .truncate()
                        .show_ui(ui, |ui| {
                            for model in &models {
                                ui.selectable_value(
                                    &mut state.selected_model,
                                    model.clone(),
                                    model,
                                );
                            }
                        });
                },
            );
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
        });
        rebuild
    }

    fn show_basic(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Light", true, foldable, |ui| {
            changed |= gradient_adjustment_slider(
                ui,
                "Exposure",
                &mut exposure.exposure,
                -5.0..=5.0,
                2,
                0.05,
                Some("Overall scene-linear brightness in exposure stops."),
                SliderGradient::Brightness,
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Contrast",
                &mut exposure.contrast,
                -100.0..=100.0,
                0,
                1.0,
                Some("Maps -100%..+100% to darktable's normal sigmoid contrast range, 0.7..3.0 around its 1.5 default."),
                SliderGradient::Brightness,
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Highlights",
                &mut exposure.highlights,
                -100.0..=100.0,
                0,
                1.0,
                Some("Recovers or brightens the upper tonal range without hard clipping."),
                SliderGradient::Brightness,
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Shadows",
                &mut exposure.shadows,
                -100.0..=100.0,
                0,
                1.0,
                Some("Opens or deepens the lower tonal range."),
                SliderGradient::Brightness,
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Whites",
                &mut exposure.whites,
                -100.0..=100.0,
                0,
                1.0,
                Some("Moves the bright endpoint and specular range."),
                SliderGradient::Brightness,
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Blacks",
                &mut exposure.blacks,
                -100.0..=100.0,
                0,
                1.0,
                Some("Moves the display black/toe endpoint while preserving sensor black calibration."),
                SliderGradient::Brightness,
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
        Self::adjustment_section(ui, "Tone Curve", false, foldable, |ui| {
            changed |= tone_curve_channel_editor(
                ui,
                ToneCurveChannels {
                    rgb: &mut exposure.tone_curve,
                    red: &mut exposure.tone_curve_red,
                    green: &mut exposure.tone_curve_green,
                    blue: &mut exposure.tone_curve_blue,
                },
                selected_tab,
                4.0,
            );
        });
        changed
    }

    fn show_color(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        raw: Option<&LoadedRaw>,
        white_balance_picker_active: &mut bool,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Color", false, foldable, |ui| {
            if let Some(raw) = raw.filter(|raw| raw.as_shot_white_balance().is_some()) {
                let presets = raw.camera_white_balance_presets();
                let matches_current = |candidate: (f32, f32)| {
                    (candidate.0 - exposure.temperature).abs() < 0.01
                        && (candidate.1 - exposure.tint).abs() < 0.01
                };
                let selection = if *white_balance_picker_active {
                    "from image area".to_owned()
                } else if exposure.temperature.abs() < 1e-5 && exposure.tint.abs() < 1e-5 {
                    "as shot".to_owned()
                } else if raw
                    .white_balance_offsets_from_temperature_tint(6504.0, 1.0)
                    .is_some_and(&matches_current)
                {
                    "camera reference (D65)".to_owned()
                } else if let Some(preset) = presets.iter().find(|preset| {
                    raw.white_balance_offsets_from_coefficients(preset.coefficients)
                        .is_some_and(&matches_current)
                }) {
                    preset.name.clone()
                } else if let Some(temperature) = [2500.0, 3200.0, 4500.0, 6000.0, 8500.0]
                    .into_iter()
                    .find(|temperature| {
                        raw.white_balance_offsets_from_temperature_tint(*temperature, 1.0)
                            .is_some_and(&matches_current)
                    })
                {
                    format!("{temperature:.0}K")
                } else {
                    "user modified".to_owned()
                };
                ui.horizontal(|ui| {
                    let picker_width = crate::ui::theme::TOOLBAR_ICON_EDGE;
                    let combo_width = (ui.available_width()
                        - picker_width
                        - ui.spacing().item_spacing.x)
                        .clamp(1.0, 240.0);
                    egui::ComboBox::from_id_salt("global-white-balance-preset")
                        .selected_text(selection)
                        .width(combo_width)
                        .truncate()
                        .show_ui(ui, |ui| {
                            if ui.selectable_label(false, "as shot").clicked() {
                                exposure.temperature = 0.0;
                                exposure.tint = 0.0;
                                *white_balance_picker_active = false;
                                changed = true;
                            }
                            if ui.selectable_label(false, "from image area").clicked() {
                                *white_balance_picker_active = true;
                            }
                            ui.label(
                                egui::RichText::new("reference")
                                    .strong()
                                    .color(ui.visuals().weak_text_color()),
                            );
                            if ui
                                .selectable_label(false, "camera reference (D65)")
                                .clicked()
                            {
                                if let Some((temperature, tint)) =
                                    raw.white_balance_offsets_from_temperature_tint(6504.0, 1.0)
                                {
                                    exposure.temperature = temperature;
                                    exposure.tint = tint;
                                    *white_balance_picker_active = false;
                                    changed = true;
                                }
                            }
                            if !presets.is_empty() {
                                ui.separator();
                                ui.label(
                                    egui::RichText::new(format!(
                                        "{} {}",
                                        raw.camera_make, raw.camera_model
                                    ))
                                    .strong(),
                                );
                                for preset in &presets {
                                    if ui.selectable_label(false, &preset.name).clicked() {
                                        if let Some((temperature, tint)) = raw
                                            .white_balance_offsets_from_coefficients(
                                                preset.coefficients,
                                            )
                                        {
                                            exposure.temperature = temperature;
                                            exposure.tint = tint;
                                            *white_balance_picker_active = false;
                                            changed = true;
                                        }
                                    }
                                }
                            }
                            ui.separator();
                            ui.label(
                                egui::RichText::new("fixed temperature")
                                    .strong()
                                    .color(ui.visuals().weak_text_color()),
                            );
                            for temperature in [2500.0, 3200.0, 4500.0, 6000.0, 8500.0] {
                                if ui
                                    .selectable_label(false, format!("{temperature:.0}K"))
                                    .clicked()
                                {
                                    if let Some((temperature, tint)) = raw
                                        .white_balance_offsets_from_temperature_tint(
                                            temperature,
                                            1.0,
                                        )
                                    {
                                        exposure.temperature = temperature;
                                        exposure.tint = tint;
                                        *white_balance_picker_active = false;
                                        changed = true;
                                    }
                                }
                            }
                        });
                    let picker = crate::ui::icons::phosphor_icon_toggle_button(
                        ui,
                        egui_phosphor::regular::EYEDROPPER,
                        *white_balance_picker_active,
                        egui::vec2(picker_width, crate::ui::theme::CONTROL_HEIGHT),
                        "Pick a neutral gray or white area in the image",
                    );
                    if picker.clicked() {
                        *white_balance_picker_active = !*white_balance_picker_active;
                    }
                });
                if *white_balance_picker_active {
                    ui.label(
                        egui::RichText::new("Drag over a neutral area in the image")
                            .size(11.5)
                            .color(ui.visuals().selection.bg_fill),
                    );
                }

                let (mut kelvin, mut tint) = raw
                    .white_balance_temperature_tint(exposure.temperature, exposure.tint)
                    .expect("white-balance model was checked above");
                let base_kelvin = raw.as_shot_temperature_kelvin().unwrap_or(kelvin);
                let kelvin_changed = ui
                    .push_id(base_kelvin.to_bits(), |ui| {
                        gradient_adjustment_slider(
                            ui,
                            "Temperature (K)",
                            &mut kelvin,
                            crate::pipeline::MIN_TEMPERATURE_KELVIN
                                ..=crate::pipeline::MAX_TEMPERATURE_KELVIN,
                            0,
                            10.0,
                            Some("Scene illuminant color temperature in Kelvin; the as-shot camera white balance is the reset value."),
                            SliderGradient::Temperature,
                        )
                    })
                    .inner;
                if kelvin_changed {
                    exposure.temperature =
                        crate::pipeline::temperature_offset_from_kelvin(base_kelvin, kelvin);
                    *white_balance_picker_active = false;
                    changed = true;
                }
                let base_tint = raw.as_shot_white_balance().map_or(tint, |value| value.1);
                let tint_neutral_fraction = ((1.0 - MIN_WHITE_BALANCE_TINT)
                    / (MAX_WHITE_BALANCE_TINT - MIN_WHITE_BALANCE_TINT))
                    .clamp(0.0, 1.0);
                let tint_changed = ui
                    .push_id(base_tint.to_bits(), |ui| {
                        gradient_adjustment_slider(
                            ui,
                            "Tint",
                            &mut tint,
                            MIN_WHITE_BALANCE_TINT..=MAX_WHITE_BALANCE_TINT,
                            3,
                            0.005,
                            Some("darktable-compatible absolute camera tint: values below 1 are magenta, values above 1 are green; the as-shot value is the reset value."),
                            SliderGradient::CameraTint {
                                neutral_fraction: tint_neutral_fraction,
                            },
                        )
                    })
                    .inner;
                if tint_changed {
                    exposure.tint = crate::pipeline::white_balance_tint_offset(base_tint, tint);
                    *white_balance_picker_active = false;
                    changed = true;
                }
            } else {
                *white_balance_picker_active = false;
                ui.label("White balance");
                ui.label(
                    egui::RichText::new(
                        "Unavailable: this image has no usable white-balance metadata",
                    )
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
                );
            }
            changed |= hue_adjustment_slider(
                ui,
                &mut exposure.hue,
                Some("Rotates every color around the perceptual color wheel while preserving lightness and chroma."),
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Vibrance",
                &mut exposure.vibrance,
                -100.0..=100.0,
                0,
                1.0,
                Some("Perceptual colorfulness with protection for saturated colors and skin hues."),
                SliderGradient::Colorfulness,
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Saturation",
                &mut exposure.saturation,
                -100.0..=100.0,
                0,
                1.0,
                Some("Uniform perceptual chroma scaling."),
                SliderGradient::Colorfulness,
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
        let contents = |ui: &mut Ui| {
            changed |= color_grading_editor(ui, grading, selected_tab);
        };
        Self::adjustment_section(ui, "Color Grading", false, foldable, contents);
        changed
    }

    fn show_detail(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        foldable: bool,
    ) -> (bool, Option<bool>) {
        let mut changed = false;
        let mut ai_request = None;
        Self::adjustment_section(ui, "Detail", false, foldable, |ui| {
            let mut ai_enabled = exposure.ai_denoise_enabled;
            let ai_response = ui.checkbox(&mut ai_enabled, "AI Denoise — RawNIND UtNet2");
            if ai_response.changed() {
                ai_request = Some(ai_enabled);
            }
            ai_response.on_hover_text(
                "Runs the pinned darktable-ai RawNIND model locally. Bayer uses joint denoise/demosaic; X-Trans uses the linear Rec.2020 variant.",
            );
            ui.add_space(4.0);
            ui.separator();
            ui.add_space(4.0);
            crate::ui::theme::strong_with_help(
                ui,
                "Noise reduction",
                "Sensor-profiled noise reduction uses the RAW's estimated a·signal+b sensor model. AI Denoise replaces these manual controls while enabled.",
            );
            ui.add_enabled_ui(!exposure.ai_denoise_enabled, |ui| {
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
                crate::ui::theme::form_combo(
                    ui,
                    "Denoise quality",
                    "develop-denoise-quality",
                    exposure.denoise_quality.label(),
                    150.0,
                    |ui| {
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
                    },
                );
                changed |= previous_quality != exposure.denoise_quality;
            });
            ui.add_space(8.0);
            ui.separator();
            ui.add_space(4.0);
            crate::ui::theme::strong_with_help(
                ui,
                "Capture sharpening",
                "Edge-aware capture sharpening restores fine RAW detail while its radius, detail, and masking controls limit halos and noisy texture.",
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
        (changed, ai_request)
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
                changed |= gradient_adjustment_slider(
                    ui,
                    "Amount",
                    &mut exposure.vignette_amount,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Darkens negative values or brightens positive values toward the image edges."),
                    SliderGradient::Brightness,
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
                changed |= gradient_adjustment_slider(
                    ui,
                    "Highlights",
                    &mut exposure.vignette_highlights,
                    0.0..=100.0,
                    0,
                    1.0,
                    Some("Restores bright edge highlights when using a dark vignette."),
                    SliderGradient::Brightness,
                );
            });
        });
        changed
    }

    fn show_hsl(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        selected_color: &mut HslMixerColor,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Color Mixer", false, foldable, |ui| {
            changed |= hsl_mixer(
                ui,
                selected_color,
                &mut exposure.hsl_hue,
                &mut exposure.hsl_saturation,
                &mut exposure.hsl_luminance,
            );
        });
        changed
    }

    fn show_rendering(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Advanced Rendering", false, foldable, |ui| {
            crate::ui::theme::strong_with_help(
                ui,
                "Sigmoid view transform",
                "A darktable-compatible sigmoid view transform maps scene-referred values into the display range while controlling contrast, target white and black, and hue handling.",
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
            changed |= gradient_adjustment_slider(
                ui,
                "Target white (%)",
                &mut exposure.sigmoid.display_white_target,
                20.0..=1600.0,
                1,
                1.0,
                None,
                SliderGradient::Brightness,
            );
            changed |= gradient_adjustment_slider(
                ui,
                "Target black (%)",
                &mut exposure.sigmoid.display_black_target,
                0.0..=15.0,
                4,
                0.0001,
                None,
                SliderGradient::Brightness,
            );

            let old_method = exposure.sigmoid.color_processing;
            crate::ui::theme::form_combo(
                ui,
                "Color processing",
                "develop-sigmoid-color-processing",
                exposure.sigmoid.color_processing.label(),
                150.0,
                |ui| {
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
                },
            );
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
            crate::ui::theme::form_combo(
                ui,
                "Demosaic",
                "develop-demosaic-mode",
                exposure.demosaic_mode.label(),
                170.0,
                |ui| {
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
                },
            );
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
