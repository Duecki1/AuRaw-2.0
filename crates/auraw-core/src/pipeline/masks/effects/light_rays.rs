use super::params::light_rays::*;

/// Editable parameters for the non-destructive mask Light Rays effect.
///
/// Mask coverage acts as an emitter rather than a final compositing boundary.
/// Shafts converge on `source` and extend beyond the mask in every radial
/// direction, which is the geometry expected from crepuscular or "god" rays.
/// All distances are image-relative so fit previews, zoom crops, and exports
/// describe the same effect.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(default)]
pub struct LightRaysEffectSettings {
    /// Overall emitted-light strength, in percent.
    pub amount: f32,
    /// Shaft reach as a percentage of the image's shorter edge.
    pub length: f32,
    /// Full-image source point in percent. Values outside 0..100 place the
    /// light just beyond the frame, which is useful for window or sun shafts.
    pub source: [f32; 2],
    /// Angular fan width, in degrees.
    pub spread: f32,
    /// How quickly the shaft energy falls off with distance, in percent.
    pub fade: f32,
    /// Approximate number of broad shafts around the source.
    pub ray_count: f32,
    /// Strength of deterministic angular variation, in percent.
    pub variation: f32,
    /// Softness of shaft edges and source sampling, in percent.
    pub softness: f32,
    /// Emitted light color in encoded sRGB, matching the color picker.
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
