#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HighlightReconstructionMethod {
    Off,
    Lch,
    #[default]
    Guided,
}

impl HighlightReconstructionMethod {
    pub(crate) const fn shader_value(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Lch => 1.0,
            Self::Guided => 2.0,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExposureParams {
    /// Sensor-space black-point calibration. This is deliberately separate from
    /// the creative `blacks` control in the Basic panel.
    pub black_point: f32,
    pub exposure: f32,
    pub hlcompr: f32,
    pub hlcomprthresh: f32,
    pub contrast: f32,
    pub middle_grey: f32,
    pub brightness: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub chroma_denoise: f32,
    pub ca_red: f32,
    pub ca_blue: f32,
    /// Reconstruction algorithm. The guided method ports Ansel's
    /// interpolate/mask/remosaic design and is the high-quality default.
    pub highlight_method: HighlightReconstructionMethod,
    /// Raw highlight-clipping threshold used by reconstruction. This scales
    /// Ansel's shared post-white-balance clipping level.
    pub highlight_clip: f32,
    /// Raw highlight-reconstruction blend/strength.
    pub highlight_reconstruction: f32,
    /// Number of progressively wider guided chroma-propagation passes.
    pub highlight_iterations: u32,
    /// How strongly surrounding highlight colour is retained instead of
    /// converging toward a neutral specular highlight.
    pub highlight_color_adaptation: f32,

    // Lightroom-style tonal and local-contrast controls. These intentionally
    // use the familiar -100..100 UI scale rather than raw pipeline units.
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,

    // Red, orange, yellow, green, aqua, blue, purple, magenta.
    pub hsl_hue: [f32; 8],
    pub hsl_saturation: [f32; 8],
    pub hsl_luminance: [f32; 8],

    pub filmic_white: f32,
    pub filmic_black: f32,
}

impl Default for ExposureParams {
    fn default() -> Self {
        Self {
            black_point: 0.0,
            exposure: 0.0,
            hlcompr: 0.0,
            hlcomprthresh: 0.0,
            contrast: 0.0,
            middle_grey: 18.42,
            brightness: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            chroma_denoise: 0.0,
            ca_red: 0.0,
            ca_blue: 0.0,
            highlight_method: HighlightReconstructionMethod::Guided,
            highlight_clip: 1.0,
            highlight_reconstruction: 1.0,
            highlight_iterations: 3,
            highlight_color_adaptation: 0.75,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            hsl_hue: [0.0; 8],
            hsl_saturation: [0.0; 8],
            hsl_luminance: [0.0; 8],
            filmic_white: 4.0,
            filmic_black: -8.0,
        }
    }
}
