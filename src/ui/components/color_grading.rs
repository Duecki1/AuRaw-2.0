use crate::app::ColorGradeTab;
use crate::pipeline::{ColorGradeWheel, ColorGrading};
use crate::ui::components::adjustment_slider::adjustment_slider;
use eframe::egui::{self, Color32, DragValue, Mesh, Pos2, Sense, Shape, Stroke, Ui};

const WHEEL_MAX_SIZE: f32 = 190.0;
const WHEEL_MIN_SIZE: f32 = 150.0;
const ANGULAR_SEGMENTS: usize = 96;
const RADIAL_SEGMENTS: usize = 12;

pub fn color_grading_editor(
    ui: &mut Ui,
    grading: &mut ColorGrading,
    selected: &mut ColorGradeTab,
) -> bool {
    let mut changed = false;

    ui.horizontal(|ui| {
        ui.strong("Four-way color grading");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Reset grading").clicked() {
                grading.reset();
                changed = true;
            }
        });
    });

    ui.horizontal_wrapped(|ui| {
        for (tab, label, tooltip) in [
            (
                ColorGradeTab::Shadows,
                "Shadows",
                "Grade the darker tonal range",
            ),
            (
                ColorGradeTab::Midtones,
                "Midtones",
                "Grade the middle tonal range",
            ),
            (
                ColorGradeTab::Highlights,
                "Highlights",
                "Grade the brighter tonal range",
            ),
            (ColorGradeTab::Global, "Global", "Grade all tones uniformly"),
        ] {
            ui.selectable_value(selected, tab, label)
                .on_hover_text(tooltip);
        }
    });
    ui.add_space(4.0);

    {
        let wheel = match selected {
            ColorGradeTab::Shadows => &mut grading.shadows,
            ColorGradeTab::Midtones => &mut grading.midtones,
            ColorGradeTab::Highlights => &mut grading.highlights,
            ColorGradeTab::Global => &mut grading.global,
        };
        changed |= color_wheel(ui, wheel);
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

fn color_wheel(ui: &mut Ui, wheel: &mut ColorGradeWheel) -> bool {
    let mut changed = false;
    let size = ui.available_width().clamp(WHEEL_MIN_SIZE, WHEEL_MAX_SIZE);

    ui.horizontal(|ui| {
        ui.strong("Hue / Saturation");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("Reset wheel").clicked() {
                wheel.reset();
                changed = true;
            }
        });
    });

    let wheel_response = ui.vertical_centered(|ui| {
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
    let _ = wheel_response;

    ui.horizontal(|ui| {
        ui.label("Hue");
        changed |= ui
            .add(
                DragValue::new(&mut wheel.hue)
                    .range(0.0..=360.0)
                    .speed(1.0)
                    .fixed_decimals(0)
                    .suffix("°"),
            )
            .changed();
        ui.separator();
        ui.label("Saturation");
        changed |= ui
            .add(
                DragValue::new(&mut wheel.saturation)
                    .range(0.0..=100.0)
                    .speed(1.0)
                    .fixed_decimals(0),
            )
            .changed();
    });
    wheel.hue = wheel.hue.rem_euclid(360.0);
    wheel.saturation = wheel.saturation.clamp(0.0, 100.0);

    changed |= adjustment_slider(
        ui,
        "Luminance",
        &mut wheel.luminance,
        -100.0..=100.0,
        0,
        1.0,
        Some("Applies a hue-preserving scene-linear exposure gain to this tonal range."),
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
