/// Editable parameters for the non-destructive mask Pixelate effect.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct PixelateEffectSettings {
    /// Blend between the developed source and the pixelated result, in percent.
    pub amount: f32,
    /// Square cell size in reference-image pixels.
    pub block_size: f32,
}

impl Default for PixelateEffectSettings {
    fn default() -> Self {
        Self {
            amount: 100.0,
            block_size: 16.0,
        }
    }
}

impl PixelateEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.block_size > 1.0
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
