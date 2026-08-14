use super::*;

impl AurawApp {
    pub(in crate::app) fn show_subject_dialogs(&mut self, ctx: &egui::Context) {
        let library_batch_refreshing = self.library_ai_mask_refresh.is_some();
        if self.subject_consent_open {
            crate::ui::responsive_popup(
                egui::Window::new("Download subject-selection model?"),
                ctx,
                520.0,
            )
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    let model = self.birefnet_quality.model();
                    ui.label(format!(
                        "{} quality uses {} with its native {} x {} input tensor.",
                        self.birefnet_quality.label(),
                        model.checkpoint,
                        model.input_height,
                        model.input_width
                    ));
                    ui.label(model.explanation);
                    ui.label("Subject masks use BiRefNet's calibrated soft selection directly. Not Subject is the exact inverse of the subject alpha.");
                    ui.label(format!(
                        "The first use downloads about {:.0} MB and stores the ONNX model in AuRaw's cache.",
                        model.bytes as f64 / 1_000_000.0
                    ));
                    ui.label("Model license: BiRefNet MIT. The model is optional and used only after this download.");
                    ui.label("Inference is local. No photograph is uploaded.");
                    ui.label("When you continue, your device connects directly to GitHub for BiRefNet. GitHub receives connection data such as your IP address and request time under its privacy policy. AuRaw sends no account identifier or telemetry.");
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
                    if self.onnx_runtime_path.is_none() {
                        ui.colored_label(
                            egui::Color32::YELLOW,
                            "Select a trusted local ONNX Runtime library in Settings before continuing. AuRaw never downloads native runtime code.",
                        );
                    }
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.button("Consent, download and continue").clicked()
                            && self.ai_runtime_ready()
                        {
                            self.subject_consent_open = false;
                            self.start_subject_worker(self.birefnet_model_path());
                        }
                        if ui.button("Cancel").clicked() {
                            self.subject_consent_open = false;
                            if self.ai_mask_update_active {
                                self.cancel_ai_mask_update();
                            }
                        }
                    });
                });
        }
        // The Library batch progress is the operation-level dialog. Do not
        // cover it with a second worker-level window while refreshing pasted
        // masks; the batch dialog stays visible for the entire operation.
        if self.subject_receiver.is_some() && !library_batch_refreshing
            && self
                .subject_task_id
                .is_some_and(|id| self.background_task_details_open(id))
        {
            let action = show_cancellable_worker_popup(ctx, "Preparing subject mask", 420.0, |ui| {
                if let Some((label, downloaded, total)) = self.subject_download_progress {
                    show_download_progress(ui, format!("Downloading {label}…"), downloaded, total);
                } else if self.subject_inferencing {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(format!(
                            "Running {} quality locally with {}…",
                            self.birefnet_quality.label(),
                            self.birefnet_quality.model().checkpoint
                        ));
                    });
                } else {
                    ui.spinner();
                }
            });
            let task_id = self.subject_task_id;
            self.apply_worker_dialog_action(task_id, action);
            ctx.request_repaint_after(Duration::from_millis(50));
        }

            crate::ui::responsive_popup(
                ctx,
                540.0,
            )
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(format!(
                    "The first use downloads up to {:.1} MB and stores both ONNX models in AuRaw's cache. The shared ViTMatte model is reused when already installed.",
                ));
                ui.label("Inference is local. No photograph or selected category is uploaded.");
                ui.label("When you continue, your device connects directly to Hugging Face. It receives connection data such as your IP address and request time under its privacy policy. AuRaw sends no account identifier or telemetry.");
                ui.label(format!(
                ));
                ui.horizontal_wrapped(|ui| {
                    ui.hyperlink_to(
                        "Hugging Face privacy policy",
                        "https://huggingface.co/privacy",
                    );
                    ui.separator();
                    ui.hyperlink_to(
                    );
                    ui.separator();
                    ui.hyperlink_to(
                        "Model card",
                    );
                    ui.separator();
                    ui.hyperlink_to(
                        "ViTMatte model and license",
                        "https://huggingface.co/hustvl/vitmatte-small-composition-1k",
                    );
                });
                #[cfg(not(target_os = "android"))]
                if self.onnx_runtime_path.is_none() {
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
                        if let Some((mask_index, component_index)) =
                        {
                                mask_index,
                                component_index,
                                true,
                            );
                        }
                    }
                    if ui.button("Cancel").clicked() {
                        if self.ai_mask_update_active {
                            self.cancel_ai_mask_update();
                        }
                    }
                });
            });
        }
            && !library_batch_refreshing
            && self
                .is_some_and(|id| self.background_task_details_open(id))
        {
                    show_download_progress(
                        ui,
                        downloaded,
                        total,
                    );
                    ui.horizontal(|ui| {
                        ui.spinner();
                    });
                } else {
                    ui.spinner();
                }
            });
            self.apply_worker_dialog_action(task_id, action);
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        if self.object_consent_open {
            crate::ui::responsive_popup(
                egui::Window::new("Download object-selection model?"),
                ctx,
                520.0,
            )
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
                .show(ctx, |ui| {
                    ui.label("Object masks use SAM 2.1 Hiera Tiny followed automatically by ViTMatte trimap-guided alpha matting for fine hair, fur, and semi-transparent boundaries.");
                    ui.label(format!(
                        "The first use downloads about {:.0} MB for SAM plus {:.0} MB for ViTMatte and stores the ONNX files in AuRaw's model cache.",
                        SAM21_MODEL_BYTES_ESTIMATE as f64 / 1_000_000.0,
                        VITMATTE_MODEL_BYTES as f64 / 1_000_000.0
                    ));
                    ui.label("Model licenses: Apache-2.0. The models are optional and can be used only after this download.");
                    ui.label("Inference is local. No photograph or prompt stroke is uploaded.");
                    ui.label("When you continue, your device connects directly to Hugging Face. Hugging Face receives connection data such as your IP address and request time under its own privacy policy. AuRaw sends no account identifier or telemetry.");
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
                    if self.onnx_runtime_path.is_none() {
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
                            self.object_consent_open = false;
                            if let Some((mask_index, component_index)) = self.object_pending_target.take() {
                                let (encoder, decoder) = self.sam21_model_paths();
                                self.start_object_worker(mask_index, component_index, encoder, decoder);
                            }
                        }
                        if ui.button("Cancel").clicked() {
                            self.object_consent_open = false;
                            self.object_pending_target = None;
                            if self.ai_mask_update_active {
                                self.cancel_ai_mask_update();
                            }
                        }
                    });
                });
        }
        if self.object_receiver.is_some() && !library_batch_refreshing
            && self
                .object_task_id
                .is_some_and(|id| self.background_task_details_open(id))
        {
            let action = show_cancellable_worker_popup(ctx, "Preparing object mask", 420.0, |ui| {
                if let Some((label, downloaded, total)) = self.object_download_progress {
                    show_download_progress(ui, format!("Downloading {label}…"), downloaded, total);
                } else if self.object_inferencing {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(if self.object_decoder_only {
                            "Updating the object mask…"
                        } else {
                            "Encoding the selected image region and generating the object mask…"
                        });
                    });
                } else {
                    ui.spinner();
                }
            });
            let task_id = self.object_task_id;
            self.apply_worker_dialog_action(task_id, action);
            ctx.request_repaint_after(Duration::from_millis(50));
        }

        if let Some(message) = self.object_error_dialog.clone() {
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
                self.object_error_dialog = None;
            }
        }
    }
}
