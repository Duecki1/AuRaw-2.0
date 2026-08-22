use super::params::tilt_shift::*;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct TiltShiftEffectSettings {
    pub amount: f32,
    pub radius: f32,
    pub center: [f32; 2],
    pub angle: f32,
    pub focus_width: f32,
    pub feather: f32,
}

impl Default for TiltShiftEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            radius: RADIUS.default,
            center: [CENTER_X.default, CENTER_Y.default],
            angle: ANGLE.default,
            focus_width: FOCUS_WIDTH.default,
            feather: FEATHER.default,
        }
    }
}

impl TiltShiftEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.radius > 1e-6
    }
}
