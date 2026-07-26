//! Responsive layout decisions shared by every top-level screen.
//!
//! Keep breakpoint policy here instead of scattering window-size checks through
//! Library, Develop, and Settings. Individual screens can still choose their
//! own content, but they all receive the same predictable shell.

use eframe::egui::Vec2;

use super::layout::ScreenLayout;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LayoutProfile {
    Expanded,
    Compact,
    TouchPortrait,
    ShortLandscape,
}

impl LayoutProfile {
    pub fn from_viewport(viewport: Vec2, is_touch_platform: bool) -> Self {
        let portrait = viewport.y > viewport.x;
        if is_touch_platform && portrait {
            Self::TouchPortrait
        } else if viewport.y < 540.0 || viewport.x < 840.0 {
            // A phone in landscape needs its image canvas first; a full-width
            // inspector would make editing impractical.
            Self::ShortLandscape
        } else if viewport.x >= 1_180.0 && viewport.y >= 680.0 {
            Self::Expanded
        } else if portrait {
            Self::TouchPortrait
        } else {
            Self::Compact
        }
    }

    pub const fn screen_layout(self) -> ScreenLayout {
        match self {
            Self::Expanded | Self::Compact => ScreenLayout::Horizontal,
            Self::TouchPortrait | Self::ShortLandscape => ScreenLayout::Vertical,
        }
    }

    pub const fn is_compact(self) -> bool {
        matches!(self, Self::Compact | Self::ShortLandscape | Self::TouchPortrait)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chooses_canvas_first_layouts_for_small_screens() {
        assert_eq!(
            LayoutProfile::from_viewport(Vec2::new(760.0, 360.0), true),
            LayoutProfile::ShortLandscape
        );
        assert_eq!(
            LayoutProfile::from_viewport(Vec2::new(390.0, 844.0), true),
            LayoutProfile::TouchPortrait
        );
    }
}
