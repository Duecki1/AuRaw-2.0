use super::*;

impl CalibRawApp {
    pub(in crate::app) fn show_subject_dialogs(&mut self, ctx: &egui::Context) {
        if self.ai.subject_consent_open {
            crate::ui::responsive_popup(
                egui::Window::new("Download subject-selection model?"),
                ctx,
                520.0,
            )
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    let model = self.ai.birefnet_quality.model();
                    ui.label(format!(
                        "{} quality uses {} with its native {} x {} input tensor.",
                        self.ai.birefnet_quality.label(),
                        model.checkpoint,
                        model.input_height,
                        model.input_width
                    ));
                    ui.label(model.explanation);
                    ui.label("Subject masks use BiRefNet's calibrated soft selection directly. Not Subject is the exact inverse of the subject alpha.");
                    ui.label(format!(
                        "The first use downloads about {:.0} MB and stores the ONNX model in CalibRaw's cache.",
                        model.bytes as f64 / 1_000_000.0
                    ));
                    ui.label("Model license: BiRefNet MIT. The model is optional and used only after this download.");
                    ui.label("Inference is local. No photograph is uploaded.");
                    ui.label("When you continue, your device connects directly to GitHub for BiRefNet. GitHub receives connection data such as your IP address and request time under its privacy policy. CalibRaw sends no account identifier or telemetry.");
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to(
                            "GitHub privacy statement",
                            "https://docs.github.com/en/site-policy/privacy-policies/github-general-privacy-statement",
                        );
                        ui.separator();
                        ui.hyperlink_to(
                            "MIT model license",
                            "https://github.com/ZhengPeng7/BiRefNet/blob/main/LICENSE",
                        );
                    });
                    #[cfg(not(target_os = "android"))]
                    if self.ai.runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing. CalibRaw never downloads native runtime code.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Consent, download and continue").clicked()
                            && self.ai_runtime_ready()
                        {
                            self.ai.subject_consent_open = false;
                            self.start_subject_worker(self.birefnet_model_path(), true);
                        }
                        if ui.button("Cancel").clicked() {
                            self.ai.subject_consent_open = false;
                            if self.ai.mask_update_active {
                                self.cancel_ai_mask_update();
                            }
                        }
                    });
                });
        }

        if self.ai.object_consent_open {
            crate::ui::responsive_popup(
                egui::Window::new("Download object-selection model?"),
                ctx,
                520.0,
            )
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Object masks use SAM 2.1 Hiera Tiny with local edge-aware cleanup.");
                    ui.label(format!(
                        "The first use downloads about {:.0} MB for SAM and stores the ONNX files in CalibRaw's model cache.",
                        SAM21_MODEL_BYTES_ESTIMATE as f64 / 1_000_000.0
                    ));
                    ui.label("Model license: Apache-2.0. The model is optional and can be used only after this download.");
                    ui.label("Inference is local. No photograph or prompt stroke is uploaded.");
                    ui.label("When you continue, your device connects directly to Hugging Face. Hugging Face receives connection data such as your IP address and request time under its own privacy policy. CalibRaw sends no account identifier or telemetry.");
                    ui.horizontal_wrapped(|ui| {
                        ui.hyperlink_to(
                            "Hugging Face privacy policy",
                            "https://huggingface.co/privacy",
                        );
                        ui.separator();
                        ui.hyperlink_to(
                            "Apache-2.0 model license",
                            "https://github.com/facebookresearch/sam2/blob/main/LICENSE",
                        );
                    });
                    #[cfg(not(target_os = "android"))]
                    if self.ai.runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Consent, download and continue").clicked()
                            && self.ai_runtime_ready()
                        {
                            self.ai.object_consent_open = false;
                            if let Some((mask_index, component_index)) = self.ai.object_pending_target.take() {
                                let (encoder, decoder) = self.sam21_model_paths();
                                self.start_object_worker(mask_index, component_index, encoder, decoder, true);
                            }
                        }
                        if ui.button("Cancel").clicked() {
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
