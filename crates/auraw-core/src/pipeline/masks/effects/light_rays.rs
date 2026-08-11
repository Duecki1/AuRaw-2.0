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
            amount: 50.0,
            length: 100.0,
            source: [50.0, 35.0],
            spread: 10.0,
            fade: 45.0,
            ray_count: 32.0,
            variation: 55.0,
            softness: 40.0,
            color: [1.0, 0.85, 0.62],
        }
    }
}

impl LightRaysEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    pub fn is_active(&self) -> bool {
        self.amount.abs() > 1e-6 && self.length > 1e-6
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}
