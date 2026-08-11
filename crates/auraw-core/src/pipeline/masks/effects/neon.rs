/// Editable Neon parameters. Values use UI-friendly units and are clamped
/// again when packed for the GPU, keeping malformed sidecars away from shader
/// math while preserving a fully non-destructive edit model.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct NeonEffectSettings {
    /// Overall edge emission strength, in percent.
    pub amount: f32,
    /// Edge sampling radius in reference-image pixels.
    pub edge_width: f32,
    /// Fine-edge sensitivity, in percent.
    pub detail: f32,
    /// Broader halo contribution, in percent.
    pub glow: f32,
    /// Amount of the source image retained behind the neon lines, in percent.
    pub background: f32,
    /// Neon color in encoded sRGB, matching the color picker.
    pub color: [f32; 3],
}

impl Default for NeonEffectSettings {
    fn default() -> Self {
        Self {
            amount: 50.0,
            edge_width: 1.0,
            detail: 10.0,
            glow: 10.0,
            background: 50.0,
            color: [0.05, 0.85, 1.0],
        }
    }
}

impl NeonEffectSettings {
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
