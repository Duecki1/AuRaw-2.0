use super::params::radial_blur::*;

/// Direction used by the Radial Blur gather around its center point.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum RadialBlurMode {
    /// Samples toward and away from the center to create a zoom burst.
    #[default]
    Zoom,
    /// Samples along the tangent around the center to create a spin blur.
    Spin,
}

impl RadialBlurMode {
    pub const ALL: [Self; 2] = [Self::Zoom, Self::Spin];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Zoom => "Zoom",
            Self::Spin => "Spin",
        }
    }

    pub const fn shader_value(self) -> f32 {
        match self {
            Self::Zoom => 0.0,
            Self::Spin => 1.0,
        }
    }
}

/// Editable parameters for the non-destructive Radial Blur mask effect.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct RadialBlurEffectSettings {
    /// Blend between the developed source and the radial-blurred result.
    pub amount: f32,
    /// Maximum trail distance in reference-image pixels.
    pub strength: f32,
    /// Blur origin as percentages of the full image dimensions.
    pub center: [f32; 2],
    pub mode: RadialBlurMode,
}

impl Default for RadialBlurEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            strength: STRENGTH.default,
            center: [CENTER_X.default, CENTER_Y.default],
            mode: RadialBlurMode::Zoom,
        }
    }
}

impl RadialBlurEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.strength > 1e-6
    }
}
