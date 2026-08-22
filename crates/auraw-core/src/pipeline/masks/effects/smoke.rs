use super::params::smoke::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct SmokeEffectSettings {
    pub amount: f32,
    pub density: f32,
    pub scale: f32,
    pub turbulence: f32,
    pub softness: f32,
    pub angle: f32,
    pub seed: f32,
    pub color: [f32; 3],
}

impl Default for SmokeEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            density: DENSITY.default,
            scale: SCALE.default,
            turbulence: TURBULENCE.default,
            softness: SOFTNESS.default,
            angle: ANGLE.default,
            seed: SEED.default,
            color: COLOR.default,
        }
    }
}

impl SmokeEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.density > 1e-6
    }
}
