use super::params::fog::*;

/// Editable parameters for the non-destructive mask Fog effect.
///
/// The procedural field is anchored in full-image coordinates, so previews,
/// zoomed views, and tiled exports all reproduce the same atmosphere. Mask
/// coverage remains a separate, editable compositing boundary.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct FogEffectSettings {
    /// Overall blend strength, in percent.
    pub amount: f32,
    /// Optical density of the generated veil, in percent.
    pub density: f32,
    /// Size of the broad fog banks, in percent.
    pub scale: f32,
    /// Smoothness of transitions between clear and foggy regions, in percent.
    pub softness: f32,
    /// Strength of the procedural density variation, in percent.
    pub variation: f32,
    /// Deterministic pattern offset.
    pub seed: f32,
    /// Fog color in encoded sRGB, matching the color picker.
    pub color: [f32; 3],
}

impl Default for FogEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            density: DENSITY.default,
            scale: SCALE.default,
            softness: SOFTNESS.default,
            variation: VARIATION.default,
            seed: SEED.default,
            color: COLOR.default,
        }
    }
}

impl FogEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.density > 1e-6
    }
}
