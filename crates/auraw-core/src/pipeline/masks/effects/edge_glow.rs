use super::params::edge_glow::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct EdgeGlowEffectSettings {
    pub amount: f32,
    pub edge_width: f32,
    pub detail: f32,
    pub glow: f32,
    pub color: [f32; 3],
}

impl Default for EdgeGlowEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            edge_width: EDGE_WIDTH.default,
            detail: DETAIL.default,
            glow: GLOW.default,
            color: COLOR.default,
        }
    }
}

impl EdgeGlowEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6
    }
}
