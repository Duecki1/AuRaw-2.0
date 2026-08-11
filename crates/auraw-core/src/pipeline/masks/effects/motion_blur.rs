/// Editable parameters for the non-destructive Motion Blur mask effect.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct MotionBlurEffectSettings {
    /// Blend between the developed source and the motion-blurred result.
    pub amount: f32,
    /// Total shutter-trail distance in reference-image pixels.
    pub distance: f32,
    /// Direction of travel in degrees.
    pub angle: f32,
}

impl Default for MotionBlurEffectSettings {
    fn default() -> Self {
        Self {
            amount: 50.0,
            distance: 32.0,
            angle: 0.0,
        }
    }
}

impl MotionBlurEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.distance > 1e-6
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
