use super::*;

impl CalibRawApp {
    pub(in crate::app) fn show_subject_dialogs(&mut self, ctx: &egui::Context) {
        if self.ai.subject_consent_open {
            let model = self.ai.birefnet_quality.model();
            let model_download_needed = !crate::ai_masks::birefnet_model_is_verified(
                self.ai.birefnet_quality,
                &self.birefnet_model_path(),
            );
            let runtime_download_needed = self.ai.runtime_download_consent_pending;
            let title = match (model_download_needed, runtime_download_needed) {
                (true, true) => "Download subject model and ONNX Runtime?",
                (true, false) => "Download subject-selection model?",
                (false, true) => "Download ONNX Runtime?",
                (false, false) => "Prepare subject selection?",
            };
            crate::ui::responsive_popup(
                egui::Window::new(title),
                ctx,
                520.0,
            )
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    if model_download_needed {
                        ui.strong("Subject-selection model");
                        ui.label(format!(
                            "{} quality uses {} with its native {} x {} input tensor.",
                            self.ai.birefnet_quality.label(),
                            model.checkpoint,
                            model.input_height,
                            model.input_width
                        ));
                        ui.label(model.explanation);
                        ui.label(format!(
                            "Download: about {:.0} MB. Model license: MIT.",
                            model.bytes as f64 / 1_000_000.0
                        ));
                    }
                    #[cfg(not(target_os = "android"))]
                    if runtime_download_needed {
                        Self::show_automatic_onnx_runtime_download_details(ui);
                    }
                    if model_download_needed && runtime_download_needed {
                        ui.separator();
                        ui.label("CalibRaw downloads and verifies the model first, followed by ONNX Runtime. Both are cached locally.");
                    }
                    ui.label("Subject masks use BiRefNet's calibrated soft selection directly. Not Subject is the exact inverse of the subject alpha.");
                    ui.label("Inference is local. No photograph is uploaded.");
                    ui.label("When you continue, your device connects directly to CalibRaw Artifacts on Hugging Face. Hugging Face receives connection data such as your IP address and request time under its privacy policy. CalibRaw sends no account identifier or telemetry.");
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to(
                            "Hugging Face privacy policy",
                            "https://huggingface.co/privacy",
                        );
                        if model_download_needed {
                            ui.separator();
                            ui.hyperlink_to(
                                "MIT model license",
                                "https://github.com/ZhengPeng7/BiRefNet/blob/main/LICENSE",
                            );
                        }
                    });
                    #[cfg(not(target_os = "android"))]
                    if self.ai.runtime_mode == OnnxRuntimeMode::Manual
                        && self.ai.runtime_path.is_none()
                    {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Manual runtime mode needs a trusted local ONNX Runtime library. Select one in Settings or switch to Automatic.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Consent, download and continue").clicked()
                            && self.ai_runtime_ready()
                        {
                            self.ai.runtime_download_consent_pending = false;
                            self.ai.subject_consent_open = false;
                            self.start_subject_worker(
                                self.birefnet_model_path(),
                                model_download_needed,
                            );
                        }
                        if ui.button("Cancel").clicked() {
                            self.ai.runtime_download_consent_pending = false;
                            self.ai.subject_consent_open = false;
                            if self.ai.mask_update_active {
                                self.cancel_ai_mask_update();
                            }
                        }
                    });
                });
        }

        if self.ai.object_consent_open {
            let (encoder, decoder) = self.sam21_model_paths();
            let model_download_needed =
                !crate::ai_masks::object_models_are_verified(&encoder, &decoder);
            let runtime_download_needed = self.ai.runtime_download_consent_pending;
            let title = match (model_download_needed, runtime_download_needed) {
                (true, true) => "Download object model and ONNX Runtime?",
                (true, false) => "Download object-selection model?",
                (false, true) => "Download ONNX Runtime?",
                (false, false) => "Prepare object selection?",
            };
            crate::ui::responsive_popup(
                egui::Window::new(title),
                ctx,
                520.0,
            )
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Object masks use SAM 2.1 Hiera Tiny with local edge-aware cleanup.");
                    if model_download_needed {
                        ui.strong("Object-selection model");
                        ui.label(format!(
                            "SAM 2.1 Hiera Tiny encoder and decoder: about {:.0} MB download. Model license: Apache-2.0.",
                            SAM21_MODEL_BYTES_ESTIMATE as f64 / 1_000_000.0
                        ));
                    }
                    #[cfg(not(target_os = "android"))]
                    if runtime_download_needed {
                        Self::show_automatic_onnx_runtime_download_details(ui);
                    }
                    if model_download_needed && runtime_download_needed {
                        ui.separator();
                        ui.label("CalibRaw downloads and verifies the model files first, followed by ONNX Runtime. Both are cached locally.");
                    }
                    ui.label("Inference is local. No photograph or prompt stroke is uploaded.");
                    ui.label("When you continue, your device connects directly to Hugging Face. Hugging Face receives connection data such as your IP address and request time under its own privacy policy. CalibRaw sends no account identifier or telemetry.");
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to(
                            "Hugging Face privacy policy",
                            "https://huggingface.co/privacy",
                        );
                        if model_download_needed {
                            ui.separator();
                            ui.hyperlink_to(
                                "Apache-2.0 model license",
                                "https://github.com/facebookresearch/sam2/blob/main/LICENSE",
                            );
                        }
                    });
                    #[cfg(not(target_os = "android"))]
                    if self.ai.runtime_mode == OnnxRuntimeMode::Manual
                        && self.ai.runtime_path.is_none()
                    {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Manual runtime mode needs a trusted local ONNX Runtime library. Select one in Settings or switch to Automatic.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Consent, download and continue").clicked()
                            && self.ai_runtime_ready()
                        {
                            self.ai.runtime_download_consent_pending = false;
                            self.ai.object_consent_open = false;
                            if let Some((mask_index, component_index)) = self.ai.object_pending_target.take() {
                                let (encoder, decoder) = self.sam21_model_paths();
                                self.start_object_worker(mask_index, component_index, encoder, decoder, model_download_needed);
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.ai.runtime_download_consent_pending = false;
                            self.ai.object_consent_open = false;
                            self.ai.object_pending_target = None;
                            if self.ai.mask_update_active {
                                self.cancel_ai_mask_update();
                            }
                        }
                    });
                });
        }

        if let Some(message) = self.ai.object_error_dialog.clone() {
            let mut close = false;
            crate::ui::responsive_popup(egui::Window::new("AI mask failed"), ctx, 420.0)
                .collapsible(false)
                .resizable(true)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label(message);
                    ui.add_space(8.0);
                    if ui.button("Close").clicked() {
                        close = true;
                    }
                });
            if close {
                self.ai.object_error_dialog = None;
            }
        }
    }
}
