use super::params::glow::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct GlowEffectSettings {
    pub amount: f32,
    pub radius: f32,
    pub core: f32,
    pub color: [f32; 3],
}

impl Default for GlowEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            radius: RADIUS.default,
            core: CORE.default,
            color: COLOR.default,
        }
    }
}

impl GlowEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6
    }
}
