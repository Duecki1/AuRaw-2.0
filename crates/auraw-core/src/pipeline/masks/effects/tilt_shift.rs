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
            amount: 75.0,
            radius: 16.0,
            center: [50.0, 50.0],
            angle: 0.0,
            focus_width: 24.0,
            feather: 18.0,
        }
    }
}

impl TiltShiftEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.radius > 1e-6
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
