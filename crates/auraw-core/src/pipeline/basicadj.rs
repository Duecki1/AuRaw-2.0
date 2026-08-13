use super::noise::DenoiseQuality;
use super::sigmoid::SigmoidParams;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum DemosaicMode {
    /// Reference high-detail demosaic: RCD for Bayer and Markesteijn 3-pass
    /// for Fuji X-Trans.
    #[default]
    Reference,
    /// Apply frequency-domain chroma suppression to the reference result.
    FrequencyDomainChroma,
    /// Blend the high-detail result with a separate robust low-frequency
    /// reconstruction using edge, sensor-noise, and reconstruction confidence.
    /// The low branch is a clean-room gradient-guided alternative rather than
    /// a direct VNG/LMMSE code port.
    Dual,
}

impl DemosaicMode {
    pub const fn shader_value(self) -> f32 {
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
            Self::Dual => "Dual demosaic (robust)",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum HighlightReconstructionMethod {
    Off,
    Lch,
    #[default]
    #[serde(other)]
    InpaintOpposed,
}

impl HighlightReconstructionMethod {
    pub const fn shader_value(self) -> f32 {
        match self {
            Self::Off => 0.0,
            Self::Lch => 1.0,
            Self::InpaintOpposed => 2.0,
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
        // The Point Curve starts with the two neutral corner endpoints.
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
        // A diagonal partial-domain curve is not an identity: values before the
        // first point and after the last point are held at the endpoint values.
        if self.points[0][0].abs() > 1e-6 || (self.points[len - 1][0] - 1.0).abs() > 1e-6 {
            return false;
        }
        self.points[..len]
            .iter()
            .all(|point| (point[1] - point[0]).abs() <= 1e-6)
    }

    pub fn sanitize(&mut self) {
        const MIN_X_GAP: f32 = 0.005;

        self.len = self.len.clamp(2, MAX_POINT_CURVE_POINTS as u32);
        let len = self.len as usize;
        for index in 0..len {
            let lower = if index == 0 {
                0.0
            } else {
                self.points[index - 1][0] + MIN_X_GAP
            };
            let remaining = (len - 1 - index) as f32;
            let upper = 1.0 - MIN_X_GAP * remaining;
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

const DARKTABLE_SIGMOID_CONTRAST_SOFT_MIN: f32 = 0.7;
const DARKTABLE_SIGMOID_CONTRAST_SOFT_MAX: f32 = 3.0;
/// Kelvin limits presented by the global white-balance control. These match
/// darktable's physical temperature control rather than exposing our internal
/// reciprocal-temperature offset.
pub const MIN_TEMPERATURE_KELVIN: f32 = 1_901.0;
pub const MAX_TEMPERATURE_KELVIN: f32 = 25_000.0;
/// Hard limits used by darktable's temperature module for its absolute tint
/// ratio. Unlike a zero-centred creative tint control, the camera's as-shot
/// value is normally close to (but not necessarily exactly) 1.0.
pub const MIN_WHITE_BALANCE_TINT: f32 = 0.135;
pub const MAX_WHITE_BALANCE_TINT: f32 = 2.326;
/// The serialized edit remains relative to the as-shot camera neutral so a
/// zero-valued sidecar still means "as shot" for every camera. One internal
/// unit is one hundredth of darktable's absolute tint ratio.
pub const WHITE_BALANCE_TINT_OFFSET_SCALE: f32 = 100.0;
pub const GLOBAL_TINT_OFFSET_LIMIT: f32 =
    (MAX_WHITE_BALANCE_TINT - MIN_WHITE_BALANCE_TINT) * WHITE_BALANCE_TINT_OFFSET_SCALE;
/// Maximum serialized mired displacement. This covers the complete Kelvin UI
/// range for every as-shot neutral inside that range.
pub const GLOBAL_TEMPERATURE_LIMIT: f32 = 500.0;

pub fn temperature_kelvin_from_offset(base_kelvin: f32, offset_mired: f32) -> f32 {
    let base_mired =
        1_000_000.0 / base_kelvin.clamp(MIN_TEMPERATURE_KELVIN, MAX_TEMPERATURE_KELVIN);
    (1_000_000.0 / (base_mired - offset_mired).max(1.0))
        .clamp(MIN_TEMPERATURE_KELVIN, MAX_TEMPERATURE_KELVIN)
}

pub fn temperature_offset_from_kelvin(base_kelvin: f32, target_kelvin: f32) -> f32 {
    let base = base_kelvin.clamp(MIN_TEMPERATURE_KELVIN, MAX_TEMPERATURE_KELVIN);
    let target = target_kelvin.clamp(MIN_TEMPERATURE_KELVIN, MAX_TEMPERATURE_KELVIN);
    (1_000_000.0 / base - 1_000_000.0 / target)
        .clamp(-GLOBAL_TEMPERATURE_LIMIT, GLOBAL_TEMPERATURE_LIMIT)
}

pub fn white_balance_tint_from_offset(base_tint: f32, offset: f32) -> f32 {
    (base_tint + offset / WHITE_BALANCE_TINT_OFFSET_SCALE)
        .clamp(MIN_WHITE_BALANCE_TINT, MAX_WHITE_BALANCE_TINT)
}

pub fn white_balance_tint_offset(base_tint: f32, target_tint: f32) -> f32 {
    ((target_tint.clamp(MIN_WHITE_BALANCE_TINT, MAX_WHITE_BALANCE_TINT) - base_tint)
        * WHITE_BALANCE_TINT_OFFSET_SCALE)
        .clamp(-GLOBAL_TINT_OFFSET_LIMIT, GLOBAL_TINT_OFFSET_LIMIT)
}
/// Extended Color Mixer hue range. Values through +/-100 retain the original
/// response for sidecar compatibility, while the extra travel allows stronger
/// creative shifts toward and beyond the neighbouring named hue.
pub const HSL_HUE_LIMIT: f32 = 200.0;
/// Whole-image and local-mask hue rotation, expressed in degrees. The signed
/// half-turn range can reach every target hue from the source color.
pub const HUE_ROTATION_LIMIT_DEGREES: f32 = 180.0;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExposureParams {
    /// Additional normalized sensor-space black-point correction. It is applied
    /// per CFA plane before white balance and demosaic, and is deliberately
    /// separate from the creative `blacks` control in the Basic panel.
    /// Values are clamped to +/-0.25 of the metadata-calibrated sensor range.
    pub black_point: f32,
    /// Scene-linear exposure in stops, applied before local/color processing.
    pub exposure: f32,
    /// User-facing global contrast in the -100..100 domain. The renderer maps
    /// it to darktable's 0.7..3.0 sigmoid middle-grey contrast slider soft
    /// slope; zero maps to 1.5.
    #[serde(default)]
    pub contrast: f32,
    /// darktable-compatible sigmoid scene-to-display transform.
    pub sigmoid: SigmoidParams,
    /// Relative metadata-aware white balance. Temperature is serialized as an
    /// internal mired displacement but presented to users in Kelvin; tint is a
    /// relative encoding of darktable's absolute camera-derived tint ratio.
    /// Zero preserves the as-shot neutral for both controls.
    pub temperature: f32,
    pub tint: f32,
    /// Perceptual hue rotation in degrees. This is distinct from the
    /// eight-channel Color Mixer and leaves lightness/chroma unchanged until
    /// gamut compression is required.
    #[serde(default)]
    pub hue: f32,
    pub saturation: f32,
    pub vibrance: f32,
    /// Composite luminance curve followed by independent scene-referred RGB channel curves.
    pub tone_curve: PointCurve,
    pub tone_curve_red: PointCurve,
    pub tone_curve_green: PointCurve,
    pub tone_curve_blue: PointCurve,
    pub chroma_denoise: f32,
    /// Sensor-profiled camera-linear luminance noise reduction, 0..100.
    #[serde(default)]
    pub luminance_denoise: f32,
    /// Detail protection for sensor-profiled denoise, 0..100. Higher values
    /// reject cross-edge samples more aggressively.
    #[serde(default = "default_denoise_detail")]
    pub denoise_detail: f32,
    /// Tap budget / scale count for the multiscale denoise stage.
    #[serde(default)]
    pub denoise_quality: DenoiseQuality,
    /// Use the pinned RawNIND UtNet2 model instead of AuRaw's standard
    /// luminance/chroma denoise path. The original standard values remain
    /// serialized so disabling AI restores them exactly.
    #[serde(default)]
    pub ai_denoise_enabled: bool,
    /// Demosaic finishing mode. The reference algorithm is always run first.
    pub demosaic_mode: DemosaicMode,
    /// Detail threshold in darktable-compatible 0..100 units for dual mode.
    pub dual_threshold: f32,
    /// Strength of the frequency-domain chroma suppression stage.
    pub frequency_chroma: f32,
    pub ca_red: f32,
    pub ca_blue: f32,
    /// Pre-demosaic highlight-reconstruction algorithm.
    pub highlight_method: HighlightReconstructionMethod,
    /// Raw highlight-clipping threshold used by reconstruction.
    pub highlight_clip: f32,
    /// LCh reconstruction strength. Inpaint opposed follows darktable and
    /// always applies its complete replacement to clipped photosites.
    pub highlight_reconstruction: f32,

    // Basic tonal controls. Highlights/Whites and Shadows are scene-referred;
    // Blacks is a view-adjacent display-linear toe/endpoint remap.
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,

    // Capture sharpening. The defaults mirror the familiar Lightroom-style
    // starting point while remaining entirely non-destructive and editable.
    // Amount is 0..150, Radius is 0.5..3.0 px at a 1080 px reference short
    // edge, and Detail/Masking use 0..100 perceptual domains.
    #[serde(default = "default_sharpen_amount")]
    pub sharpen_amount: f32,
    #[serde(default = "default_sharpen_radius")]
    pub sharpen_radius: f32,
    #[serde(default = "default_sharpen_detail")]
    pub sharpen_detail: f32,
    #[serde(default)]
    pub sharpen_masking: f32,

    // Creative effects. Glow follows a highlight-aware, multi-scale bloom.
    // Vignette is a post-crop, display-linear edge treatment calibrated from
    // Lightroom's default composite shape.
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

pub fn sigmoid_contrast_from_percent(contrast: f32) -> f32 {
    let amount = if contrast.is_finite() {
        (contrast / 100.0).clamp(-1.0, 1.0)
    } else {
        0.0
    };
    // Preserve AuRaw's centered percentage UI while matching darktable's
    // normal sigmoid slider: -100 -> 0.7, 0 -> 1.5, +100 -> 3.0. Interpolate
    // the slope geometrically on each side because contrast is multiplicative.
    // Darktable's 0.1..10 hard limits remain available only through expert
    // entry there and produced brittle, near-binary output at AuRaw's endpoint.
    let default = SigmoidParams::default().contrast;
    if amount >= 0.0 {
        default * (DARKTABLE_SIGMOID_CONTRAST_SOFT_MAX / default).powf(amount)
    } else {
        default * (DARKTABLE_SIGMOID_CONTRAST_SOFT_MIN / default).powf(-amount)
    }
}

impl ExposureParams {
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
            black_point: 0.0,
            exposure: 0.0,
            contrast: 0.0,
            sigmoid: SigmoidParams::default(),
            temperature: 0.0,
            tint: 0.0,
            hue: 0.0,
            saturation: 0.0,
            vibrance: 0.0,
            tone_curve: PointCurve::linear(),
            tone_curve_red: PointCurve::linear(),
            tone_curve_green: PointCurve::linear(),
            tone_curve_blue: PointCurve::linear(),
            chroma_denoise: 0.0,
            luminance_denoise: 0.0,
            denoise_detail: default_denoise_detail(),
            denoise_quality: DenoiseQuality::default(),
            ai_denoise_enabled: false,
            demosaic_mode: DemosaicMode::Reference,
            dual_threshold: 20.0,
            frequency_chroma: 1.0,
            ca_red: 0.0,
            ca_blue: 0.0,
            highlight_method: HighlightReconstructionMethod::InpaintOpposed,
            highlight_clip: 1.0,
            highlight_reconstruction: 1.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
            sharpen_amount: default_sharpen_amount(),
            sharpen_radius: default_sharpen_radius(),
            sharpen_detail: default_sharpen_detail(),
            sharpen_masking: 0.0,
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

const fn default_denoise_detail() -> f32 {
    50.0
}

const fn default_sharpen_amount() -> f32 {
    40.0
}

const fn default_sharpen_radius() -> f32 {
    1.0
}

const fn default_sharpen_detail() -> f32 {
    25.0
}

#[cfg(test)]
mod tests {
    use super::{
        sigmoid_contrast_from_percent, temperature_kelvin_from_offset,
        temperature_offset_from_kelvin, DemosaicMode, ExposureParams,
        HighlightReconstructionMethod, PointCurve, MAX_TEMPERATURE_KELVIN, MIN_TEMPERATURE_KELVIN,
    };
    use crate::pipeline::SigmoidParams;

    #[test]
    fn kelvin_ui_round_trips_through_the_serialized_mired_offset() {
        let base = 5_000.0;
        for target in [
            MIN_TEMPERATURE_KELVIN,
            3_200.0,
            base,
            6_500.0,
            MAX_TEMPERATURE_KELVIN,
        ] {
            let offset = temperature_offset_from_kelvin(base, target);
            let round_trip = temperature_kelvin_from_offset(base, offset);
            assert!((round_trip - target).abs() < 0.01, "{target} K");
        }
        assert_eq!(temperature_offset_from_kelvin(base, base), 0.0);
    }

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
        assert_eq!(rendition.hue, 0.0);
        assert_eq!(rendition.saturation, 0.0);
        assert_eq!(rendition.vibrance, 0.0);
        assert_eq!(
            rendition.highlight_method,
            HighlightReconstructionMethod::InpaintOpposed
        );
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
    fn point_curve_sanitize_preserves_moved_endpoints() {
        let mut curve = PointCurve::linear();
        curve.points[0] = [0.2, 0.3];
        curve.points[1] = [0.8, 0.7];
        curve.sanitize();

        assert_eq!(curve.points[0], [0.2, 0.3]);
        assert_eq!(curve.points[1], [0.8, 0.7]);
    }

    #[test]
    fn diagonal_partial_domain_curve_is_not_identity() {
        let mut curve = PointCurve::linear();
        curve.points[0] = [0.2, 0.2];
        curve.points[1] = [0.8, 0.8];
        curve.sanitize();

        assert!(!curve.is_identity());
    }

    #[test]
    fn point_curve_sanitize_keeps_all_points_sorted_inside_the_domain() {
        let mut curve = PointCurve::linear();
        curve.len = 4;
        curve.points[0] = [0.7, -1.0];
        curve.points[1] = [0.1, 0.25];
        curve.points[2] = [0.2, 0.75];
        curve.points[3] = [0.3, 2.0];
        curve.sanitize();

        assert!(curve.points[0][0] >= 0.0);
        assert!(curve.points[3][0] <= 1.0);
        for index in 1..curve.len as usize {
            assert!(curve.points[index][0] - curve.points[index - 1][0] >= 0.0049);
        }
        for point in &curve.points[..curve.len as usize] {
            assert!((0.0..=1.0).contains(&point[1]));
        }
    }

    #[test]
    fn demosaic_modes_have_stable_shader_values() {
        assert_eq!(DemosaicMode::Reference.shader_value(), 0.0);
        assert_eq!(DemosaicMode::FrequencyDomainChroma.shader_value(), 1.0);
        assert_eq!(DemosaicMode::Dual.shader_value(), 2.0);
    }

    #[test]
    fn retired_highlight_methods_decode_as_inpaint_opposed() {
        let decoded: HighlightReconstructionMethod =
            serde_json::from_str("\"RetiredMethod\"").expect("decode retired method");
        assert_eq!(decoded, HighlightReconstructionMethod::InpaintOpposed);
    }

    #[test]
    fn contrast_percent_endpoints_map_to_photographic_darktable_slopes() {
        for (percent, expected) in [(-100.0, 0.7), (0.0, 1.5), (100.0, 3.0)] {
            assert!((sigmoid_contrast_from_percent(percent) - expected).abs() < 1e-5);
        }
        assert!((sigmoid_contrast_from_percent(-50.0) - 1.024_695).abs() < 1e-5);
        assert!((sigmoid_contrast_from_percent(50.0) - 2.121_320_2).abs() < 1e-5);
    }

    #[test]
    fn serialization_contains_only_modern_adjustment_fields() {
        let serialized =
            serde_json::to_value(ExposureParams::default()).expect("serialize exposure");
        assert_eq!(serialized["contrast"], 0.0);
        assert_eq!(serialized["hue"], 0.0);
        assert_eq!(serialized["sigmoid"]["contrast"], 1.5);
    }

    #[test]
    fn exposure_without_hue_deserializes_to_a_neutral_rotation() {
        let mut serialized =
            serde_json::to_value(ExposureParams::default()).expect("serialize exposure");
        serialized
            .as_object_mut()
            .expect("exposure is a JSON object")
            .remove("hue");
        let decoded: ExposureParams =
            serde_json::from_value(serialized).expect("deserialize legacy exposure");
        assert_eq!(decoded.hue, 0.0);
    }
}
