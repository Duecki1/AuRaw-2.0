use crate::app::{
    AdjustmentSection, AurawApp, ColorGradeTab, HslMixerColor, InpaintTool, MaskSection,
    SidebarTab, ToneCurveTab,
};
use crate::pipeline::{
    BrushMode, DenoiseQuality, ExportBitDepth, ExportResizeMode, ExposureParams, LoadedRaw,
    LocalMask, MaskCombineMode, MaskComponent, MaskEffect, MaskEffectCategory, MaskGeometry,
    MaskKind, RetouchAlignment, MAX_LOCAL_MASKS, MAX_MASK_COMPONENTS, MAX_WHITE_BALANCE_TINT,
    MIN_WHITE_BALANCE_TINT,
};
use crate::ui::components::adjustment_slider::{
    adjustment_slider, adjustment_slider_with_reset, gradient_adjustment_slider,
    hue_adjustment_slider, slider_scroll_locked, SliderGradient,
};
use crate::ui::components::color_grading::color_grading_editor;
use crate::ui::components::hsl_mixer::hsl_mixer;
use crate::ui::components::tone_curve_editor::{tone_curve_channel_editor, ToneCurveChannels};
use crate::ui::layout::ScreenLayout;
use eframe::egui::{self, Ui};

pub(crate) struct Sidebar;

mod mask_effects;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaskCardSize {
    Group,
    Submask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaskStripOrientation {
    Horizontal,
    Vertical,
}

impl MaskCardSize {
    fn card_size(self) -> egui::Vec2 {
        match self {
            Self::Group => egui::vec2(68.0, 72.0),
            Self::Submask => egui::vec2(56.0, 62.0),
        }
    }

    fn image_edge(self) -> f32 {
        match self {
            Self::Group => 54.0,
            Self::Submask => 44.0,
        }
    }

    fn label_font_size(self) -> f32 {
        match self {
            Self::Group => 9.5,
            Self::Submask => 8.5,
        }
    }

    fn create_button_size(self, orientation: MaskStripOrientation) -> egui::Vec2 {
        const THIN_EDGE: f32 = crate::ui::theme::CONTROL_HEIGHT;
        let card = self.card_size();
        match orientation {
            MaskStripOrientation::Horizontal => egui::vec2(THIN_EDGE, card.y),
            MaskStripOrientation::Vertical => egui::vec2(card.x, THIN_EDGE),
        }
    }
}

impl Sidebar {
    #[cfg(not(target_os = "android"))]
    pub(crate) const DESKTOP_TOOL_RAIL_WIDTH: f32 = 60.0;
    #[cfg(target_os = "android")]
    pub(crate) const ANDROID_LANDSCAPE_TOOL_RAIL_WIDTH: f32 = 72.0;
    const MASK_THUMBNAIL_EDGE: u32 = 64;
    pub(crate) const VERTICAL_MASK_STRIP_HEIGHT: f32 = 92.0;
    pub(crate) const HORIZONTAL_MASK_STRIP_WIDTH: f32 = 92.0;
    const CONTEXT_TAB_WIDTH: f32 = 64.0;
}

include!("sidebar/navigation.rs");
mod masks;
include!("sidebar/inpainting.rs");
include!("sidebar/export.rs");
include!("sidebar/develop.rs");
include!("sidebar/crop.rs");

#[cfg(test)]
mod tests {
    use super::masks::{mask_component_badge, mask_creation_icon};
    use super::{
        mobile_tab_icon_geometry, mobile_tab_text_geometry, MaskCardSize, MaskCombineMode,
        MaskStripOrientation,
    };

    #[test]
    fn mask_badges_match_base_and_combine_semantics() {
        assert_eq!(mask_component_badge(0, MaskCombineMode::Subtract), "BASE");
        assert_eq!(
            mask_component_badge(1, MaskCombineMode::Add),
            egui_phosphor::regular::PLUS
        );
        assert_eq!(
            mask_component_badge(1, MaskCombineMode::Subtract),
            egui_phosphor::regular::MINUS
        );
        assert_eq!(
            mask_component_badge(1, MaskCombineMode::Intersect),
            egui_phosphor::regular::INTERSECT
        );
    }

    #[test]
    fn mask_creation_controls_use_a_compact_plus_icon() {
        assert_eq!(mask_creation_icon(), egui_phosphor::regular::PLUS);
    }

    #[test]
    fn mask_creation_controls_are_thin_along_the_strip_axis() {
        assert_eq!(
            MaskCardSize::Group.create_button_size(MaskStripOrientation::Horizontal),
            eframe::egui::vec2(crate::ui::theme::CONTROL_HEIGHT, 72.0)
        );
        assert_eq!(
            MaskCardSize::Submask.create_button_size(MaskStripOrientation::Horizontal),
            eframe::egui::vec2(crate::ui::theme::CONTROL_HEIGHT, 62.0)
        );
        assert_eq!(
            MaskCardSize::Group.create_button_size(MaskStripOrientation::Vertical),
            eframe::egui::vec2(68.0, crate::ui::theme::CONTROL_HEIGHT)
        );
        assert_eq!(
            MaskCardSize::Submask.create_button_size(MaskStripOrientation::Vertical),
            eframe::egui::vec2(56.0, crate::ui::theme::CONTROL_HEIGHT)
        );
    }

    #[test]
    fn mobile_tab_icon_and_label_stack_is_vertically_centered() {
        for height in [44.0, 48.0, 52.0, 56.0] {
            let (icon_size, label_size, icon_center, label_center) =
                mobile_tab_text_geometry(height);
            let stack_top = icon_center - icon_size * 0.5;
            let stack_bottom = label_center + label_size * 0.5;
            assert!(((stack_top + stack_bottom) * 0.5 - height * 0.5).abs() < 0.001);
            assert!(icon_center < label_center);
        }
    }

    #[test]
    fn unlabeled_mobile_tab_icon_is_centered() {
        for height in [44.0, 48.0, 52.0, 56.0] {
            let (icon_size, icon_center) = mobile_tab_icon_geometry(height, false);
            assert!((icon_center - height * 0.5).abs() < 0.001);
            assert!((21.0..=25.0).contains(&icon_size));
        }
    }
}
