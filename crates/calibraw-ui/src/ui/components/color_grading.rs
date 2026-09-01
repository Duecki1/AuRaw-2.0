use crate::app::ColorGradeTab;
use crate::pipeline::{ColorGradeWheel, ColorGrading};
use crate::ui::components::adjustment_slider::{
    adjustment_slider, gradient_adjustment_slider, SliderGradient,
};
use eframe::egui::{self, Color32, Mesh, Pos2, Sense, Shape, Stroke, Ui};

const WHEEL_MAX_SIZE: f32 = 190.0;
const ANGULAR_SEGMENTS: usize = 96;
const RADIAL_SEGMENTS: usize = 12;
#[cfg(target_os = "android")]
const ANDROID_TAB_RAIL_WIDTH: f32 = 72.0;

const COLOR_GRADE_TABS: [(ColorGradeTab, &str, &str); 4] = [
    (
        ColorGradeTab::Shadows,
        "Shadows",
        "Grade the darker tonal range",
    ),
    (
        ColorGradeTab::Midtones,
        "Mid",
        "Grade the middle tonal range",
    ),
    (
        ColorGradeTab::Highlights,
        "High",
        "Grade the brighter tonal range",
    ),
    (ColorGradeTab::Global, "Global", "Grade all tones uniformly"),
];

pub(crate) fn color_grading_editor(
    ui: &mut Ui,
    grading: &mut ColorGrading,
    selected: &mut ColorGradeTab,
) -> bool {
    ui.push_id("color-grading-editor", |ui| {
        color_grading_editor_contents(ui, grading, selected)
    })
    .inner
}

fn color_grading_editor_contents(
    ui: &mut Ui,
    grading: &mut ColorGrading,
    selected: &mut ColorGradeTab,
) -> bool {
    let mut changed = false;
    let editor_width = ui.available_width().max(1.0);
    ui.set_width(editor_width);
    ui.set_max_width(editor_width);

    #[cfg(not(target_os = "android"))]
    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Four-way color grading");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset all color grading",
            )
            .clicked()
            {
                grading.reset();
                changed = true;
            }
        });
    });

    #[cfg(not(target_os = "android"))]
    {
        color_grade_tab_row(ui, selected);
        ui.add_space(4.0);

        let (wheel_id, wheel) = selected_color_wheel(grading, *selected);
        changed |= ui.push_id(wheel_id, |ui| color_wheel(ui, wheel)).inner;
    }

    #[cfg(target_os = "android")]
    {
        ui.horizontal(|ui| {
            color_grade_tab_rail(ui, selected);
            let (wheel_id, wheel) = selected_color_wheel(grading, *selected);
            let wheel_edge = ui.available_width().clamp(1.0, WHEEL_MAX_SIZE);
            changed |= ui
                .allocate_ui_with_layout(
                    egui::vec2(wheel_edge, wheel_edge),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.push_id((wheel_id, "picker"), |ui| color_wheel_picker(ui, wheel))
                            .inner
                    },
                )
                .inner;
        });

        let (wheel_id, wheel) = selected_color_wheel(grading, *selected);
        changed |= ui
            .push_id((wheel_id, "sliders"), |ui| color_wheel_sliders(ui, wheel))
            .inner;
    }

    ui.separator();
    changed |= adjustment_slider(
        ui,
        "Blending",
        &mut grading.blending,
        0.0..=100.0,
        0,
        1.0,
        Some("Controls the overlap between shadows, midtones, and highlights."),
    );
    changed |= adjustment_slider(
        ui,
        "Balance",
        &mut grading.balance,
        -100.0..=100.0,
        0,
        1.0,
        Some("Moves the tonal pivot between shadow and highlight grading."),
    );

    changed
}

#[cfg(not(target_os = "android"))]
fn color_grade_tab_row(ui: &mut Ui, selected: &mut ColorGradeTab) {
    ui.horizontal(|ui| {
        let segment_width =
            ((ui.available_width() - ui.spacing().item_spacing.x * 3.0).max(4.0)) / 4.0;
        for (tab, label, tooltip) in COLOR_GRADE_TABS {
            if crate::ui::theme::segmented_button(ui, label, *selected == tab, segment_width)
                .on_hover_text(tooltip)
                .clicked()
            {
                *selected = tab;
            }
        }
    });
}

#[cfg(target_os = "android")]
fn color_grade_tab_rail(ui: &mut Ui, selected: &mut ColorGradeTab) {
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y =
            ((WHEEL_MAX_SIZE - crate::ui::theme::CONTROL_HEIGHT * 4.0) / 3.0).max(0.0);
        for (tab, label, tooltip) in COLOR_GRADE_TABS {
            if crate::ui::theme::segmented_button(
                ui,
                label,
                *selected == tab,
                ANDROID_TAB_RAIL_WIDTH,
            )
            .on_hover_text(tooltip)
            .clicked()
            {
                *selected = tab;
            }
        }
    });
}

fn selected_color_wheel(
    grading: &mut ColorGrading,
    selected: ColorGradeTab,
) -> (&'static str, &mut ColorGradeWheel) {
    match selected {
        ColorGradeTab::Shadows => ("shadows", &mut grading.shadows),
        ColorGradeTab::Midtones => ("midtones", &mut grading.midtones),
        ColorGradeTab::Highlights => ("highlights", &mut grading.highlights),
        ColorGradeTab::Global => ("global", &mut grading.global),
    }
}

#[cfg(not(target_os = "android"))]
fn color_wheel(ui: &mut Ui, wheel: &mut ColorGradeWheel) -> bool {
    let mut changed = false;
    changed |= color_wheel_toolbar(ui, wheel);
    changed |= color_wheel_picker(ui, wheel);
    changed |= color_wheel_sliders(ui, wheel);
    changed
}

#[cfg(not(target_os = "android"))]
fn color_wheel_toolbar(ui: &mut Ui, wheel: &mut ColorGradeWheel) -> bool {
    let mut changed = false;
    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.strong("Hue / Saturation");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset this color wheel",
            )
            .clicked()
            {
                wheel.reset();
                changed = true;
            }
        });
    });
    changed
}

fn color_wheel_picker(ui: &mut Ui, wheel: &mut ColorGradeWheel) -> bool {
    let mut changed = false;
    let size = ui.available_width().clamp(1.0, WHEEL_MAX_SIZE);

    ui.vertical_centered(|ui| {
        let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), Sense::click_and_drag());
        let painter = ui.painter_at(rect);
        let center = rect.center();
        let radius = 0.5 * rect.width().min(rect.height()) - 4.0;

        painter.circle_filled(center, radius + 2.0, ui.visuals().extreme_bg_color);
        painter.add(Shape::mesh(build_wheel_mesh(center, radius)));
        painter.circle_stroke(
            center,
            radius,
            Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
        );
        painter.circle_stroke(
            center,
            radius * 0.5,
            Stroke::new(0.75, Color32::from_white_alpha(45)),
        );
        painter.line_segment(
            [
                Pos2::new(center.x - radius, center.y),
                Pos2::new(center.x + radius, center.y),
            ],
            Stroke::new(0.5, Color32::from_white_alpha(35)),
        );
        painter.line_segment(
            [
                Pos2::new(center.x, center.y - radius),
                Pos2::new(center.x, center.y + radius),
            ],
            Stroke::new(0.5, Color32::from_white_alpha(35)),
        );

        let angle = wheel.hue.to_radians();
        let marker_radius = radius * (wheel.saturation / 100.0).clamp(0.0, 1.0);
        let marker = Pos2::new(
            center.x + angle.cos() * marker_radius,
            center.y - angle.sin() * marker_radius,
        );
        painter.circle_filled(marker, 5.0, Color32::WHITE);
        painter.circle_stroke(marker, 6.5, Stroke::new(1.5, Color32::BLACK));

        if response.double_clicked() {
            wheel.reset();
            changed = true;
        } else if let Some(pointer) = (response.dragged() || response.clicked())
            .then(|| response.interact_pointer_pos())
            .flatten()
        {
            let offset = pointer - center;
            let distance = offset.length().min(radius);
            let new_saturation = (distance / radius * 100.0).clamp(0.0, 100.0);
            let new_hue = (-offset.y).atan2(offset.x).to_degrees().rem_euclid(360.0);
            if (wheel.saturation - new_saturation).abs() > f32::EPSILON
                || (wheel.hue - new_hue).abs() > f32::EPSILON
            {
                wheel.saturation = new_saturation;
                if distance > 1.0 {
                    wheel.hue = new_hue;
                }
                changed = true;
            }
        }

        response.on_hover_text(
            "Drag from the center to choose hue and saturation. Double-click the wheel to reset it.",
        )
    });

    changed
}

fn color_wheel_sliders(ui: &mut Ui, wheel: &mut ColorGradeWheel) -> bool {
    let mut changed = false;

    changed |= gradient_adjustment_slider(
        ui,
        "Hue",
        &mut wheel.hue,
        0.0..=360.0,
        0,
        1.0,
        Some("Sets the color-wheel angle in degrees."),
        SliderGradient::HueDegrees {
            start: 0.0,
            end: 360.0,
        },
    );
    wheel.hue = wheel.hue.rem_euclid(360.0);
    let grade_color = hsv_color(wheel.hue / 360.0, 0.86, 0.90);
    changed |= gradient_adjustment_slider(
        ui,
        "Saturation",
        &mut wheel.saturation,
        0.0..=100.0,
        0,
        1.0,
        Some("Sets the distance from the neutral center of the wheel."),
        SliderGradient::Saturation(grade_color),
    );
    wheel.saturation = wheel.saturation.clamp(0.0, 100.0);

    changed |= gradient_adjustment_slider(
        ui,
        "Luminance",
        &mut wheel.luminance,
        -100.0..=100.0,
        0,
        1.0,
        Some("Applies a hue-preserving scene-linear exposure gain to this tonal range."),
        SliderGradient::Luminance(grade_color),
    );
    wheel.luminance = wheel.luminance.clamp(-100.0, 100.0);

    changed
}

fn build_wheel_mesh(center: Pos2, radius: f32) -> Mesh {
    let mut mesh = Mesh::default();
    for ring in 0..=RADIAL_SEGMENTS {
        let radial = ring as f32 / RADIAL_SEGMENTS as f32;
        for segment in 0..=ANGULAR_SEGMENTS {
            let hue = segment as f32 / ANGULAR_SEGMENTS as f32;
            let angle = hue * std::f32::consts::TAU;
            let pos = Pos2::new(
                center.x + angle.cos() * radius * radial,
                center.y - angle.sin() * radius * radial,
            );
            mesh.vertices.push(egui::epaint::Vertex {
                pos,
                uv: Pos2::ZERO,
                color: hsv_color(hue, radial.powf(0.92), 0.90),
            });
        }
    }

    let stride = ANGULAR_SEGMENTS + 1;
    for ring in 0..RADIAL_SEGMENTS {
        for segment in 0..ANGULAR_SEGMENTS {
            let a = (ring * stride + segment) as u32;
            let b = a + 1;
            let c = ((ring + 1) * stride + segment) as u32;
            let d = c + 1;
            mesh.indices.extend_from_slice(&[a, c, b, b, c, d]);
        }
    }
    mesh
}

fn hsv_color(hue: f32, saturation: f32, value: f32) -> Color32 {
    let h = hue.rem_euclid(1.0) * 6.0;
    let i = h.floor() as i32;
    let f = h - i as f32;
    let p = value * (1.0 - saturation);
    let q = value * (1.0 - saturation * f);
    let t = value * (1.0 - saturation * (1.0 - f));
    let (r, g, b) = match i.rem_euclid(6) {
        0 => (value, t, p),
        1 => (q, value, p),
        2 => (p, value, t),
        3 => (p, q, value),
        4 => (t, p, value),
        _ => (value, p, q),
    };
    Color32::from_rgb(
        (r * 255.0).round() as u8,
        (g * 255.0).round() as u8,
        (b * 255.0).round() as u8,
    )
}
