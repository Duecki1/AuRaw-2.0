use super::params::lens_blur::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct LensBlurEffectSettings {
    pub amount: f32,
    pub radius: f32,
    pub blades: f32,
    pub rotation: f32,
    pub highlight_boost: f32,
}

impl Default for LensBlurEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            radius: RADIUS.default,
            blades: BLADES.default,
            rotation: ROTATION.default,
            highlight_boost: HIGHLIGHTS.default,
        }
    }
}

impl LensBlurEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.radius > 1e-6
    }
}
