use super::*;
use std::sync::atomic::Ordering;

fn try_install_foreground_operation(
    slot: &mut Option<ForegroundOperation>,
    mut operation: ForegroundOperation,
) -> bool {
    if slot.is_some() {
        operation.cancel();
        return false;
    }
    *slot = Some(operation);
    true
}

impl ForegroundOperation {
    fn cancel(&mut self) {
        self.cancellation.store(true, Ordering::Release);
        self.cancelling = true;
        self.progress = ForegroundProgress::indeterminate("Cancelling…");
    }

    pub(in crate::app) fn is_cancelled(&self) -> bool {
        self.cancelling || self.cancellation.load(Ordering::Acquire)
    }

    pub(in crate::app) fn accepts_result(&self, document_id: u64) -> bool {
        self.document_id == document_id && !self.is_cancelled()
    }
}

impl ForegroundOperationKind {
    pub(crate) const fn title(self) -> &'static str {
        match self {
            Self::SubjectMask => "Preparing subject mask",
            Self::ObjectMask => "Preparing object mask",
            Self::AiDenoise => "Applying AI denoise",
            Self::LensCorrection => "Applying lens correction",
        }
    }
}

impl CalibRawApp {
    pub(crate) fn foreground_operation_kind(&self) -> Option<ForegroundOperationKind> {
        self.foreground_operation
            .as_ref()
            .map(|operation| operation.kind)
    }

    pub(crate) fn foreground_operation_is(&self, kind: ForegroundOperationKind) -> bool {
        self.foreground_operation_kind() == Some(kind)
    }

    pub(crate) fn foreground_operation_active(&self) -> bool {
        self.foreground_operation.is_some()
    }

    pub(crate) fn begin_foreground_operation(&mut self, operation: ForegroundOperation) -> bool {
        let started = try_install_foreground_operation(&mut self.foreground_operation, operation);
        if started {
            self.egui_ctx.request_repaint();
        }
        started
    }

    pub(crate) fn cancel_foreground_operation(&mut self) -> bool {
        let Some(operation) = self.foreground_operation.as_mut() else {
            return false;
        };
        operation.cancel();
        self.egui_ctx.request_repaint();
        true
    }

    pub(crate) fn cancel_foreground_operation_if(&mut self, kind: ForegroundOperationKind) -> bool {
        if self.foreground_operation_is(kind) {
            self.cancel_foreground_operation()
        } else {
            false
        }
    }

    pub(crate) fn show_foreground_operation_dialog(&mut self, ctx: &egui::Context) {
        if self.ai.library_mask_refresh.is_some() {
            return;
        }
        let Some(operation) = self.foreground_operation.as_ref() else {
            return;
        };
        let kind = operation.kind;
        let progress = operation.progress.clone();
        let cancelling = operation.cancelling;
        let mut cancel = false;
        crate::ui::responsive_popup(egui::Window::new(kind.title()), ctx, 440.0)
            .id(egui::Id::new("foreground-operation-progress"))
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                ui.label(&progress.phase);
                if let Some(detail) = &progress.detail {
                    ui.label(egui::RichText::new(detail).small());
                }
                ui.add_space(6.0);
                match progress.value {
                    ForegroundProgressValue::Indeterminate => {
                        ui.add(egui::ProgressBar::new(0.0).animate(!cancelling));
                    }
                    ForegroundProgressValue::Units {
                        completed,
                        total,
                        ref unit,
                    } => {
                        if total == 0 {
                            ui.add(egui::ProgressBar::new(0.0).animate(!cancelling));
                        } else {
                            let fraction = (completed as f32 / total as f32).clamp(0.0, 1.0);
                            let text = unit.as_deref().map_or_else(
                                || format!("{completed} / {total}"),
                                |unit| format!("{completed} / {total} {unit}"),
                            );
                            ui.add(egui::ProgressBar::new(fraction).text(text));
                        }
                    }
                }
                if cancelling {
                    ui.label(egui::RichText::new("Stopping at the next safe point…").small());
                }
                ui.add_space(8.0);
                if ui
                    .add_enabled(!cancelling, egui::Button::new("Cancel"))
                    .clicked()
                {
                    cancel = true;
                }
            });
        if cancel {
            self.cancel_foreground_operation();
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

impl CalibRawApp {
    pub(in crate::app) fn poll_foreground_operation(&mut self, frame: &eframe::Frame) {
        match self.foreground_operation_kind() {
            Some(ForegroundOperationKind::SubjectMask) => self.poll_subject_worker(),
            Some(ForegroundOperationKind::ObjectMask) => self.poll_object_worker(),
            Some(ForegroundOperationKind::AiDenoise) => self.poll_ai_denoise_worker(),
            Some(ForegroundOperationKind::LensCorrection) => {
                self.poll_lens_correction_worker(frame)
            }
            None => {}
        }
    }

    pub(super) fn cancel_document_bound_foreground_operation(&mut self) {
        self.cancel_foreground_operation();
    }

    #[cfg(target_os = "android")]
    pub(crate) fn android_foreground_task_active(&self) -> bool {
        self.foreground_operation_active() || self.ai.library_mask_refresh.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_operation(document_id: u64) -> ForegroundOperation {
        let (_sender, receiver) = mpsc::channel::<SubjectMaskEvent>();
        ForegroundOperation {
            kind: ForegroundOperationKind::SubjectMask,
            document_id,
            cancellation: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            progress: ForegroundProgress::indeterminate("Testing…"),
            cancelling: false,
            receiver: ForegroundOperationReceiver::Subject(receiver),
            context: ForegroundOperationContext::Subject,
        }
    }

    #[test]
    fn only_one_foreground_operation_can_occupy_the_slot() {
        let mut slot = None;
        assert!(try_install_foreground_operation(
            &mut slot,
            test_operation(7)
        ));
        assert!(!try_install_foreground_operation(
            &mut slot,
            test_operation(7)
        ));
        assert_eq!(
            slot.as_ref().map(|operation| operation.kind),
            Some(ForegroundOperationKind::SubjectMask)
        );
    }

    #[test]
    fn foreground_completion_releases_the_slot() {
        let mut slot = Some(test_operation(7));
        let completed = slot.take();
        assert!(completed.is_some());
        assert!(slot.is_none());
    }

    #[test]
    fn foreground_cancellation_rejects_late_results() {
        let mut operation = test_operation(7);
        operation.cancel();
        assert!(operation.is_cancelled());
        assert!(!operation.accepts_result(7));
    }

    #[test]
    fn stale_foreground_results_are_rejected_after_document_change() {
        let operation = test_operation(7);
        assert!(operation.accepts_result(7));
        assert!(!operation.accepts_result(8));
    }
}
