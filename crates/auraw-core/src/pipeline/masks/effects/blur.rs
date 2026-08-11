/// Editable parameters for the non-destructive mask Blur effect.
///
/// The developed image is sampled into a temporary GPU result and blended by
/// mask coverage. No source pixels are rewritten, so the radius and strength
/// remain fully reversible.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct BlurEffectSettings {
    /// Blend between the developed source and its blurred result, in percent.
    pub amount: f32,
    /// Blur radius in reference-image pixels.
    pub radius: f32,
}

impl Default for BlurEffectSettings {
    fn default() -> Self {
        Self {
            amount: 50.0,
            radius: 8.0,
        }
    }
}

impl BlurEffectSettings {
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
