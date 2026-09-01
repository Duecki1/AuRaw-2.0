use super::params::motion_blur::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct MotionBlurEffectSettings {
    pub amount: f32,
    pub distance: f32,
    pub angle: f32,
}

impl Default for MotionBlurEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            distance: DISTANCE.default,
            angle: ANGLE.default,
        }
    }
}

impl MotionBlurEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.distance > 1e-6
    }
}
