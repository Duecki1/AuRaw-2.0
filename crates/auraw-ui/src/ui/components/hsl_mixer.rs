use crate::app::HslMixerColor;
use crate::pipeline::HSL_HUE_LIMIT;
use crate::ui::components::adjustment_slider::{
    accented_gradient_adjustment_slider, SliderGradient,
};
use eframe::egui::{
    self, Align, Align2, Color32, FontId, Layout, RichText, Sense, Stroke, StrokeKind, Ui,
};

use crate::ui::theme::HSL_CHANNELS as CHANNELS;

const SELECTOR_GAP: f32 = 4.0;
const SELECTOR_HEIGHT: f32 = 30.0;

pub(crate) fn hsl_mixer(
    ui: &mut Ui,
    selected: &mut HslMixerColor,
    hue: &mut [f32; 8],
    saturation: &mut [f32; 8],
    luminance: &mut [f32; 8],
) -> bool {
    let mut changed = false;

    ui.label(
        RichText::new("Color range")
            .size(11.5)
            .color(ui.visuals().weak_text_color()),
    );

    let (selector_width, selector_gap) = selector_geometry(ui.available_width());
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = selector_gap;
        for (index, (name, accent)) in CHANNELS.iter().enumerate() {
            if color_selector_button(ui, name, *accent, selected.index() == index, selector_width)
                .clicked()
            {
                *selected = HslMixerColor::ALL[index];
            }
        }
    });

    let index = selected.index();
    let (name, accent) = CHANNELS[index];
    let mut reset_color = false;

    ui.add_space(4.0);
    crate::ui::theme::toolbar_row(ui, |ui| {
        ui.label(RichText::new(format!("{name} adjustments")).strong());
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            reset_color = crate::ui::icons::phosphor_icon_button(
                ui,
                egui_phosphor::regular::ARROW_COUNTER_CLOCKWISE,
                crate::ui::theme::toolbar_icon_size(),
                "Reset Hue, Saturation, and Luminance for this color",
            )
            .clicked();
        });
    });

    if reset_color
        && (hue[index].abs() > f32::EPSILON
            || saturation[index].abs() > f32::EPSILON
            || luminance[index].abs() > f32::EPSILON)
    {
        hue[index] = 0.0;
        saturation[index] = 0.0;
        luminance[index] = 0.0;
        changed = true;
    }

    ui.push_id(("hsl-color", index), |ui| {
        changed |= accented_gradient_adjustment_slider(
            ui,
            "Hue",
            &mut hue[index],
            -HSL_HUE_LIMIT..=HSL_HUE_LIMIT,
            0,
            1.0,
            Some("Shift this color range toward neighboring hues."),
            accent,
            SliderGradient::ChannelHue {
                left: CHANNELS[(index + CHANNELS.len() - 2) % CHANNELS.len()].1,
                center: accent,
                right: CHANNELS[(index + 2) % CHANNELS.len()].1,
            },
        );
        changed |= accented_gradient_adjustment_slider(
            ui,
            "Saturation",
            &mut saturation[index],
            -100.0..=100.0,
            0,
            1.0,
            Some("Increase or reduce this color range's intensity."),
            accent,
            SliderGradient::Saturation(accent),
        );
        changed |= accented_gradient_adjustment_slider(
            ui,
            "Luminance",
            &mut luminance[index],
            -100.0..=100.0,
            0,
            1.0,
            Some("Brighten or darken this color range."),
            accent,
            SliderGradient::Luminance(accent),
        );
    });

    changed
}

fn selector_geometry(available_width: f32) -> (f32, f32) {
    let available_width = available_width.max(0.0);
    let gap = SELECTOR_GAP.min(available_width / 14.0);
    let width = (available_width - gap * 7.0) / 8.0;
    (width.max(0.0), gap)
}

fn color_selector_button(
    ui: &mut Ui,
    name: &str,
    accent: Color32,
    selected: bool,
    width: f32,
) -> egui::Response {
    let (rect, response) =
        ui.allocate_exact_size(egui::vec2(width, SELECTOR_HEIGHT), Sense::click());
    let visuals = if response.is_pointer_button_down_on() {
        ui.visuals().widgets.active
    } else if response.hovered() || response.has_focus() {
        ui.visuals().widgets.hovered
    } else {
        ui.visuals().widgets.inactive
    };
    let fill = if selected {
        ui.visuals().selection.bg_fill
    } else {
        visuals.weak_bg_fill
    };
    let stroke = if selected {
        Stroke::new(2.0, accent)
    } else {
        visuals.bg_stroke
    };

    ui.painter().rect_filled(rect, 4.0, fill);
    ui.painter()
        .rect_stroke(rect, 4.0, stroke, StrokeKind::Inside);

    let radius = (rect.width() * 0.25).clamp(4.0, 9.0);
    ui.painter().circle_filled(rect.center(), radius, accent);
    ui.painter().circle_stroke(
        rect.center(),
        radius,
        Stroke::new(1.0, Color32::from_white_alpha(100)),
    );
    let initial = name.chars().next().unwrap_or('?');
    let luminance = u16::from(accent.r()) * 3 + u16::from(accent.g()) * 6 + u16::from(accent.b());
    let text_color = if luminance > 1_650 {
        Color32::from_gray(35)
    } else {
        Color32::WHITE
    };
    ui.painter().text(
        rect.center(),
        Align2::CENTER_CENTER,
        initial,
        FontId::proportional(11.0),
        text_color,
    );

    response.on_hover_text(format!("Select the {name} color range"))
}

#[cfg(test)]
mod tests {
    use super::{selector_geometry, CHANNELS};
    use crate::app::HslMixerColor;

    #[test]
    fn mixer_colors_match_pipeline_channel_order() {
        assert_eq!(HslMixerColor::ALL.len(), CHANNELS.len());
        for (index, color) in HslMixerColor::ALL.iter().enumerate() {
            assert_eq!(color.index(), index);
        }
        assert_eq!(CHANNELS[0].0, "Red");
        assert_eq!(CHANNELS[7].0, "Magenta");
    }

    #[test]
    fn selector_never_requests_more_than_the_available_width() {
        for available in [0.0, 1.0, 80.0, 220.0, 320.0, 520.0] {
            let (button, gap) = selector_geometry(available);
            let requested = button * 8.0 + gap * 7.0;
            assert!(requested <= available + f32::EPSILON * 8.0);
        }
    }
}
