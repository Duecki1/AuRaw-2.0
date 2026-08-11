/// Editable parameters for the non-destructive mask Edge Glow effect.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct EdgeGlowEffectSettings {
    /// Overall emitted edge-light strength, in percent.
    pub amount: f32,
    /// Edge sampling width in reference-image pixels.
    pub edge_width: f32,
    /// Fine-edge sensitivity, in percent.
    pub detail: f32,
    /// Strength of the broader edge halo, in percent.
    pub glow: f32,
    /// Emitted edge color in encoded sRGB, matching the color picker.
    pub color: [f32; 3],
}

impl Default for EdgeGlowEffectSettings {
    fn default() -> Self {
        Self {
            amount: 50.0,
            edge_width: 1.5,
            detail: 35.0,
            glow: 55.0,
            color: [1.0, 0.42, 0.08],
        }
    }
}

impl EdgeGlowEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
