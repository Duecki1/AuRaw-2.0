use eframe::egui::Vec2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScreenLayout {
    Horizontal,
    Vertical,
}

impl ScreenLayout {
    pub fn from_size(size: Vec2) -> Self {
        if size.x >= size.y {
            Self::Horizontal
        } else {
            Self::Vertical
        }
    }

    pub fn is_vertical(self) -> bool {
        matches!(self, Self::Vertical)
    }

    pub fn sidebar_default_size(self, viewport: Vec2) -> f32 {
        match self {
            Self::Horizontal => (viewport.x * 0.28).clamp(280.0, 420.0),
            Self::Vertical => (viewport.y * 0.34).clamp(240.0, 440.0),
        }
    }
}
