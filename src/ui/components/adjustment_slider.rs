use eframe::egui::{self, Align, DragValue, Layout, RichText, Slider, Ui};
use std::ops::RangeInclusive;

const VALUE_FIELD_WIDTH: f32 = 72.0;
const HEADER_HEIGHT: f32 = 24.0;
const LABEL_SIZE: f32 = 12.5;
const CONTROL_GAP: f32 = 4.0;
const ROW_BOTTOM_SPACE: f32 = 7.0;

/// Reusable adjustment control used by the Develop sidebar and Settings.
///
/// The label is left-aligned above the slider. The editable numeric field is
/// aligned to the right on the same header row. The slider then fills the
/// complete available content width below them.
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
        let control_width = ui.available_width().max(VALUE_FIELD_WIDTH + 96.0);

        ui.vertical(|ui| {
            ui.set_width(control_width);
            ui.spacing_mut().item_spacing.y = CONTROL_GAP;

            ui.allocate_ui_with_layout(
                egui::vec2(control_width, HEADER_HEIGHT),
                Layout::left_to_right(Align::Center),
                |ui| {
                    let label_response = ui.label(RichText::new(label).size(LABEL_SIZE));
                    if let Some(text) = hover_text {
                        label_response.on_hover_text(text);
                    }

                    // Give the value editor the remaining row and pin it to the
                    // right edge. This keeps every field perfectly aligned.
                    ui.allocate_ui_with_layout(
                        ui.available_size(),
                        Layout::right_to_left(Align::Center),
                        |ui| {
                            let value_response = ui.add_sized(
                                [VALUE_FIELD_WIDTH, HEADER_HEIGHT],
                                DragValue::new(value)
                                    .range(range.clone())
                                    .speed(speed)
                                    .fixed_decimals(decimals),
                            );
                            changed |= value_response.changed();
                            if let Some(text) = hover_text {
                                value_response.on_hover_text(text);
                            }
                        },
                    );
                },
            );

            // Slider width in egui is controlled by the current spacing style,
            // not by `add_sized`. Override it locally so the track really spans
            // the full row instead of falling back to the global 180 px width.
            let slider_response = ui
                .scope(|ui| {
                    ui.spacing_mut().slider_width = control_width;
                    ui.add(Slider::new(value, range).show_value(false))
                })
                .inner;
            changed |= slider_response.changed();
            if let Some(text) = hover_text {
                slider_response.on_hover_text(text);
            }

            ui.add_space(ROW_BOTTOM_SPACE);
        });
    });

    changed
}
