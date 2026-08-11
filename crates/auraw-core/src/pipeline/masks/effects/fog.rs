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
            amount: 50.0,
            density: 55.0,
            scale: 65.0,
            softness: 70.0,
            variation: 45.0,
            seed: 0.0,
            color: [0.82, 0.87, 0.92],
        }
    }
}

impl FogEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.density > 1e-6
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
