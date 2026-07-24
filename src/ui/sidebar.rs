use crate::app::{
    AdjustmentSection, AurawApp, ColorGradeTab, MaskSection, SidebarTab, ToneCurveTab,
};
use crate::pipeline::{
    BrushMode, DemosaicMode, DenoiseQuality, ExportBitDepth, ExportColorProfile, ExportResizeMode,
    ExposureParams, LocalMask, MaskCombineMode, MaskComponent, MaskGeometry, MaskKind,
    SigmoidColorProcessing, CURRENT_PROCESS_VERSION, HSL_HUE_LIMIT, MAX_EXPORT_EDGE,
    MAX_LOCAL_MASKS, MAX_MASK_COMPONENTS, SCENE_DISPLAY_BOUNDARY_PROCESS_VERSION,
};
use crate::ui::components::adjustment_slider::{adjustment_slider, slider_scroll_locked};
use crate::ui::components::color_grading::color_grading_editor;
use crate::ui::components::tone_curve_editor::tone_curve_editor;
use crate::ui::layout::ScreenLayout;
use eframe::egui::{self, Ui};

pub struct Sidebar;

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
        const THIN_EDGE: f32 = 26.0;
        let card = self.card_size();
        match orientation {
            // Portrait: the strip runs left-to-right, so creation controls are
            // narrow columns that keep the full card height.
            MaskStripOrientation::Horizontal => egui::vec2(THIN_EDGE, card.y),
            // Wider screens: the strip runs top-to-bottom, so creation controls
            // become short rows that keep the full card width.
            MaskStripOrientation::Vertical => egui::vec2(card.x, THIN_EDGE),
        }
    }
}

impl Sidebar {
    const SCROLLBAR_GUTTER: f32 = 18.0;
    const MASK_THUMBNAIL_EDGE: u32 = 64;
    pub(crate) const VERTICAL_MASK_STRIP_HEIGHT: f32 = 92.0;
    pub(crate) const HORIZONTAL_MASK_STRIP_WIDTH: f32 = 92.0;
}

include!("sidebar/navigation.rs");
include!("sidebar/masks.rs");
include!("sidebar/inpainting.rs");
include!("sidebar/export.rs");
include!("sidebar/develop.rs");
include!("sidebar/crop.rs");
