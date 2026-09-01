use crate::app::{CalibRawApp, OnboardingStep, PreviewQuality};
use eframe::egui;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OnboardingAction {
    Back,
    Next,
    Finish,
}

#[derive(Clone, Copy, Debug)]
struct OnboardingLayoutMeasurement {
    step: OnboardingStep,
    width_bits: u32,
    height_bits: u32,
    needs_scroll: bool,
}

const ONBOARDING_LAYOUT_MEASUREMENT_ID: &str = "calibraw-first-run-onboarding-layout";
const ONBOARDING_FOOTER_EXTRA_HEIGHT: f32 = 30.0;

pub(crate) fn show(ctx: &egui::Context, app: &mut CalibRawApp) {
    let Some(step) = app.ui.onboarding_step else {
        return;
    };

    let available = ctx.content_rect().size() - egui::vec2(32.0, 32.0);
    let width = available.x.clamp(1.0, 540.0);
    let max_height = available.y.max(1.0);
    let max_body_height =
        (max_height - crate::ui::theme::CONTROL_HEIGHT - ONBOARDING_FOOTER_EXTRA_HEIGHT).max(1.0);
    let measurement = ctx.data(|data| {
        data.get_temp::<OnboardingLayoutMeasurement>(egui::Id::new(
            ONBOARDING_LAYOUT_MEASUREMENT_ID,
        ))
    });
    let needs_scroll = measurement
        .filter(|measurement| {
            measurement.step == step
                && measurement.width_bits == width.to_bits()
                && measurement.height_bits == max_body_height.to_bits()
        })
        .map(|measurement| measurement.needs_scroll);
    let mut action = None;

    egui::Modal::new(egui::Id::new(("calibraw-first-run-onboarding", step))).show(ctx, |ui| {
        ui.set_width(width);
        ui.set_max_width(width);
        if needs_scroll == Some(false) {
            show_step_body(ui, app, step);
        } else {
            let output = egui::ScrollArea::vertical()
                .max_height(max_body_height)
                .auto_shrink([false, true])
                .show(ui, |ui| show_step_body(ui, app, step));
            if needs_scroll.is_none() {
                let needs_scroll = output.content_size.y > output.inner_rect.height() + 1.0;
                ctx.data_mut(|data| {
                    data.insert_temp(
                        egui::Id::new(ONBOARDING_LAYOUT_MEASUREMENT_ID),
                        OnboardingLayoutMeasurement {
                            step,
                            width_bits: width.to_bits(),
                            height_bits: max_body_height.to_bits(),
                            needs_scroll,
                        },
                    );
                });
                if !needs_scroll {
                    ctx.request_discard("onboarding content fits without scrolling");
                }
            }
        }
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(8.0);
        show_navigation(ui, step, &mut action);
    });

    match action {
        Some(OnboardingAction::Back) => app.ui.onboarding_step = previous_step(step),
        Some(OnboardingAction::Next) => app.ui.onboarding_step = next_step(step),
        Some(OnboardingAction::Finish) => {
            app.preferences.onboarding_completed = true;
            app.ui.onboarding_step = None;
            if !app.persist_performance_settings() {
                app.ui.notice = Some(
                    "Setup is complete, but CalibRaw could not save the first-run preferences."
                        .to_owned(),
                );
            }
        }
        None => {}
    }
}

fn show_step_body(ui: &mut egui::Ui, app: &mut CalibRawApp, step: OnboardingStep) {
    show_header(ui, step);
    ui.add_space(8.0);
    match step {
        OnboardingStep::Appearance => show_appearance(ui, app),
        OnboardingStep::Preview => show_preview(ui, app),
        OnboardingStep::CopyPaste => show_copy_paste(ui, app),
        #[cfg(not(target_os = "android"))]
        OnboardingStep::Ai => show_ai(ui, app),
    }
}

fn show_header(ui: &mut egui::Ui, step: OnboardingStep) {
    ui.horizontal(|ui| {
        ui.heading(step.title());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.weak(format!("Step {} of {}", step.number(), step.total()));
        });
    });
    ui.label(step.introduction());
}

fn show_appearance(ui: &mut egui::Ui, app: &mut CalibRawApp) {
    let mut design = app.preferences.ui_design;
    crate::ui::theme::form_combo_with_help(
        ui,
        "Design",
        "onboarding-ui-design",
        design.label(),
        220.0,
        design.description(),
        |ui| {
            for option in crate::ui::theme::UiDesign::ALL {
                ui.selectable_value(&mut design, option, option.label())
                    .on_hover_text(option.description());
            }
        },
    );
    if design != app.preferences.ui_design {
        app.set_ui_design(design);
    }

    ui.add_space(8.0);
    let mut backdrop = app.preferences.preview_backdrop;
    crate::ui::theme::form_combo_with_help(
        ui,
        "Preview background",
        "onboarding-preview-backdrop",
        backdrop.label(),
        220.0,
        "Sets the canvas color around the photo. Match photo derives a quiet color from each image.",
        |ui| {
            for option in crate::ui::theme::PreviewBackdrop::ALL {
                ui.selectable_value(&mut backdrop, option, option.label());
            }
        },
    );
    if backdrop != app.preferences.preview_backdrop {
        app.set_preview_backdrop(backdrop);
    }

    ui.add_space(8.0);
    let sample_color = backdrop.color(app.ui.adaptive_preview_backdrop);
    let (sample, _) = ui.allocate_exact_size(
        egui::vec2(ui.available_width().max(1.0), 38.0),
        egui::Sense::hover(),
    );
    ui.painter().rect_filled(sample, 6.0, sample_color);
    ui.painter().rect_stroke(
        sample,
        6.0,
        ui.visuals().widgets.noninteractive.bg_stroke,
        egui::StrokeKind::Inside,
    );
}

fn show_preview(ui: &mut egui::Ui, app: &mut CalibRawApp) {
    let mut quality = app.preview.quality;
    crate::ui::theme::form_combo_with_help(
        ui,
        "Preview quality",
        "onboarding-preview-quality",
        quality.label(),
        180.0,
        "Higher quality renders more preview pixels and uses more GPU memory. Medium is a good starting point for most displays.",
        |ui| {
            for option in [
                PreviewQuality::Low,
                PreviewQuality::Medium,
                PreviewQuality::High,
                PreviewQuality::Max,
            ] {
                ui.selectable_value(&mut quality, option, option.label());
            }
        },
    );
    if quality != app.preview.quality {
        app.preview.quality = quality;
        app.preview_quality_changed();
    }

    ui.add_space(8.0);
    ui.small(match quality {
        PreviewQuality::Low => "75% render density · lowest GPU memory use",
        PreviewQuality::Medium => "100% render density · recommended balance",
        PreviewQuality::High => "125% render density · finer previews",
        PreviewQuality::Max => "150% render density · highest GPU memory use",
    });
}

fn show_copy_paste(ui: &mut egui::Ui, app: &mut CalibRawApp) {
    let mut settings = app.preferences.adjustment_copy_settings;
    let mut changed = false;
    changed |= crate::ui::theme::checkbox_with_help(
        ui,
        &mut settings.adjustments,
        "Adjustments",
        "Exposure, color, tone, detail, effects, and other Develop adjustments.",
    )
    .changed();
    changed |= crate::ui::theme::checkbox_with_help(
        ui,
        &mut settings.geometry,
        "Crop & geometry",
        "Crop, rotation, transforms, and flips.",
    )
    .changed();
    changed |= crate::ui::theme::checkbox_with_help(
        ui,
        &mut settings.camera_profile,
        "Camera profile",
        "The selected camera profile, when it is available for the destination image.",
    )
    .changed();
    changed |= crate::ui::theme::checkbox_with_help(
        ui,
        &mut settings.masks,
        "Manual masks",
        "Brush, linear, radial, and fullscreen mask components with their local edits.",
    )
    .changed();
    changed |= crate::ui::theme::checkbox_with_help(
        ui,
        &mut settings.ai_masks,
        "Content-aware masks",
        "Subject, background, object, luminance-range, and color-range components. They are regenerated for the destination image when needed.",
    )
    .changed();
    changed |= crate::ui::theme::checkbox_with_help(
        ui,
        &mut settings.lens_correction,
        "Lens correction",
        "The enabled state and selected lens profile.",
    )
    .changed();
    if changed {
        app.set_adjustment_copy_settings(settings);
    }
}

#[cfg(not(target_os = "android"))]
fn show_ai(ui: &mut egui::Ui, app: &mut CalibRawApp) {
    let mut acceleration = app.ai.gpu_acceleration;
    if crate::ui::theme::checkbox_with_help(
        ui,
        &mut acceleration,
        "Use GPU acceleration when available",
        "Allows AI masks and AI denoise to use a supported GPU execution provider. CPU fallback remains automatic.",
    )
    .changed()
    {
        app.set_ai_gpu_acceleration(acceleration);
    }

    ui.add_space(8.0);
    let mut quality = app.ai.birefnet_quality;
    let explanation = quality.model().explanation;
    crate::ui::theme::form_combo_with_help(
        ui,
        "Subject mask quality",
        "onboarding-subject-mask-quality",
        quality.label(),
        180.0,
        explanation,
        |ui| {
            for option in crate::ai_masks::BiRefNetQuality::ALL {
                ui.selectable_value(&mut quality, option, option.label())
                    .on_hover_text(option.model().explanation);
            }
        },
    );
    if quality != app.ai.birefnet_quality {
        app.set_birefnet_quality(quality);
    }
    ui.add_space(8.0);
    ui.small(quality.model().explanation);
    ui.small("AI models are downloaded only when you first use the corresponding tool.");
}

fn show_navigation(ui: &mut egui::Ui, step: OnboardingStep, action: &mut Option<OnboardingAction>) {
    ui.horizontal(|ui| {
        if ui
            .add_enabled(
                step != OnboardingStep::Appearance,
                egui::Button::new("Back"),
            )
            .clicked()
        {
            *action = Some(OnboardingAction::Back);
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let final_step = is_final_step(step);
            if ui
                .button(if final_step { "Finish setup" } else { "Next" })
                .clicked()
            {
                *action = Some(if final_step {
                    OnboardingAction::Finish
                } else {
                    OnboardingAction::Next
                });
            }
        });
    });
}

fn previous_step(step: OnboardingStep) -> Option<OnboardingStep> {
    Some(match step {
        OnboardingStep::Appearance => return Some(OnboardingStep::Appearance),
        OnboardingStep::Preview => OnboardingStep::Appearance,
        OnboardingStep::CopyPaste => OnboardingStep::Preview,
        #[cfg(not(target_os = "android"))]
        OnboardingStep::Ai => OnboardingStep::CopyPaste,
    })
}

fn next_step(step: OnboardingStep) -> Option<OnboardingStep> {
    match step {
        OnboardingStep::Appearance => Some(OnboardingStep::Preview),
        OnboardingStep::Preview => Some(OnboardingStep::CopyPaste),
        OnboardingStep::CopyPaste => {
            #[cfg(not(target_os = "android"))]
            {
                Some(OnboardingStep::Ai)
            }
            #[cfg(target_os = "android")]
            {
                None
            }
        }
        #[cfg(not(target_os = "android"))]
        OnboardingStep::Ai => None,
    }
}

const fn is_final_step(step: OnboardingStep) -> bool {
    #[cfg(not(target_os = "android"))]
    {
        matches!(step, OnboardingStep::Ai)
    }
    #[cfg(target_os = "android")]
    {
        matches!(step, OnboardingStep::CopyPaste)
    }
}

impl OnboardingStep {
    const fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Welcome to CalibRaw",
            Self::Preview => "Preview setup",
            Self::CopyPaste => "Copy & paste setup",
            #[cfg(not(target_os = "android"))]
            Self::Ai => "AI model setup",
        }
    }

    const fn introduction(self) -> &'static str {
        match self {
            Self::Appearance => {
                "Choose how CalibRaw looks and the background shown around your photographs."
            }
            Self::Preview => {
                "Choose the balance between interactive preview detail and GPU memory use."
            }
            Self::CopyPaste => {
                "Choose which edit categories are included when copying and pasting adjustments."
            }
            #[cfg(not(target_os = "android"))]
            Self::Ai => {
                "Choose how local AI models use your hardware and the quality of new Subject masks."
            }
        }
    }

    const fn number(self) -> usize {
        match self {
            Self::Appearance => 1,
            Self::Preview => 2,
            Self::CopyPaste => 3,
            #[cfg(not(target_os = "android"))]
            Self::Ai => 4,
        }
    }

    const fn total(self) -> usize {
        let _ = self;
        if cfg!(target_os = "android") {
            3
        } else {
            4
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn onboarding_steps_move_forward_and_back() {
        assert_eq!(
            next_step(OnboardingStep::Appearance),
            Some(OnboardingStep::Preview)
        );
        assert_eq!(
            next_step(OnboardingStep::Preview),
            Some(OnboardingStep::CopyPaste)
        );
        assert_eq!(
            previous_step(OnboardingStep::Preview),
            Some(OnboardingStep::Appearance)
        );
        assert_eq!(
            previous_step(OnboardingStep::CopyPaste),
            Some(OnboardingStep::Preview)
        );
    }

    #[test]
    fn final_step_ends_the_sequence() {
        #[cfg(not(target_os = "android"))]
        let final_step = OnboardingStep::Ai;
        #[cfg(target_os = "android")]
        let final_step = OnboardingStep::CopyPaste;

        assert!(is_final_step(final_step));
        assert_eq!(next_step(final_step), None);
    }
}
