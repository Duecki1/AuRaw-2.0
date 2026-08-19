use super::params::glow::*;

/// Editable parameters for the non-destructive mask Glow effect.
///
/// The mask supplies emission only: its coverage defines the bright core and
/// the source that is diffused into surrounding pixels. The resulting halo is
/// deliberately composited without applying the mask a second time.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct GlowEffectSettings {
    /// Overall core and halo strength, in percent.
    pub amount: f32,
    /// Halo diffusion radius, in percent of the available multi-scale range.
    pub radius: f32,
    /// Brightness and whiteness of the source core, in percent.
    pub core: f32,
    /// Emitted halo color in encoded sRGB, matching the color picker.
    pub color: [f32; 3],
}

impl Default for GlowEffectSettings {
    fn default() -> Self {
        Self {
            amount: AMOUNT.default,
            radius: RADIUS.default,
            core: CORE.default,
            color: COLOR.default,
        }
    }
}

impl GlowEffectSettings {
    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6
    }
}
