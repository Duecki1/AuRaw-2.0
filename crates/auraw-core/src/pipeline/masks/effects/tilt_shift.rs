use super::params::tilt_shift::*;

/// Editable parameters for the non-destructive Tilt-Shift mask effect.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct TiltShiftEffectSettings {
    /// Maximum blend between the developed source and blurred result.
    pub amount: f32,
    /// Defocus radius in reference-image pixels.
    pub radius: f32,
    /// A point on the in-focus line, in full-image percentages.
    pub center: [f32; 2],
    /// Direction of the in-focus band in degrees.
    pub angle: f32,
    /// Width of the sharp band as a percentage of the image's shorter edge.
    pub focus_width: f32,
    /// Width of each focus transition as a percentage of the shorter edge.
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
