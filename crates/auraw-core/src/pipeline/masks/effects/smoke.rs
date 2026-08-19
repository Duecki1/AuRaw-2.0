use super::params::smoke::*;

/// Editable parameters for the non-destructive mask Smoke effect.
///
/// Smoke is generated procedurally from full-image coordinates and blended
/// through the mask at render time. No source pixels or mask rasters are
/// modified when these controls change.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct SmokeEffectSettings {
    /// Overall blend strength, in percent.
    pub amount: f32,
    /// Opacity of the generated smoke, in percent.
    pub density: f32,
    /// Size of the smoke plumes, in percent.
    pub scale: f32,
    /// Strength of the domain-warped curls, in percent.
    pub turbulence: f32,
    /// Smoothness of the plume boundaries, in percent.
    pub softness: f32,
    /// Plume orientation, in degrees.
    pub angle: f32,
    /// Deterministic pattern offset.
    pub seed: f32,
    /// Smoke color in encoded sRGB, matching the color picker.
    pub color: [f32; 3],
}

impl Default for SmokeEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            density: DENSITY.default,
            scale: SCALE.default,
            turbulence: TURBULENCE.default,
            softness: SOFTNESS.default,
            angle: ANGLE.default,
            seed: SEED.default,
            color: COLOR.default,
        }
    }
}

impl SmokeEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.density > 1e-6
    }
}
