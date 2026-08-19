use super::params::neon::*;

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
            amount: AMOUNT.default,
            edge_width: EDGE_WIDTH.default,
            detail: DETAIL.default,
            glow: GLOW.default,
            background: BACKGROUND.default,
            color: COLOR.default,
        }
    }
}

impl NeonEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6
    }
}
