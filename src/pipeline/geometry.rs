use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CropAspectRatio {
    #[default]
    Free,
    Original,
    Square,
    FourThree,
    ThreeFour,
    ThreeTwo,
    TwoThree,
    SixteenNine,
    NineSixteen,
}

impl CropAspectRatio {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Free => "Free",
            Self::Original => "Original",
            Self::Square => "1 × 1",
            Self::FourThree => "4 × 3",
            Self::ThreeFour => "3 × 4",
            Self::ThreeTwo => "3 × 2",
            Self::TwoThree => "2 × 3",
            Self::SixteenNine => "16 × 9",
            Self::NineSixteen => "9 × 16",
        }
    }

    pub fn value(self, source_width: u32, source_height: u32) -> Option<f32> {
        match self {
            Self::Free => None,
            Self::Original => Some(source_width.max(1) as f32 / source_height.max(1) as f32),
            Self::Square => Some(1.0),
            Self::FourThree => Some(4.0 / 3.0),
            Self::ThreeFour => Some(3.0 / 4.0),
            Self::ThreeTwo => Some(3.0 / 2.0),
            Self::TwoThree => Some(2.0 / 3.0),
            Self::SixteenNine => Some(16.0 / 9.0),
            Self::NineSixteen => Some(9.0 / 16.0),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
pub struct GeometryTransform {
    /// Normalized crop rectangle in full-image coordinates: left, top, right, bottom.
    #[serde(default = "default_crop_rect")]
    pub crop: [f32; 4],
    #[serde(default)]
    pub aspect_ratio: CropAspectRatio,
    /// Clockwise quarter-turns, stored separately from fine straighten.
    #[serde(default)]
    pub quarter_turns: u8,
    /// Clockwise straighten angle in degrees.
    #[serde(default)]
    pub rotation_degrees: f32,
    #[serde(default)]
    pub flip_horizontal: bool,
    #[serde(default)]
    pub flip_vertical: bool,
    /// Affine keystone/shear correction in degrees.
    #[serde(default)]
    pub horizontal_transform: f32,
    #[serde(default)]
    pub vertical_transform: f32,
}

const fn default_crop_rect() -> [f32; 4] {
    [0.0, 0.0, 1.0, 1.0]
}

impl Default for GeometryTransform {
    fn default() -> Self {
        Self {
            crop: default_crop_rect(),
            aspect_ratio: CropAspectRatio::Free,
            quarter_turns: 0,
            rotation_degrees: 0.0,
            flip_horizontal: false,
            flip_vertical: false,
            horizontal_transform: 0.0,
            vertical_transform: 0.0,
        }
    }
}

impl GeometryTransform {
    pub const MIN_CROP_EXTENT: f32 = 0.01;

    pub fn sanitized(mut self) -> Self {
        for value in &mut self.crop {
            if !value.is_finite() {
                *value = 0.0;
            }
        }
        self.crop[0] = self.crop[0].clamp(0.0, 1.0 - Self::MIN_CROP_EXTENT);
        self.crop[1] = self.crop[1].clamp(0.0, 1.0 - Self::MIN_CROP_EXTENT);
        self.crop[2] = self.crop[2].clamp(self.crop[0] + Self::MIN_CROP_EXTENT, 1.0);
        self.crop[3] = self.crop[3].clamp(self.crop[1] + Self::MIN_CROP_EXTENT, 1.0);
        self.quarter_turns %= 4;
        self.rotation_degrees = finite_clamp(self.rotation_degrees, -45.0, 45.0);
        self.horizontal_transform = finite_clamp(self.horizontal_transform, -30.0, 30.0);
        self.vertical_transform = finite_clamp(self.vertical_transform, -30.0, 30.0);
        self
    }

    pub fn is_identity(self) -> bool {
        let value = self.sanitized();
        value.crop == default_crop_rect()
            && value.quarter_turns == 0
            && value.rotation_degrees.abs() < 1e-4
            && !value.flip_horizontal
            && !value.flip_vertical
            && value.horizontal_transform.abs() < 1e-4
            && value.vertical_transform.abs() < 1e-4
    }

    pub fn crop_pixel_dimensions(self, source_width: u32, source_height: u32) -> (u32, u32) {
        let value = self.sanitized();
        let width = ((value.crop[2] - value.crop[0]) * source_width.max(1) as f32)
            .round()
            .max(1.0) as u32;
        let height = ((value.crop[3] - value.crop[1]) * source_height.max(1) as f32)
            .round()
            .max(1.0) as u32;
        if value.quarter_turns % 2 == 0 {
            (width, height)
        } else {
            (height, width)
        }
    }

    pub fn set_full_crop(&mut self) {
        self.crop = default_crop_rect();
    }

    pub fn rotate_quarter_turn(&mut self, clockwise: bool) {
        self.quarter_turns = if clockwise {
            (self.quarter_turns + 1) % 4
        } else {
            (self.quarter_turns + 3) % 4
        };
    }
}

fn finite_clamp(value: f32, min: f32, max: f32) -> f32 {
    if value.is_finite() {
        value.clamp(min, max)
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn geometry_defaults_to_identity() {
        assert!(GeometryTransform::default().is_identity());
    }

    #[test]
    fn crop_dimensions_follow_normalized_rectangle() {
        let geometry = GeometryTransform {
            crop: [0.25, 0.25, 0.75, 0.75],
            ..Default::default()
        };
        assert_eq!(geometry.crop_pixel_dimensions(4000, 3000), (2000, 1500));
    }
}
