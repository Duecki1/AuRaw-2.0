use super::sigmoid::SigmoidParams;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum DemosaicMode {
    /// Reference high-detail demosaic: RCD for Bayer and Markesteijn 3-pass
    /// for Fuji X-Trans.
    #[default]
    Reference,
    /// Apply frequency-domain chroma suppression to the reference result.
    FrequencyDomainChroma,
    /// Blend the high-detail result with a low-detail VNG-style result using
    /// a Scharr/detail mask, following darktable's dual-demosaic behaviour.
    Dual,
}

impl DemosaicMode {
    pub(crate) const fn shader_value(self) -> f32 {
        match self {
            Self::Reference => 0.0,
            Self::FrequencyDomainChroma => 1.0,
            Self::Dual => 2.0,
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Reference => "Reference (RCD / Markesteijn 3-pass)",
            Self::FrequencyDomainChroma => "Frequency-domain chroma",
            Self::Dual => "Dual demosaic",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
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

/// Lightroom-style editable point curve. Points are stored in normalized
/// input/output coordinates and evaluated in a reversible scene-luminance
/// shaper, so the neutral diagonal is an exact no-op for HDR scene values.
pub const MAX_POINT_CURVE_POINTS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct PointCurve {
    pub points: [[f32; 2]; MAX_POINT_CURVE_POINTS],
    pub len: u32,
}

impl PointCurve {
    pub const fn linear() -> Self {
        // Lightroom's Point Curve starts with only the two locked endpoints.
        // Interior control points are created explicitly by the user.
        Self {
            points: [
                [0.0, 0.0],
                [1.0, 1.0],
                [1.0, 1.0],
                [1.0, 1.0],
                [1.0, 1.0],
                [1.0, 1.0],
                [1.0, 1.0],
                [1.0, 1.0],
            ],
            len: 2,
        }
    }

    pub fn reset(&mut self) {
        *self = Self::linear();
    }

    pub fn is_identity(&self) -> bool {
        let len = self.len.clamp(2, MAX_POINT_CURVE_POINTS as u32) as usize;
        self.points[..len]
            .iter()
            .all(|point| (point[1] - point[0]).abs() <= 1e-6)
    }

    pub fn sanitize(&mut self) {
        self.len = self.len.clamp(2, MAX_POINT_CURVE_POINTS as u32);
        let len = self.len as usize;
        self.points[0] = [0.0, self.points[0][1].clamp(0.0, 1.0)];
        self.points[len - 1] = [1.0, self.points[len - 1][1].clamp(0.0, 1.0)];
        for index in 1..len - 1 {
            let lower = self.points[index - 1][0] + 0.005;
            let remaining = (len - 1 - index) as f32;
            let upper = 1.0 - 0.005 * remaining;
            self.points[index][0] = self.points[index][0].clamp(lower, upper.max(lower));
            self.points[index][1] = self.points[index][1].clamp(0.0, 1.0);
        }
        for point in &mut self.points[len..] {
            *point = [1.0, 1.0];
        }
    }
}

impl Default for PointCurve {
    fn default() -> Self {
        Self::linear()
    }
}

/// One perceptual color-grading wheel. Hue is stored in degrees so presets
/// and numeric entry remain intuitive; saturation and luminance use the
/// familiar -/0..100 editing domains. A zero-saturation wheel is an exact
/// chromatic no-op regardless of its remembered hue.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ColorGradeWheel {
    pub hue: f32,
    pub saturation: f32,
    pub luminance: f32,
}

impl Default for ColorGradeWheel {
    fn default() -> Self {
        Self {
            hue: 0.0,
            saturation: 0.0,
            luminance: 0.0,
        }
    }
}

impl ColorGradeWheel {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_neutral(self) -> bool {
        self.saturation.abs() <= 1e-6 && self.luminance.abs() <= 1e-6
    }
}

/// Scene-referred four-way grading inspired by Lightroom Color Grading and
/// darktable color balance rgb. Tonal ranges overlap smoothly in log-luminance
/// space; `blending` controls that overlap and `balance` moves the pivot.
#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ColorGrading {
    pub shadows: ColorGradeWheel,
    pub midtones: ColorGradeWheel,
    pub highlights: ColorGradeWheel,
    pub global: ColorGradeWheel,
    pub blending: f32,
    pub balance: f32,
}

impl Default for ColorGrading {
    fn default() -> Self {
        Self {
            shadows: ColorGradeWheel::default(),
            midtones: ColorGradeWheel::default(),
            highlights: ColorGradeWheel::default(),
            global: ColorGradeWheel::default(),
            blending: 50.0,
            balance: 0.0,
        }
    }
}

impl ColorGrading {
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn is_neutral(self) -> bool {
        self.shadows.is_neutral()
            && self.midtones.is_neutral()
            && self.highlights.is_neutral()
            && self.global.is_neutral()
    }
}

pub const CURRENT_PROCESS_VERSION: u32 = 8;
/// Global camera white-balance temperature range in mired displacement.
/// +/-150 reaches roughly 2,850 K to 20,000 K around a 5,000 K as-shot neutral
/// while retaining fine one-unit control near zero.
pub const GLOBAL_TEMPERATURE_LIMIT: f32 = 150.0;
/// Extended Color Mixer hue range. Values through +/-100 retain the original
/// response for sidecar compatibility, while the extra travel allows stronger
/// creative shifts toward and beyond the neighbouring named hue.
pub const HSL_HUE_LIMIT: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExposureParams {
    /// Version of the processing formulas used by this edit. Saved edits must
    /// migrate deliberately instead of silently adopting new shader behavior.
    pub process_version: u32,
    /// Additional normalized sensor-space black-point correction. It is applied
    /// per CFA plane before white balance and demosaic, and is deliberately
    /// separate from the creative `blacks` control in the Basic panel.
    /// Values are clamped to +/-0.25 of the metadata-calibrated sensor range.
    pub black_point: f32,
    /// Scene-linear exposure in stops, applied before local/color processing.
    pub exposure: f32,
    /// Lightroom-style contrast in the -100..100 UI domain.
    pub contrast: f32,
    /// darktable-compatible sigmoid scene-to-display transform.
    pub sigmoid: SigmoidParams,
    /// Relative metadata-aware white balance. Temperature uses the extended
    /// +/-150 mired domain while tint retains the familiar -100..100 domain.
    /// Temperature is a mired displacement and tint is a Planckian-normal Duv
    /// displacement; zero preserves the camera's as-shot neutral exactly.
    pub temperature: f32,
    pub tint: f32,
    pub saturation: f32,
    pub vibrance: f32,
    /// Composite luminance curve followed by independent scene-referred RGB channel curves.
    pub tone_curve: PointCurve,
    pub tone_curve_red: PointCurve,
    pub tone_curve_green: PointCurve,
    pub tone_curve_blue: PointCurve,
    pub chroma_denoise: f32,
    /// Demosaic finishing mode. The reference algorithm is always run first.
    pub demosaic_mode: DemosaicMode,
    /// Detail threshold in darktable-compatible 0..100 units for dual mode.
    pub dual_threshold: f32,
    /// Strength of the frequency-domain chroma suppression stage.
    pub frequency_chroma: f32,
    pub ca_red: f32,
    pub ca_blue: f32,
    /// Reconstruction algorithm. The guided method ports Ansel's
    /// interpolate/mask/remosaic design and is the high-quality default.
    pub highlight_method: HighlightReconstructionMethod,
    /// Raw highlight-clipping threshold used by reconstruction. This scales
    /// Ansel's shared post-white-balance clipping level.
    pub highlight_clip: f32,
    /// Raw highlight-reconstruction strength.
    pub highlight_reconstruction: f32,
    /// Number of progressively wider guided chroma-propagation passes.
    pub highlight_iterations: u32,
    /// How strongly surrounding highlight colour is retained instead of
    /// converging toward a neutral specular highlight.
    pub highlight_color_adaptation: f32,

    // Lightroom-style tonal controls are applied as scene-linear, local
    // exposure shaping before the final darktable sigmoid display transform.
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,

    // Creative effects. Glow follows a highlight-aware, multi-scale bloom
    // model; vignette is a post-crop, exposure-domain edge treatment.
    pub glow_amount: f32,
    pub glow_radius: f32,
    pub glow_threshold: f32,
    pub vignette_amount: f32,
    pub vignette_midpoint: f32,
    pub vignette_roundness: f32,
    pub vignette_feather: f32,
    pub vignette_highlights: f32,

    // Red, orange, yellow, green, aqua, blue, purple, magenta.
    pub hsl_hue: [f32; 8],
    pub hsl_saturation: [f32; 8],
    pub hsl_luminance: [f32; 8],

    /// Perceptual four-way color grading in scene-linear Rec.2020.
    pub color_grading: ColorGrading,
}

/// Fixed backend exposure lift applied only to the global Exposure control.
/// The user-facing/global edit value is centered at 0 EV; local-mask Exposure
/// remains an unbiased relative adjustment.
pub const GLOBAL_EXPOSURE_BACKEND_OFFSET_EV: f32 = 0.7;

impl ExposureParams {
    pub fn migrate_to_current_process(&mut self) {
        // Version 8 rebases the persisted global Exposure control so 0 is the
        // neutral UI value while the historical +0.7 EV rendition lift moves
        // to the backend. Subtracting the lift here preserves the exact render
        // of older sidecars after the backend starts adding it automatically.
        match self.process_version {
            0..=7 => {
                self.exposure -= GLOBAL_EXPOSURE_BACKEND_OFFSET_EV;
                self.process_version = CURRENT_PROCESS_VERSION;
            }
            CURRENT_PROCESS_VERSION => {}
            // Preserve unknown future versions. Callers can reject them or
            // load them in a compatibility mode, but must not silently
            // reinterpret them with older formulas.
            _ => {}
        }
    }

    pub fn sanitize_tone_curves(&mut self) {
        self.tone_curve.sanitize();
        self.tone_curve_red.sanitize();
        self.tone_curve_green.sanitize();
        self.tone_curve_blue.sanitize();
    }

    pub fn reset_tone_curves(&mut self) {
        self.tone_curve.reset();
        self.tone_curve_red.reset();
        self.tone_curve_green.reset();
        self.tone_curve_blue.reset();
    }

    pub fn scene_referred_default() -> Self {
        Self::default()
    }
}

impl Default for ExposureParams {
    fn default() -> Self {
        Self {
            process_version: CURRENT_PROCESS_VERSION,
            black_point: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            sigmoid: SigmoidParams::default(),
            temperature: 0.0,
            tint: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            tone_curve: PointCurve::linear(),
            tone_curve_red: PointCurve::linear(),
            tone_curve_green: PointCurve::linear(),
            tone_curve_blue: PointCurve::linear(),
            chroma_denoise: 0.0,
            demosaic_mode: DemosaicMode::Reference,
            dual_threshold: 20.0,
            frequency_chroma: 1.0,
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
            glow_amount: 0.0,
            glow_radius: 50.0,
            glow_threshold: 60.0,
            vignette_amount: 0.0,
            vignette_midpoint: 50.0,
            vignette_roundness: 0.0,
            vignette_feather: 50.0,
            vignette_highlights: 0.0,
            hsl_hue: [0.0; 8],
            hsl_saturation: [0.0; 8],
            hsl_luminance: [0.0; 8],
            color_grading: ColorGrading::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DemosaicMode, ExposureParams, PointCurve, CURRENT_PROCESS_VERSION,
        GLOBAL_EXPOSURE_BACKEND_OFFSET_EV,
    };
    use crate::pipeline::SigmoidParams;

    #[test]
    fn reference_demosaic_is_the_default() {
        let params = ExposureParams::default();
        assert_eq!(params.demosaic_mode, DemosaicMode::Reference);
        assert_eq!(params.dual_threshold, 20.0);
        assert_eq!(params.frequency_chroma, 1.0);
    }

    #[test]
    fn global_exposure_defaults_to_zero_in_the_edit_model() {
        let neutral = ExposureParams::default();
        assert_eq!(neutral.exposure, 0.0);
        assert_eq!(neutral.black_point, 0.0);

        let rendition = ExposureParams::scene_referred_default();
        assert_eq!(rendition.exposure, 0.0);
        assert_eq!(rendition.black_point, 0.0);
        assert_eq!(rendition.sigmoid, SigmoidParams::default());
        assert_eq!(rendition.contrast, 0.0);
        assert_eq!(rendition.temperature, 0.0);
        assert_eq!(rendition.tint, 0.0);
        assert_eq!(rendition.saturation, 0.0);
        assert_eq!(rendition.vibrance, 0.0);
    }

    #[test]
    fn point_curve_default_is_a_sorted_identity() {
        let curve = PointCurve::default();
        assert_eq!(curve.len, 2);
        for (index, point) in curve.points[..curve.len as usize].iter().enumerate() {
            assert!((point[0] - point[1]).abs() < f32::EPSILON);
            if index > 0 {
                assert!(point[0] > curve.points[index - 1][0]);
            }
        }
    }

    #[test]
    fn demosaic_modes_have_stable_shader_values() {
        assert_eq!(DemosaicMode::Reference.shader_value(), 0.0);
        assert_eq!(DemosaicMode::FrequencyDomainChroma.shader_value(), 1.0);
        assert_eq!(DemosaicMode::Dual.shader_value(), 2.0);
    }

    #[test]
    fn older_presence_formulas_migrate_to_the_current_version() {
        let mut params = ExposureParams {
            process_version: CURRENT_PROCESS_VERSION - 1,
            exposure: GLOBAL_EXPOSURE_BACKEND_OFFSET_EV,
            ..ExposureParams::default()
        };
        params.migrate_to_current_process();
        assert_eq!(params.process_version, CURRENT_PROCESS_VERSION);
        assert_eq!(params.exposure, 0.0);
    }
}
