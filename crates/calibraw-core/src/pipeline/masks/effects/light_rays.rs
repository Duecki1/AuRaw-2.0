use super::params::light_rays::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct LightRaysEffectSettings {
    pub amount: f32,
    pub length: f32,
    pub source: [f32; 2],
    pub spread: f32,
    pub fade: f32,
    pub ray_count: f32,
    pub variation: f32,
    pub softness: f32,
    pub color: [f32; 3],
}

impl Default for LightRaysEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            length: LENGTH.default,
            source: [SOURCE_X.default, SOURCE_Y.default],
            spread: SPREAD.default,
            fade: FADE.default,
            ray_count: RAY_COUNT.default,
            variation: VARIATION.default,
            softness: SOFTNESS.default,
            color: COLOR.default,
        }
    }
}

impl LightRaysEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.length > 1e-6
    }
}
