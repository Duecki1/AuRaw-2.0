use eframe::egui::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenLayout {
    Horizontal,
    Vertical,
}

impl ScreenLayout {
    pub const MIN_HORIZONTAL_SIDEBAR_WIDTH: f32 = 320.0;
    pub const MIN_VERTICAL_SIDEBAR_HEIGHT: f32 = 240.0;

    pub fn from_size(size: Vec2) -> Self {
        if size.x >= size.y {
            Self::Horizontal
        } else {
            Self::Vertical
        }
    }

    pub fn sidebar_default_size(self, viewport: Vec2) -> f32 {
        match self {
            Self::Horizontal => {
                (viewport.x * 0.28).clamp(Self::MIN_HORIZONTAL_SIDEBAR_WIDTH, 460.0)
            }
            Self::Vertical => (viewport.y * 0.34).clamp(Self::MIN_VERTICAL_SIDEBAR_HEIGHT, 480.0),
        }
    }
}