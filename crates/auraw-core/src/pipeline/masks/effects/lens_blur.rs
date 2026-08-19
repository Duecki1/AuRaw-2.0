use super::params::lens_blur::*;

/// Editable parameters for the non-destructive Lens Blur mask effect.
///
/// The aperture-shaped gather is evaluated from the developed image on the
/// GPU and blended through mask coverage. Source pixels are never rewritten.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct LensBlurEffectSettings {
    /// Blend between the developed source and the lens-blurred result.
    pub amount: f32,
    /// Aperture radius in reference-image pixels.
    pub radius: f32,
    /// Number of sides in the simulated aperture.
    pub blades: f32,
    /// Aperture rotation in degrees.
    pub rotation: f32,
    /// Extra weighting given to bright samples, in percent.
    pub highlight_boost: f32,
}

impl Default for LensBlurEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            radius: RADIUS.default,
            blades: BLADES.default,
            rotation: ROTATION.default,
            highlight_boost: HIGHLIGHTS.default,
        }
    }
}

impl LensBlurEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.radius > 1e-6
    }
}
