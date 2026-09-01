use super::params::blur::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct BlurEffectSettings {
    pub amount: f32,
    pub radius: f32,
}

impl Default for BlurEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            radius: RADIUS.default,
        }
    }
}

impl BlurEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.radius > 1e-6
    }
}
