use eframe::egui::{self, DragValue, RichText, Slider, Ui};
use std::ops::RangeInclusive;

const VALUE_FIELD_WIDTH: f32 = 66.0;
const CONTROL_HEIGHT: f32 = 22.0;
const SCROLLBAR_CLEARANCE: f32 = 8.0;

/// Compact, reusable adjustment row.
///
/// The label, slider, and editable numeric value share one row. The numeric
/// value uses `DragValue`, so it can be dragged or clicked and typed into.
pub fn adjustment_slider<Num>(
    ui: &mut Ui,
    label: &str,
    value: &mut Num,
    range: RangeInclusive<Num>,
    decimals: usize,
    speed: f64,
    hover_text: Option<&str>,
) -> bool
where
    Num: egui::emath::Numeric + Copy,
{
    let mut changed = false;

    ui.push_id(label, |ui| {
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;

            let usable_width = (ui.available_width() - SCROLLBAR_CLEARANCE)
                .max(VALUE_FIELD_WIDTH + 132.0);
            let label_width = (usable_width * 0.34).clamp(88.0, 160.0);
            let slider_width = (usable_width
                - label_width
                - VALUE_FIELD_WIDTH
                - ui.spacing().item_spacing.x * 2.0)
                .max(48.0);

            let label_response = ui.add_sized(
                [label_width, CONTROL_HEIGHT],
                egui::Label::new(
                    RichText::new(label)
                        .small()
                        .color(ui.visuals().weak_text_color()),
                ),
            );
            if let Some(text) = hover_text {
                label_response.on_hover_text(text);
            }

            let slider_response = ui.add_sized(
                [slider_width, CONTROL_HEIGHT],
                Slider::new(value, range.clone()).show_value(false),
            );
            changed |= slider_response.changed();
            if let Some(text) = hover_text {
                slider_response.on_hover_text(text);
            }

            let value_response = ui.add_sized(
                [VALUE_FIELD_WIDTH, CONTROL_HEIGHT],
                DragValue::new(value)
                    .range(range)
                    .speed(speed)
                    .fixed_decimals(decimals),
            );
            changed |= value_response.changed();
            if let Some(text) = hover_text {
                value_response.on_hover_text(text);
            }
        });
    });

    changed
}
