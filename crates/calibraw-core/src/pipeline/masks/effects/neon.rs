use super::params::neon::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct NeonEffectSettings {
    pub amount: f32,
    pub edge_width: f32,
    pub detail: f32,
    pub glow: f32,
    pub background: f32,
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
