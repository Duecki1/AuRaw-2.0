use eframe::egui;
use std::sync::mpsc;

/// Spawns a one-shot UI-owned worker and wakes egui after the result is ready.
///
/// Long-running work with progress/cancellation stays in `BackgroundTaskManager`;
/// this helper is only for small picker/settings operations that return one value.
pub(crate) fn spawn_ui_worker<T, F>(context: &egui::Context, work: F) -> mpsc::Receiver<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (sender, receiver) = mpsc::channel();
    let context = context.clone();
    std::thread::spawn(move || {
        let result = work();
        let _ = sender.send(result);
        context.request_repaint();
    });
    receiver
}

/// Drains all currently available worker events, optionally stopping after a
/// terminal event. The boolean reports whether the producer disconnected.
pub(crate) fn drain_worker_events<T>(
    receiver: Option<&mpsc::Receiver<T>>,
    is_terminal: impl Fn(&T) -> bool,
) -> (Vec<T>, bool) {
    let mut events = Vec::new();
    let mut disconnected = false;
    let Some(receiver) = receiver else {
        return (events, disconnected);
    };
    loop {
        match receiver.try_recv() {
            Ok(event) => {
                let terminal = is_terminal(&event);
                events.push(event);
                if terminal {
                    break;
                }
            }
            Err(mpsc::TryRecvError::Empty) => break,
            Err(mpsc::TryRecvError::Disconnected) => {
                disconnected = true;
                break;
            }
        }
    }
    (events, disconnected)
}

pub(crate) fn show_download_progress(
    ui: &mut egui::Ui,
    label: impl Into<egui::WidgetText>,
    downloaded: u64,
    total: u64,
) {
    ui.label(label);
    ui.add(
        egui::ProgressBar::new(downloaded as f32 / total.max(1) as f32)
            .show_percentage()
            .text(format!(
                "{:.1} / {:.1} MB",
                downloaded as f64 / 1_000_000.0,
                total as f64 / 1_000_000.0
            )),
    );
}

#[derive(Clone, Copy, Default)]
pub(crate) struct WorkerDialogAction {
    minimize: bool,
    cancel: bool,
}

pub(crate) fn show_cancellable_worker_popup(
    ctx: &egui::Context,
    title: &str,
    width: f32,
    add_contents: impl FnOnce(&mut egui::Ui),
) -> WorkerDialogAction {
    let mut action = WorkerDialogAction::default();
    crate::ui::responsive_popup(egui::Window::new(title), ctx, width)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
        .show(ctx, |ui| {
            add_contents(ui);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                #[cfg(not(target_os = "android"))]
                {
                    action.minimize = ui.button("Minimize").clicked();
                }
                action.cancel = ui.button("Cancel").clicked();
            });
        });
    action
}

impl super::AurawApp {
    pub(crate) fn apply_worker_dialog_action(
        &mut self,
        task_id: Option<super::TaskId>,
        action: WorkerDialogAction,
    ) {
        let Some(task_id) = task_id else {
            return;
        };
        #[cfg(not(target_os = "android"))]
        if action.minimize {
            self.set_background_task_details_open(task_id, false);
        }
        if action.cancel {
            self.cancel_background_task(task_id);
        }
    }
}
