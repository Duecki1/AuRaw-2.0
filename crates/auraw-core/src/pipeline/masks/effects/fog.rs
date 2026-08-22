use super::params::fog::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct FogEffectSettings {
    pub amount: f32,
    pub density: f32,
    pub scale: f32,
    pub softness: f32,
    pub variation: f32,
    pub seed: f32,
    pub color: [f32; 3],
}

impl Default for FogEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            density: DENSITY.default,
            scale: SCALE.default,
            softness: SOFTNESS.default,
            variation: VARIATION.default,
            seed: SEED.default,
            color: COLOR.default,
        }
    }
}

impl FogEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.density > 1e-6
    }
}
