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
            Self::Dual => "Dual demosaic (robust)",
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

/// Historical process milestones are retained for decoding old sidecars and
/// documenting shader changes. Runtime rendering is canonical: every supported
/// edit is migrated to `CURRENT_PROCESS_VERSION` before it can be rendered,
/// copied, or saved, so identical adjustment values cannot select different
/// image-processing graphs.
pub const LEGACY_SCENE_DISPLAY_PROCESS_VERSION: u32 = 12;
pub const SCENE_DISPLAY_BOUNDARY_PROCESS_VERSION: u32 = 13;
pub const SENSOR_DENOISE_PROCESS_VERSION: u32 = 14;
pub const HIGHLIGHT_CONSENSUS_PROCESS_VERSION: u32 = 15;
pub const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16;
/// Process 17 separates the photographic roles of the two low-tone controls:
/// Shadows is a bounded scene-EV zone remap with an actual-pixel-aware selector,
/// while Blacks is a view-adjacent display-linear toe remap. This prevents DCP
/// ProfileToneCurve/sigmoid compression from consuming most of the black-point
/// authority and keeps low-key/high-key pivots from degenerating.
pub const PHOTOGRAPHIC_LOW_TONE_PROCESS_VERSION: u32 = 17;
/// Process 18 calibrates the basic-control endpoints against isolated Adobe
/// Camera Raw/Lightroom exports. Shadows becomes a monotone low-pass tonal
/// range instead of a band-pass zone (so the deepest visible detail receives
/// full authority), and Dehaze uses a bounded ambient-relative transfer that
/// cannot collapse broad midtone ranges to black.
pub const LIGHTROOM_BASIC_MATCH_PROCESS_VERSION: u32 = 18;
/// Process 19 calibrates the flat-field RAW-noise estimate and gives new or
/// previously-default edits per-capture Detail starting values. Capture
/// sharpening also uses that sensor model as a noise threshold, so its Amount
/// 40 default restores acutance without crispening high-ISO speckle.
pub const ADAPTIVE_DETAIL_DEFAULTS_PROCESS_VERSION: u32 = 19;
/// Process 20 replaces sparse-ring chroma averaging with a dense, staged
/// camera-space wavelet shrinker. It decorrelates the two opponent axes,
/// preserves the camera signal exactly, and removes demosaic-correlated colour
/// clouds across six multiscale passes.
pub const MULTISCALE_COLOR_DENOISE_PROCESS_VERSION: u32 = 20;
/// Process 21 adds a variance-normalized opponent-color guide to every
/// multiscale chroma pass. Isoluminant color boundaries no longer look flat to
/// the denoiser, preventing broad-scale chroma from bleeding into adjacent
/// neutral or differently colored regions.
pub const EDGE_AWARE_COLOR_DENOISE_PROCESS_VERSION: u32 = 21;
/// Process 22 tracks the falling residual noise variance across the staged
/// chroma wavelet passes and uses a profiled-noise dead zone in the color
/// guide. High Color values can therefore reject subtle dark color boundaries
/// without blocking the smoothing of stochastic chroma noise.
pub const SCALE_AWARE_COLOR_DENOISE_PROCESS_VERSION: u32 = 22;
/// Process 23 calibrates the default vignette shape against Lightroom while
/// retaining the optional Midpoint, Roundness, Feather, and Highlights
/// controls. Its final-frame geometry follows darktable's auto-ratio
/// convention, so the default falloff is stable across aspect ratios.
pub const LIGHTROOM_VIGNETTE_PROCESS_VERSION: u32 = 23;
/// RawNIND AI denoise is a persisted, mutually-exclusive RAW reconstruction
/// choice. Derived pixels live in a rebuildable, source-validated disk cache.
pub const AI_DENOISE_PROCESS_VERSION: u32 = 24;
/// Bayer RawNIND output follows darktable's production remosaic contract
/// before entering AuRaw's ordinary demosaic stage.
pub const AI_DENOISE_REMOSAIC_PROCESS_VERSION: u32 = 25;
/// Process 26 is calibrated from aligned 16-bit AdobeRGB Lightroom endpoints.
/// It narrows Highlights/Whites to their measured tonal zones, gives Contrast
/// a photographic black-end response, bounds Dehaze's tone/chroma authority,
/// and balances the presence and color-control endpoints without weakening the
/// scene-linear, hue-preserving processing guarantees.
pub const LIGHTROOM_HIGH_QUALITY_PROCESS_VERSION: u32 = 26;
/// Process 27 matches RawNIND's daylight-WB preprocessing, reconstructs
/// clipped highlights before the scene boundary, overlap-blends every tile,
/// and persists a versioned derived result for instant reopen.
pub const AI_DENOISE_SEAMLESS_CACHE_PROCESS_VERSION: u32 = 27;
pub const CURRENT_PROCESS_VERSION: u32 = AI_DENOISE_SEAMLESS_CACHE_PROCESS_VERSION;
/// Kelvin limits presented by the global white-balance control. These match
/// darktable's physical temperature control rather than exposing our internal
/// reciprocal-temperature offset.
pub const MIN_TEMPERATURE_KELVIN: f32 = 1_901.0;
pub const MAX_TEMPERATURE_KELVIN: f32 = 25_000.0;
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
/// Extended Color Mixer hue range. Values through +/-100 retain the original
/// response for sidecar compatibility, while the extra travel allows stronger
/// creative shifts toward and beyond the neighbouring named hue.
pub const HSL_HUE_LIMIT: f32 = 200.0;

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ExposureParams {
    /// Serialized process marker. Supported historical values are accepted at
    /// the sidecar boundary, then immediately canonicalized to the current
    /// renderer so this field never changes image behaviour at runtime.
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
    /// Relative metadata-aware white balance. Temperature is serialized as an
    /// internal mired displacement but presented to users in Kelvin; tint is a
    /// Planckian-normal Duv displacement. Zero preserves the as-shot neutral.
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

    // Basic tonal controls. Highlights/Whites and Shadows are scene-referred;
    // Process 17+ Blacks is a view-adjacent display-linear toe/endpoint remap.
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

/// Historical renderer-only exposure lift used by process versions 8 and 9.
/// New edits never receive this implicitly; it exists only so older sidecars
/// can migrate their rendered brightness into the explicit Exposure value.
pub const LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV: f32 = 0.7;

impl ExposureParams {
    pub fn migrate_to_current_process(&mut self) {
        // Versions 8 and 9 stored a zero-centered Exposure control while the
        // renderer secretly added +0.7 EV. Preserve that brightness by moving
        // the hidden lift into the visible value once. Every supported process
        // version then adopts the current graph and formulas. This deliberately
        // favors one deterministic interpretation of an adjustment set over
        // retaining multiple image-dependent compatibility renderers.
        match self.process_version {
            0..=7 => {
                self.process_version = CURRENT_PROCESS_VERSION;
            }
            8 | 9 => {
                self.exposure += LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV;
                self.process_version = CURRENT_PROCESS_VERSION;
            }
            10..=CURRENT_PROCESS_VERSION => {
                self.process_version = CURRENT_PROCESS_VERSION;
            }
            // Preserve unknown future versions so the sidecar boundary can
            // reject them rather than reinterpret them with older formulas.
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

    /// Detail settings used by process 18 and earlier before per-capture
    /// defaults existed. This lets migration improve untouched defaults while
    /// preserving any image where the photographer changed a Detail control.
    pub(crate) fn has_legacy_default_detail_settings(&self) -> bool {
        self.chroma_denoise == 0.0
            && self.luminance_denoise == 0.0
            && self.denoise_detail == default_denoise_detail()
            && self.denoise_quality == DenoiseQuality::Balanced
            && !self.ai_denoise_enabled
            && self.sharpen_amount == default_sharpen_amount()
            && self.sharpen_radius == default_sharpen_radius()
            && self.sharpen_detail == default_sharpen_detail()
            && self.sharpen_masking == 0.0
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
            luminance_denoise: 0.0,
            denoise_detail: default_denoise_detail(),
            denoise_quality: DenoiseQuality::default(),
            ai_denoise_enabled: false,
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
        temperature_kelvin_from_offset, temperature_offset_from_kelvin, DemosaicMode,
        ExposureParams, PointCurve, CURRENT_PROCESS_VERSION,
        LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV, MAX_TEMPERATURE_KELVIN, MIN_TEMPERATURE_KELVIN,
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
        assert_eq!(neutral.process_version, CURRENT_PROCESS_VERSION);
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
    fn every_supported_process_version_uses_the_current_renderer() {
        for process_version in 0..=CURRENT_PROCESS_VERSION {
            let mut previous = ExposureParams {
                process_version,
                shadows: 42.0,
                blacks: -31.0,
                luminance_denoise: 55.0,
                ..ExposureParams::default()
            };
            previous.migrate_to_current_process();
            assert_eq!(previous.process_version, CURRENT_PROCESS_VERSION);
            assert_eq!(previous.shadows, 42.0);
            assert_eq!(previous.blacks, -31.0);
            assert_eq!(previous.luminance_denoise, 55.0);
        }
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
    fn legacy_backend_exposure_is_migrated_into_the_visible_control() {
        let mut pre_backend = ExposureParams {
            process_version: 7,
            exposure: LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV,
            ..ExposureParams::default()
        };
        pre_backend.migrate_to_current_process();
        assert_eq!(pre_backend.process_version, CURRENT_PROCESS_VERSION);
        assert_eq!(
            pre_backend.exposure,
            LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV
        );

        let mut hidden_backend = ExposureParams {
            process_version: 9,
            exposure: 0.0,
            ..ExposureParams::default()
        };
        hidden_backend.migrate_to_current_process();
        assert_eq!(hidden_backend.process_version, CURRENT_PROCESS_VERSION);
        assert_eq!(
            hidden_backend.exposure,
            LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV
        );

        let mut previous_tone_formula = ExposureParams {
            process_version: 10,
            exposure: 0.35,
            contrast: 42.0,
            ..ExposureParams::default()
        };
        previous_tone_formula.migrate_to_current_process();
        assert_eq!(
            previous_tone_formula.process_version,
            CURRENT_PROCESS_VERSION
        );
        assert_eq!(previous_tone_formula.exposure, 0.35);
        assert_eq!(previous_tone_formula.contrast, 42.0);
    }
}
