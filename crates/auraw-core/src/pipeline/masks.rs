use half::f16;
use rayon::prelude::*;
use std::f32::consts::TAU;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

mod effects;

pub use effects::{
    params as effect_params, BlurEffectSettings, EdgeGlowEffectSettings, FogEffectSettings,
    GlowEffectSettings, LensBlurEffectSettings, LightRaysEffectSettings, MaskEffect,
    MaskEffectCategory, MaskEffectSettings, MotionBlurEffectSettings, NeonEffectSettings,
    PixelateEffectSettings, RadialBlurEffectSettings, RadialBlurMode, SmokeEffectSettings,
    TiltShiftEffectSettings,
};

pub const MAX_LOCAL_MASKS: usize = 32;
pub const MAX_MASK_COMPONENTS: usize = 64;
pub const MASK_ATLAS_EDGE_DESKTOP: u32 = 2048;
pub const MASK_ATLAS_EDGE_ANDROID: u32 = 1024;
pub const MASK_ATLAS_EDGE_EXPORT_DESKTOP: u32 = 4096;
pub const MASK_ATLAS_EDGE_EXPORT_ANDROID: u32 = 2048;

pub const fn mask_atlas_edge() -> u32 {
    if cfg!(target_os = "android") {
        MASK_ATLAS_EDGE_ANDROID
    } else {
        MASK_ATLAS_EDGE_DESKTOP
    }
}

pub const fn export_mask_atlas_edge_limit() -> u32 {
    if cfg!(target_os = "android") {
        MASK_ATLAS_EDGE_EXPORT_ANDROID
    } else {
        MASK_ATLAS_EDGE_EXPORT_DESKTOP
    }
}

pub fn export_mask_atlas_edge(image_width: u32, image_height: u32) -> u32 {
    image_width
        .max(image_height)
        .min(export_mask_atlas_edge_limit())
        .max(mask_atlas_edge())
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum MaskKind {
    #[default]
    Brush,
    Fullscreen,
    Radial,
    Linear,
    Subject,
    Background,
    #[serde(alias = "Landscape")]
    Object,
    LuminanceRange,
    ColorRange,
    DepthRange,
}

impl MaskKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Brush => "Brush",
            Self::Fullscreen => "Fullscreen",
            Self::Radial => "Radial Gradient",
            Self::Linear => "Linear Gradient",
            Self::Subject => "Select Subject",
            Self::Background => "Select Not Subject",
            Self::Object => "Select Object",
            Self::LuminanceRange => "Luminance Range",
            Self::ColorRange => "Color Range",
            Self::DepthRange => "Depth Range",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
            Self::Fullscreen => "Full Image",
            Self::Radial => "Radial",
            Self::Linear => "Linear",
            Self::LuminanceRange => "Luminance",
            Self::ColorRange => "Color",
            Self::DepthRange => "Depth",
            _ => self.label(),
        }
    }

    pub const fn is_available(self) -> bool {
        matches!(
            self,
            Self::Brush
                | Self::Fullscreen
                | Self::Radial
                | Self::Linear
                | Self::Subject
                | Self::Background
                | Self::Object
                | Self::LuminanceRange
                | Self::ColorRange
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum MaskCombineMode {
    #[default]
    Add,
    Subtract,
    Intersect,
}

impl MaskCombineMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Add => "Add",
            Self::Subtract => "Subtract",
            Self::Intersect => "Intersect",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BrushMode {
    #[default]
    Paint,
    Erase,
}

impl BrushMode {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Paint => "Brush",
            Self::Erase => "Eraser",
        }
    }

    pub fn dab_opacity(self, opacity_enabled: bool, opacity: f32) -> f32 {
        let magnitude = if opacity_enabled {
            opacity.clamp(0.0, 1.0)
        } else {
            1.0
        };
        match self {
            Self::Paint => magnitude,
            Self::Erase => -magnitude,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct BrushDab {
    pub center: [f32; 2],
    pub opacity: f32,
    pub size: f32,
    pub feather: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SubjectRefinement {
    #[serde(default = "default_subject_refinement_size")]
    pub size: f32,
    #[serde(default = "default_subject_refinement_feather")]
    pub feather: f32,
    #[serde(default = "default_subject_refinement_flow")]
    pub flow: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub stroke_starts: Vec<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dabs: Vec<BrushDab>,
}

impl Default for SubjectRefinement {
    fn default() -> Self {
        Self {
            size: default_subject_refinement_size(),
            feather: default_subject_refinement_feather(),
            flow: default_subject_refinement_flow(),
            stroke_starts: Vec::new(),
            dabs: Vec::new(),
        }
    }
}

impl SubjectRefinement {
    pub fn is_empty(&self) -> bool {
        self.dabs.is_empty()
    }

    pub fn clear(&mut self) {
        self.dabs.clear();
        self.stroke_starts.clear();
    }

    pub fn composite(&self, raw_ai_mask: &MaskImage) -> Option<MaskImage> {
        if self.is_empty() {
            return Some(raw_ai_mask.clone());
        }
        let delta = rasterize_subject_refinement_delta(
            MaskRasterSpace::new(
                raw_ai_mask.width,
                raw_ai_mask.height,
                raw_ai_mask.width,
                raw_ai_mask.height,
            ),
            self,
        );
        let pixels = raw_ai_mask
            .pixels
            .iter()
            .copied()
            .zip(delta)
            .map(|(raw, delta)| {
                let probability = raw as f32 / 255.0;
                ((probability + delta).clamp(0.0, 1.0) * 255.0 + 0.5) as u8
            })
            .collect();
        MaskImage::new(raw_ai_mask.width, raw_ai_mask.height, pixels)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct ObjectStroke {
    pub points: Vec<[f32; 2]>,
    pub positive: bool,
    #[serde(default)]
    pub brush_size: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskImage {
    pub width: u32,
    pub height: u32,
    #[serde(with = "base64_arc_bytes")]
    pub pixels: Arc<[u8]>,
    #[serde(skip, default = "unit_sampling_rect")]
    sampling_rect: [f32; 4],
}

impl MaskImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        let pixel_count = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?;
        (pixels.len() == pixel_count).then(|| Self {
            width,
            height,
            pixels: pixels.into(),
            sampling_rect: unit_sampling_rect(),
        })
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskRgbImage {
    pub width: u32,
    pub height: u32,
    #[serde(with = "base64_arc_bytes")]
    pub rgba: Arc<[u8]>,
    #[serde(skip, default = "unit_sampling_rect")]
    sampling_rect: [f32; 4],
}

impl MaskRgbImage {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        let byte_count = usize::try_from(width)
            .ok()?
            .checked_mul(usize::try_from(height).ok()?)?
            .checked_mul(4)?;
        (rgba.len() == byte_count).then(|| Self {
            width,
            height,
            rgba: rgba.into(),
            sampling_rect: unit_sampling_rect(),
        })
    }
}

fn unit_sampling_rect() -> [f32; 4] {
    [0.0, 0.0, 1.0, 1.0]
}

mod base64_arc_bytes {
    use base64::Engine as _;
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S>(bytes: &Arc<[u8]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.collect_str(&base64::display::Base64Display::new(
            bytes.as_ref(),
            &base64::engine::general_purpose::STANDARD,
        ))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u8]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map(Arc::from)
            .map_err(serde::de::Error::custom)
    }
}

impl Default for BrushDab {
    fn default() -> Self {
        Self {
            center: [0.5, 0.5],
            opacity: 1.0,
            size: 0.055,
            feather: 0.55,
        }
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub enum MaskGeometry {
    Fullscreen,
    Brush {
        size: f32,
        feather: f32,
        #[serde(default)]
        opacity_enabled: bool,
        #[serde(default = "default_brush_opacity")]
        opacity: f32,
        #[serde(default = "default_brush_overlap_enabled")]
        overlap_enabled: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        stroke_starts: Vec<usize>,
        dabs: Vec<BrushDab>,
    },
    Radial {
        center: [f32; 2],
        radius: [f32; 2],
        rotation: f32,
        feather: f32,
        initialized: bool,
    },
    Linear {
        start: [f32; 2],
        end: [f32; 2],
        feather: f32,
        initialized: bool,
    },
    Ai {
        mask: Option<MaskImage>,
        #[serde(default)]
        grow: f32,
        feather: f32,
    },
    #[serde(alias = "Landscape")]
    Object {
        mask: Option<MaskImage>,
        #[serde(default)]
        grow: f32,
        feather: f32,
        #[serde(default = "default_object_brush_size")]
        brush_size: f32,
        #[serde(default = "default_object_edge_refine")]
        edge_refine: f32,
        #[serde(default)]
        strokes: Vec<ObjectStroke>,
    },
    LuminanceRange {
        #[serde(default, skip_serializing)]
        source: Option<MaskRgbImage>,
        low: f32,
        high: f32,
        #[serde(default)]
        grow: f32,
        feather: f32,
    },
    ColorRange {
        #[serde(default, skip_serializing)]
        source: Option<MaskRgbImage>,
        sample: [f32; 3],
        tolerance: f32,
        #[serde(default)]
        grow: f32,
        feather: f32,
        sampled: bool,
    },
    Placeholder,
}

fn default_object_brush_size() -> f32 {
    0.055
}

fn default_subject_refinement_size() -> f32 {
    0.035
}

fn default_subject_refinement_feather() -> f32 {
    0.55
}

fn default_subject_refinement_flow() -> f32 {
    1.0
}

fn default_brush_opacity() -> f32 {
    1.0
}

fn default_brush_overlap_enabled() -> bool {
    true
}

fn default_object_edge_refine() -> f32 {
    0.55
}

impl MaskGeometry {
    pub fn for_kind(kind: MaskKind) -> Self {
        match kind {
            MaskKind::Fullscreen => Self::Fullscreen,
            MaskKind::Brush => Self::Brush {
                size: 0.055,
                feather: 0.55,
                opacity_enabled: false,
                opacity: default_brush_opacity(),
                overlap_enabled: default_brush_overlap_enabled(),
                stroke_starts: Vec::new(),
                dabs: Vec::new(),
            },
            MaskKind::Radial => Self::Radial {
                center: [0.5, 0.5],
                radius: [0.22, 0.16],
                rotation: 0.0,
                feather: 0.55,
                initialized: false,
            },
            MaskKind::Linear => Self::Linear {
                start: [0.35, 0.5],
                end: [0.65, 0.5],
                feather: 1.0,
                initialized: false,
            },
            MaskKind::Subject | MaskKind::Background => Self::Ai {
                mask: None,
                grow: 0.0,
                feather: 0.0,
            },
            MaskKind::Object => Self::Object {
                mask: None,
                grow: 0.0,
                feather: 0.0,
                brush_size: default_object_brush_size(),
                edge_refine: default_object_edge_refine(),
                strokes: Vec::new(),
            },
            MaskKind::LuminanceRange => Self::LuminanceRange {
                source: None,
                low: 0.2,
                high: 0.8,
                grow: 0.0,
                feather: 0.15,
            },
            MaskKind::ColorRange => Self::ColorRange {
                source: None,
                sample: [0.5; 3],
                tolerance: 0.18,
                grow: 0.0,
                feather: 0.12,
                sampled: false,
            },
            _ => Self::Placeholder,
        }
    }

    pub fn is_initialized(&self) -> bool {
        match self {
            Self::Fullscreen => true,
            Self::Brush { dabs, .. } => !dabs.is_empty(),
            Self::Radial { initialized, .. } | Self::Linear { initialized, .. } => *initialized,
            Self::Ai { mask, .. } | Self::Object { mask, .. } => mask.is_some(),
            Self::LuminanceRange { source, .. } => source.is_some(),
            Self::ColorRange {
                source, sampled, ..
            } => source.is_some() && *sampled,
            Self::Placeholder => false,
        }
    }

    pub fn set_feather(&mut self, value: f32) -> bool {
        let feather = match self {
            Self::Brush { feather, .. }
            | Self::Radial { feather, .. }
            | Self::Linear { feather, .. }
            | Self::Ai { feather, .. }
            | Self::Object { feather, .. }
            | Self::LuminanceRange { feather, .. }
            | Self::ColorRange { feather, .. } => feather,
            Self::Fullscreen | Self::Placeholder => return false,
        };
        set_if_changed(feather, value)
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskCommon {
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub invert: bool,
}

impl MaskCommon {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            enabled: true,
            invert: false,
        }
    }

    pub fn rename(&mut self, name: impl Into<String>) -> bool {
        set_if_changed(&mut self.name, name.into())
    }

    pub fn set_enabled(&mut self, enabled: bool) -> bool {
        set_if_changed(&mut self.enabled, enabled)
    }

    pub fn toggle_invert(&mut self) {
        self.invert = !self.invert;
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskComponent {
    #[serde(flatten)]
    pub common: MaskCommon,
    pub kind: MaskKind,
    pub combine: MaskCombineMode,
    pub geometry: MaskGeometry,
}

impl Deref for MaskComponent {
    type Target = MaskCommon;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl DerefMut for MaskComponent {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.common
    }
}

impl MaskComponent {
    pub fn new(kind: MaskKind, combine: MaskCombineMode) -> Self {
        Self {
            common: MaskCommon::new(kind.label()),
            kind,
            combine,
            geometry: MaskGeometry::for_kind(kind),
        }
    }

    pub fn set_combine(&mut self, combine: MaskCombineMode) -> bool {
        set_if_changed(&mut self.combine, combine)
    }

    pub fn set_feather(&mut self, feather: f32) -> bool {
        self.geometry.set_feather(feather)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct LocalAdjustments {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub temperature: f32,
    pub tint: f32,
    #[serde(default)]
    pub hue: f32,
    pub saturation: f32,
    pub texture: f32,
    pub clarity: f32,
    pub dehaze: f32,
    pub tone_curve: super::PointCurve,
    pub tone_curve_red: super::PointCurve,
    pub tone_curve_green: super::PointCurve,
    pub tone_curve_blue: super::PointCurve,
    pub hsl_hue: [f32; 8],
    pub hsl_saturation: [f32; 8],
    pub hsl_luminance: [f32; 8],
    pub color_grading: super::ColorGrading,
}

impl Default for LocalAdjustments {
    fn default() -> Self {
        use effect_params::adjustment;

        Self {
            exposure: adjustment::EXPOSURE.default,
            contrast: adjustment::CONTRAST.default,
            highlights: adjustment::HIGHLIGHTS.default,
            shadows: adjustment::SHADOWS.default,
            whites: adjustment::WHITES.default,
            blacks: adjustment::BLACKS.default,
            temperature: adjustment::TEMPERATURE.default,
            tint: adjustment::TINT.default,
            hue: adjustment::HUE.default,
            saturation: adjustment::SATURATION.default,
            texture: adjustment::TEXTURE.default,
            clarity: adjustment::CLARITY.default,
            dehaze: adjustment::DEHAZE.default,
            tone_curve: super::PointCurve::linear(),
            tone_curve_red: super::PointCurve::linear(),
            tone_curve_green: super::PointCurve::linear(),
            tone_curve_blue: super::PointCurve::linear(),
            hsl_hue: [0.0; 8],
            hsl_saturation: [0.0; 8],
            hsl_luminance: [0.0; 8],
            color_grading: super::ColorGrading::default(),
        }
    }
}

impl LocalAdjustments {
    pub fn curve_feature_flags(self) -> u32 {
        u32::from(!self.tone_curve.is_identity())
            | (u32::from(!self.tone_curve_red.is_identity()) << 1)
            | (u32::from(!self.tone_curve_green.is_identity()) << 2)
            | (u32::from(!self.tone_curve_blue.is_identity()) << 3)
    }

    pub fn has_color_mixer(self) -> bool {
        self.hsl_hue
            .iter()
            .chain(&self.hsl_saturation)
            .chain(&self.hsl_luminance)
            .any(|value| value.abs() > 1e-6)
    }

    pub fn has_color_grading(self) -> bool {
        !self.color_grading.is_neutral()
    }

    pub fn is_neutral(self) -> bool {
        let mut normalized = self;
        if normalized.color_grading.is_neutral() {
            normalized.color_grading = super::ColorGrading::default();
        }
        normalized == Self::default()
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }

    pub fn sanitize_tone_curves(&mut self) {
        self.tone_curve.sanitize();
        self.tone_curve_red.sanitize();
        self.tone_curve_green.sanitize();
        self.tone_curve_blue.sanitize();
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct LocalMask {
    #[serde(flatten)]
    pub common: MaskCommon,
    #[serde(default)]
    pub effect: MaskEffect,
    #[serde(default, skip_serializing_if = "MaskEffectSettings::is_default")]
    pub effect_settings: MaskEffectSettings,
    pub opacity: f32,
    pub components: Vec<MaskComponent>,
    pub adjustments: LocalAdjustments,
}

impl Deref for LocalMask {
    type Target = MaskCommon;

    fn deref(&self) -> &Self::Target {
        &self.common
    }
}

impl DerefMut for LocalMask {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.common
    }
}

impl LocalMask {
    pub fn new(kind: MaskKind, number: usize) -> Self {
        Self {
            common: MaskCommon::new(format!("Mask {number}")),
            effect: MaskEffect::default(),
            effect_settings: MaskEffectSettings::default(),
            opacity: 1.0,
            components: vec![MaskComponent::new(kind, MaskCombineMode::Add)],
            adjustments: LocalAdjustments::default(),
        }
    }

    pub fn set_opacity(&mut self, opacity: f32) -> bool {
        set_if_changed(&mut self.opacity, opacity)
    }
}

#[derive(Clone, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskStack {
    pub masks: Vec<LocalMask>,
    pub selected_mask: Option<usize>,
    pub selected_component: Option<usize>,
    #[serde(skip, default)]
    pub subject_refinement: SubjectRefinement,
}

impl MaskStack {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn cropped_for_region(
        &self,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        full_width: u32,
        full_height: u32,
    ) -> Self {
        let mut cropped = self.clone();
        let full_width = full_width.max(1);
        let full_height = full_height.max(1);
        let width = width.max(1);
        let height = height.max(1);
        let u0 = x as f32 / full_width as f32;
        let v0 = y as f32 / full_height as f32;
        let du = width as f32 / full_width as f32;
        let dv = height as f32 / full_height as f32;
        let image_scale = full_width.min(full_height) as f32 / width.min(height) as f32;

        let remap_point = |point: &mut [f32; 2]| {
            point[0] = (point[0] - u0) / du.max(f32::EPSILON);
            point[1] = (point[1] - v0) / dv.max(f32::EPSILON);
        };

        for mask in &mut cropped.masks {
            for component in &mut mask.components {
                match &mut component.geometry {
                    MaskGeometry::Fullscreen => {}
                    MaskGeometry::Brush { size, dabs, .. } => {
                        *size *= image_scale;
                        for dab in dabs {
                            remap_point(&mut dab.center);
                            dab.size *= image_scale;
                        }
                    }
                    MaskGeometry::Radial { center, radius, .. } => {
                        remap_point(center);
                        radius[0] /= du.max(f32::EPSILON);
                        radius[1] /= dv.max(f32::EPSILON);
                    }
                    MaskGeometry::Linear { start, end, .. } => {
                        remap_point(start);
                        remap_point(end);
                    }
                    MaskGeometry::Ai {
                        mask,
                        grow,
                        feather,
                    } => {
                        *mask = mask
                            .as_ref()
                            .map(|source| crop_mask_image(source, u0, v0, du, dv));
                        *grow *= image_scale;
                        *feather *= image_scale.powf(1.0 / 1.30);
                    }
                    MaskGeometry::Object {
                        mask,
                        grow,
                        feather,
                        brush_size,
                        strokes,
                        ..
                    } => {
                        *mask = mask
                            .as_ref()
                            .map(|source| crop_mask_image(source, u0, v0, du, dv));
                        *grow *= image_scale;
                        *feather *= image_scale.powf(1.0 / 1.30);
                        *brush_size *= image_scale;
                        for stroke in strokes {
                            if stroke.brush_size > 0.0 {
                                stroke.brush_size *= image_scale;
                            }
                            for point in &mut stroke.points {
                                remap_point(point);
                            }
                        }
                    }
                    MaskGeometry::LuminanceRange { source, grow, .. }
                    | MaskGeometry::ColorRange { source, grow, .. } => {
                        *source = source
                            .as_ref()
                            .map(|source| crop_rgb_image(source, u0, v0, du, dv));
                        *grow *= image_scale;
                    }
                    MaskGeometry::Placeholder => {}
                }
            }
        }
        cropped.subject_refinement.size *= image_scale;
        for dab in &mut cropped.subject_refinement.dabs {
            remap_point(&mut dab.center);
            dab.size *= image_scale;
        }
        cropped
    }

    pub fn add_mask(&mut self, kind: MaskKind) -> Option<(usize, usize)> {
        if self.masks.len() >= MAX_LOCAL_MASKS || !kind.is_available() {
            return None;
        }
        let mask_index = self.masks.len();
        self.masks.push(LocalMask::new(kind, mask_index + 1));
        self.select_mask(mask_index);
        Some((mask_index, 0))
    }

    pub fn add_component(
        &mut self,
        kind: MaskKind,
        combine: MaskCombineMode,
    ) -> Option<(usize, usize)> {
        if !kind.is_available() {
            return None;
        }
        let mask_index = self.selected_mask?;
        let mask = self.masks.get_mut(mask_index)?;
        if mask.components.len() >= MAX_MASK_COMPONENTS {
            return None;
        }
        let component_index = mask.components.len();
        mask.components.push(MaskComponent::new(kind, combine));
        self.selected_component = Some(component_index);
        Some((mask_index, component_index))
    }

    pub fn selected_mask(&self) -> Option<&LocalMask> {
        self.masks.get(self.selected_mask?)
    }

    pub fn selected_mask_mut(&mut self) -> Option<&mut LocalMask> {
        self.masks.get_mut(self.selected_mask?)
    }

    pub fn selected_component(&self) -> Option<&MaskComponent> {
        self.selected_mask()?
            .components
            .get(self.selected_component?)
    }

    pub fn selected_component_mut(&mut self) -> Option<&mut MaskComponent> {
        let component_index = self.selected_component?;
        self.selected_mask_mut()?
            .components
            .get_mut(component_index)
    }

    pub fn ensure_selection(&mut self) -> Option<(usize, usize)> {
        if self.masks.is_empty() {
            self.selected_mask = None;
            self.selected_component = None;
            return None;
        }
        let mask_index = self
            .selected_mask
            .filter(|&index| index < self.masks.len())
            .unwrap_or(self.masks.len() - 1);
        let component_count = self.masks[mask_index].components.len();
        if component_count == 0 {
            self.selected_mask = Some(mask_index);
            self.selected_component = None;
            return None;
        }
        let component_index = self
            .selected_component
            .filter(|&index| index < component_count)
            .unwrap_or(0);
        self.selected_mask = Some(mask_index);
        self.selected_component = Some(component_index);
        Some((mask_index, component_index))
    }

    pub fn select_mask(&mut self, mask_index: usize) -> bool {
        if mask_index >= self.masks.len() {
            return false;
        }
        self.selected_mask = Some(mask_index);
        self.selected_component = (!self.masks[mask_index].components.is_empty()).then_some(0);
        true
    }

    pub fn select_component(&mut self, mask_index: usize, component_index: usize) -> bool {
        if self
            .masks
            .get(mask_index)
            .is_none_or(|mask| component_index >= mask.components.len())
        {
            return false;
        }
        self.selected_mask = Some(mask_index);
        self.selected_component = Some(component_index);
        true
    }

    pub fn raster_margin_pixels_for_layer(
        &self,
        mask_index: usize,
        component_index: Option<usize>,
        image_width: u32,
        image_height: u32,
    ) -> u32 {
        let Some(mask) = self.masks.get(mask_index) else {
            return 2;
        };
        let edge = image_width.min(image_height).max(1) as f32;
        mask.components
            .iter()
            .enumerate()
            .filter(|(index, component)| {
                component.enabled && component_index.is_none_or(|selected| selected == *index)
            })
            .map(|(_, component)| component_shape_margin_pixels(component, edge))
            .fold(2.0_f32, f32::max)
            .ceil() as u32
    }

    pub fn raster_margin_pixels(&self, image_width: u32, image_height: u32) -> u32 {
        self.masks
            .iter()
            .enumerate()
            .map(|(index, _)| {
                self.raster_margin_pixels_for_layer(index, None, image_width, image_height)
            })
            .max()
            .unwrap_or(2)
    }

    pub fn delete_mask(&mut self, mask_index: usize) -> bool {
        if mask_index >= self.masks.len() {
            return false;
        }
        self.masks.remove(mask_index);
        for (number, mask) in self.masks.iter_mut().enumerate() {
            if mask.name.starts_with("Mask ") {
                mask.name = format!("Mask {}", number + 1);
            }
        }
        if self.masks.is_empty() {
            self.selected_mask = None;
            self.selected_component = None;
        } else {
            self.select_mask(mask_index.min(self.masks.len() - 1));
        }
        true
    }

    pub fn delete_component(&mut self, mask_index: usize, component_index: usize) -> bool {
        let Some(mask) = self.masks.get_mut(mask_index) else {
            return false;
        };
        if mask.components.len() <= 1 || component_index >= mask.components.len() {
            return false;
        }
        mask.components.remove(component_index);
        self.selected_mask = Some(mask_index);
        self.selected_component = Some(component_index.min(mask.components.len() - 1));
        true
    }

    pub fn duplicate_mask(&mut self, mask_index: usize, invert: bool) -> bool {
        let Some(mask) = self.masks.get(mask_index).cloned() else {
            return false;
        };
        self.insert_mask_copy(mask_index, mask, invert)
    }

    pub fn insert_mask_copy(
        &mut self,
        mask_index: usize,
        mut mask: LocalMask,
        invert: bool,
    ) -> bool {
        if self.masks.len() >= MAX_LOCAL_MASKS || mask_index >= self.masks.len() {
            return false;
        }
        mask.name = copied_name(&mask.name, |candidate| {
            self.masks.iter().any(|mask| mask.name == candidate)
        });
        if invert {
            mask.common.toggle_invert();
            mask.adjustments.reset();
        }
        let insert_at = mask_index + 1;
        self.masks.insert(insert_at, mask);
        self.select_mask(insert_at);
        true
    }

    pub fn duplicate_component(
        &mut self,
        mask_index: usize,
        component_index: usize,
        invert: bool,
    ) -> bool {
        let Some(component) = self
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
            .cloned()
        else {
            return false;
        };
        self.insert_component_copy(mask_index, component_index, component, invert)
    }

    pub fn insert_component_copy(
        &mut self,
        mask_index: usize,
        component_index: usize,
        mut component: MaskComponent,
        invert: bool,
    ) -> bool {
        let Some(mask) = self.masks.get_mut(mask_index) else {
            return false;
        };
        if mask.components.len() >= MAX_MASK_COMPONENTS || component_index >= mask.components.len()
        {
            return false;
        }
        component.name = copied_name(&component.name, |candidate| {
            mask.components
                .iter()
                .any(|component| component.name == candidate)
        });
        if invert {
            component.common.toggle_invert();
        }
        let insert_at = component_index + 1;
        mask.components.insert(insert_at, component);
        self.selected_mask = Some(mask_index);
        self.selected_component = Some(insert_at);
        true
    }

    pub fn move_submask_component(
        &mut self,
        source_mask: usize,
        source_component: usize,
        target_mask: usize,
        target_insert: usize,
    ) -> Option<(usize, usize)> {
        let source = self.masks.get(source_mask)?;
        if source.components.len() <= 1 || source_component >= source.components.len() {
            return None;
        }
        let target = self.masks.get(target_mask)?;
        if source_mask != target_mask && target.components.len() >= MAX_MASK_COMPONENTS {
            return None;
        }

        let component = self.masks[source_mask].components.remove(source_component);
        let adjusted_insert = if source_mask == target_mask && target_insert > source_component {
            target_insert - 1
        } else {
            target_insert
        };
        let insert_at = adjusted_insert.min(self.masks[target_mask].components.len());
        self.masks[target_mask]
            .components
            .insert(insert_at, component);
        self.selected_mask = Some(target_mask);
        self.selected_component = Some(insert_at);
        Some((target_mask, insert_at))
    }

    pub fn move_mask(&mut self, from: usize, to: usize) -> bool {
        if from == to || from >= self.masks.len() || to >= self.masks.len() {
            return false;
        }
        let mask = self.masks.remove(from);
        self.masks.insert(to, mask);
        self.selected_mask = self
            .selected_mask
            .map(|selected| moved_index(selected, from, to));
        true
    }

    pub fn move_component(&mut self, from: usize, to: usize) -> bool {
        let Some(mask_index) = self.selected_mask else {
            return false;
        };
        let Some(mask) = self.masks.get_mut(mask_index) else {
            return false;
        };
        if from == to || from >= mask.components.len() || to >= mask.components.len() {
            return false;
        }
        let component = mask.components.remove(from);
        mask.components.insert(to, component);
        self.selected_component = self
            .selected_component
            .map(|selected| moved_index(selected, from, to));
        true
    }

    fn rasterize_layer_coverage(
        &self,
        layer: usize,
        atlas_width: u32,
        atlas_height: u32,
        image_width: u32,
        image_height: u32,
    ) -> Vec<f32> {
        let len = atlas_width as usize * atlas_height as usize;
        let Some(mask) = self.masks.get(layer) else {
            return vec![0.0; len];
        };
        if mask.components.is_empty() {
            return vec![0.0; len];
        }

        let mut combined: Option<Vec<f32>> = None;
        for component in &mask.components {
            if !component.enabled || !component.geometry.is_initialized() {
                continue;
            }
            let mut coverage = rasterize_component(
                component,
                atlas_width,
                atlas_height,
                image_width,
                image_height,
                &self.subject_refinement,
            );
            if component.invert {
                coverage
                    .par_iter_mut()
                    .for_each(|value| *value = 1.0 - *value);
            }

            let Some(existing) = combined.as_mut() else {
                combined = Some(if component.combine == MaskCombineMode::Add {
                    coverage
                } else {
                    vec![0.0; len]
                });
                continue;
            };
            match component.combine {
                MaskCombineMode::Add => {
                    existing
                        .par_iter_mut()
                        .zip(coverage.into_par_iter())
                        .for_each(|(dst, src)| *dst = dst.max(src));
                }
                MaskCombineMode::Subtract => {
                    existing
                        .par_iter_mut()
                        .zip(coverage.into_par_iter())
                        .for_each(|(dst, src)| *dst *= 1.0 - src);
                }
                MaskCombineMode::Intersect => {
                    existing
                        .par_iter_mut()
                        .zip(coverage.into_par_iter())
                        .for_each(|(dst, src)| *dst *= src);
                }
            }
        }

        let Some(combined) = combined else {
            return vec![0.0; len];
        };
        let opacity = mask.opacity.clamp(0.0, 1.0);
        combined
            .into_par_iter()
            .map(|value| {
                let value = if mask.invert { 1.0 - value } else { value };
                value.clamp(0.0, 1.0) * opacity
            })
            .collect()
    }

    pub fn rasterize_layer(
        &self,
        layer: usize,
        atlas_width: u32,
        atlas_height: u32,
        image_width: u32,
        image_height: u32,
    ) -> Vec<u8> {
        self.rasterize_layer_coverage(layer, atlas_width, atlas_height, image_width, image_height)
            .into_par_iter()
            .map(|value| (value * 255.0 + 0.5) as u8)
            .collect()
    }

    pub fn rasterize_layer_f16(
        &self,
        layer: usize,
        atlas_width: u32,
        atlas_height: u32,
        image_width: u32,
        image_height: u32,
    ) -> Vec<u16> {
        self.rasterize_layer_coverage(layer, atlas_width, atlas_height, image_width, image_height)
            .into_par_iter()
            .map(|value| f16::from_f32(value).to_bits())
            .collect()
    }

    pub fn rasterize_component_layer(
        &self,
        mask_index: usize,
        component_index: usize,
        width: u32,
        height: u32,
        image_width: u32,
        image_height: u32,
    ) -> Vec<u8> {
        let Some(component) = self
            .masks
            .get(mask_index)
            .and_then(|mask| mask.components.get(component_index))
        else {
            return vec![0; width as usize * height as usize];
        };
        let mut coverage = rasterize_component(
            component,
            width,
            height,
            image_width,
            image_height,
            &self.subject_refinement,
        );
        if component.invert {
            for value in &mut coverage {
                *value = 1.0 - *value;
            }
        }
        coverage
            .into_iter()
            .map(|value| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
            .collect()
    }
}

fn crop_mask_image(source: &MaskImage, u0: f32, v0: f32, du: f32, dv: f32) -> MaskImage {
    let mut cropped = source.clone();
    cropped.sampling_rect = crop_sampling_rect(source.sampling_rect, u0, v0, du, dv);
    cropped
}

fn crop_rgb_image(source: &MaskRgbImage, u0: f32, v0: f32, du: f32, dv: f32) -> MaskRgbImage {
    let mut cropped = source.clone();
    cropped.sampling_rect = crop_sampling_rect(source.sampling_rect, u0, v0, du, dv);
    cropped
}

fn crop_sampling_rect(source: [f32; 4], u0: f32, v0: f32, du: f32, dv: f32) -> [f32; 4] {
    let source_width = source[2] - source[0];
    let source_height = source[3] - source[1];
    [
        source[0] + u0 * source_width,
        source[1] + v0 * source_height,
        source[0] + (u0 + du) * source_width,
        source[1] + (v0 + dv) * source_height,
    ]
}

fn set_if_changed<T: PartialEq>(slot: &mut T, value: T) -> bool {
    if *slot == value {
        false
    } else {
        *slot = value;
        true
    }
}

fn copied_name(base: &str, exists: impl Fn(&str) -> bool) -> String {
    for number in 1..=10_000usize {
        let candidate = if number == 1 {
            format!("{base} Copy")
        } else {
            format!("{base} Copy {number}")
        };
        if !exists(&candidate) {
            return candidate;
        }
    }
    format!("{base} Copy")
}

fn moved_index(selected: usize, from: usize, to: usize) -> usize {
    if selected == from {
        to
    } else if from < to && selected > from && selected <= to {
        selected - 1
    } else if from > to && selected >= to && selected < from {
        selected + 1
    } else {
        selected
    }
}

fn rasterize_component(
    component: &MaskComponent,
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
    subject_refinement: &SubjectRefinement,
) -> Vec<f32> {
    let space = MaskRasterSpace::new(width, height, image_width, image_height);
    match &component.geometry {
        MaskGeometry::Fullscreen => vec![1.0; width as usize * height as usize],
        MaskGeometry::Brush {
            overlap_enabled,
            stroke_starts,
            dabs,
            ..
        } => rasterize_recorded_brush(space, dabs, *overlap_enabled, stroke_starts),
        MaskGeometry::Radial {
            center,
            radius,
            rotation,
            feather,
            initialized: true,
        } => rasterize_radial(space, *center, *radius, *rotation, *feather),
        MaskGeometry::Linear {
            start,
            end,
            feather,
            initialized: true,
        } => rasterize_linear(space, *start, *end, *feather),
        MaskGeometry::Ai {
            mask: Some(mask),
            grow,
            feather,
        } => {
            let mut coverage = rasterize_mask_image(width, height, mask);
            if matches!(component.kind, MaskKind::Subject | MaskKind::Background)
                && !subject_refinement.is_empty()
            {
                let delta = rasterize_subject_refinement_delta(space, subject_refinement);
                coverage.par_iter_mut().zip(delta.into_par_iter()).for_each(
                    |(probability, delta)| {
                        *probability = (*probability + delta).clamp(0.0, 1.0);
                    },
                );
            }
            let grow = if component.kind == MaskKind::Background {
                -*grow
            } else {
                *grow
            };
            shape_probability_mask(&mut coverage, width, height, grow, *feather);
            if component.kind == MaskKind::Background {
                coverage
                    .par_iter_mut()
                    .for_each(|value| *value = 1.0 - *value);
            }
            coverage
        }
        MaskGeometry::Object {
            mask: Some(mask),
            grow,
            feather,
            ..
        } => {
            let mut coverage = rasterize_mask_image(width, height, mask);
            shape_probability_mask(&mut coverage, width, height, *grow, *feather);
            coverage
        }
        MaskGeometry::Object {
            mask: None,
            brush_size,
            strokes,
            ..
        } => {
            let dabs = object_prompt_dabs(strokes, *brush_size);
            rasterize_brush(space, &dabs)
        }
        MaskGeometry::LuminanceRange {
            source: Some(source),
            low,
            high,
            grow,
            feather,
        } => {
            let mut coverage =
                rasterize_luminance_range(width, height, source, *low, *high, *feather);
            if grow.abs() > 1e-5 {
                shape_probability_mask(&mut coverage, width, height, *grow, 0.0);
            }
            coverage
        }
        MaskGeometry::ColorRange {
            source: Some(source),
            sample,
            tolerance,
            grow,
            feather,
            sampled: true,
        } => {
            let mut coverage =
                rasterize_color_range(width, height, source, *sample, *tolerance, *feather);
            if grow.abs() > 1e-5 {
                shape_probability_mask(&mut coverage, width, height, *grow, 0.0);
            }
            coverage
        }
        _ => vec![0.0; width as usize * height as usize],
    }
}

fn rasterize_mask_image(width: u32, height: u32, mask: &MaskImage) -> Vec<f32> {
    if width == 0 || height == 0 || mask.width == 0 || mask.height == 0 {
        return vec![0.0; width as usize * height as usize];
    }
    let row_stride = width as usize;
    let mut out = vec![0.0; row_stride * height as usize];
    out.par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, row)| {
            let sample_v = mask.sampling_rect[1]
                + (y as f32 + 0.5) / height as f32
                    * (mask.sampling_rect[3] - mask.sampling_rect[1]);
            let source_y = (sample_v * mask.height as f32 - 0.5)
                .clamp(0.0, mask.height.saturating_sub(1) as f32);
            let y0 = source_y.floor() as usize;
            let y1 = (y0 + 1).min(mask.height as usize - 1);
            let fy = source_y - y0 as f32;
            for (x, value) in row.iter_mut().enumerate() {
                let sample_u = mask.sampling_rect[0]
                    + (x as f32 + 0.5) / width as f32
                        * (mask.sampling_rect[2] - mask.sampling_rect[0]);
                let source_x = (sample_u * mask.width as f32 - 0.5)
                    .clamp(0.0, mask.width.saturating_sub(1) as f32);
                let x0 = source_x.floor() as usize;
                let x1 = (x0 + 1).min(mask.width as usize - 1);
                let fx = source_x - x0 as f32;
                let sample = |sx: usize, sy: usize| {
                    mask.pixels[sy * mask.width as usize + sx] as f32 / 255.0
                };
                let top = sample(x0, y0) + (sample(x1, y0) - sample(x0, y0)) * fx;
                let bottom = sample(x0, y1) + (sample(x1, y1) - sample(x0, y1)) * fx;
                *value = top + (bottom - top) * fy;
            }
        });
    out
}

fn component_shape_margin_pixels(component: &MaskComponent, image_edge: f32) -> f32 {
    let shape_margin = |grow: f32, feather: f32| {
        grow.abs().clamp(0.0, 1.0) * image_edge * 0.05
            + feather.clamp(0.0, 1.0).powf(1.30) * image_edge * 0.045
            + 2.0
    };
    match &component.geometry {
        MaskGeometry::Ai { grow, feather, .. } | MaskGeometry::Object { grow, feather, .. } => {
            shape_margin(*grow, *feather)
        }
        MaskGeometry::LuminanceRange { grow, .. } | MaskGeometry::ColorRange { grow, .. } => {
            shape_margin(*grow, 0.0)
        }
        _ => 2.0,
    }
}

fn chamfer_distance(binary: &[u8], width: usize, height: usize, target: u8) -> Vec<f32> {
    const INF: f32 = 1.0e20;
    const DIAGONAL: f32 = std::f32::consts::SQRT_2;
    let mut distance = binary
        .iter()
        .map(|value| if *value == target { 0.0 } else { INF })
        .collect::<Vec<_>>();

    for y in 0..height {
        for x in 0..width {
            let index = y * width + x;
            let mut best = distance[index];
            if x > 0 {
                best = best.min(distance[index - 1] + 1.0);
            }
            if y > 0 {
                best = best.min(distance[index - width] + 1.0);
                if x > 0 {
                    best = best.min(distance[index - width - 1] + DIAGONAL);
                }
                if x + 1 < width {
                    best = best.min(distance[index - width + 1] + DIAGONAL);
                }
            }
            distance[index] = best;
        }
    }
    for y in (0..height).rev() {
        for x in (0..width).rev() {
            let index = y * width + x;
            let mut best = distance[index];
            if x + 1 < width {
                best = best.min(distance[index + 1] + 1.0);
            }
            if y + 1 < height {
                best = best.min(distance[index + width] + 1.0);
                if x > 0 {
                    best = best.min(distance[index + width - 1] + DIAGONAL);
                }
                if x + 1 < width {
                    best = best.min(distance[index + width + 1] + DIAGONAL);
                }
            }
            distance[index] = best;
        }
    }
    distance
}

fn shape_probability_mask(mask: &mut [f32], width: u32, height: u32, grow: f32, feather: f32) {
    if width == 0 || height == 0 || mask.is_empty() {
        return;
    }

    let grow = grow.clamp(-32.0, 32.0);
    let feather = feather.clamp(0.0, 32.0);
    if grow.abs() <= 1e-5 && feather <= 1e-5 {
        mask.par_iter_mut()
            .for_each(|value| *value = value.clamp(0.0, 1.0));
        return;
    }

    // Generated masks already contain a subpixel anti-aliased boundary. Blur
    // that alpha directly when only Feather is requested; rebuilding it from a
    // binary distance field creates visible atlas-resolution stair-steps.
    if grow.abs() <= 1e-5 {
        let edge = width.min(height) as f32;
        // Whole-pixel radii keep full-frame and export-tile crops bit-identical.
        // Subpixel radii remain continuous near zero, where that progression is
        // most visible in the UI.
        let raw_radius = feather.powf(1.30) * edge * 0.045;
        let feather_radius = if raw_radius < 1.0 {
            raw_radius
        } else {
            raw_radius.round()
        };
        blur_probability_mask(mask, width as usize, height as usize, feather_radius);
        return;
    }

    let width = width as usize;
    let height = height as usize;
    let binary = mask
        .iter()
        .map(|value| u8::from(*value >= 0.5))
        .collect::<Vec<_>>();
    let distance_to_inside = chamfer_distance(&binary, width, height, 1);
    let distance_to_outside = chamfer_distance(&binary, width, height, 0);
    let edge = width.min(height) as f32;
    let grow_radius = grow * edge * 0.05;
    let feather_radius = (feather.powf(1.30) * edge * 0.045).max(0.75);

    mask.par_iter_mut().enumerate().for_each(|(index, value)| {
        let confidence_offset = (*value - 0.5) * 0.5;
        let signed_distance = distance_to_outside[index] - distance_to_inside[index]
            + confidence_offset
            + grow_radius;
        *value = if feather <= 1e-5 {
            smoothstep(-0.75, 0.75, signed_distance)
        } else {
            smoothstep(-feather_radius, feather_radius, signed_distance)
        };
    });
}

fn blur_probability_mask(mask: &mut [f32], width: usize, height: usize, radius: f32) {
    if width == 0 || height == 0 || radius <= 1e-5 {
        return;
    }
    let selected = mask.iter().map(|value| *value >= 0.5).collect::<Vec<_>>();
    let integer = radius.floor() as usize;
    let fraction = radius - integer as f32;
    let mut horizontal = vec![0.0; mask.len()];

    for y in 0..height {
        let row = &mask[y * width..(y + 1) * width];
        let mut sum = row[..=integer.min(width - 1)].iter().sum::<f32>();
        for x in 0..width {
            let left = x.saturating_sub(integer);
            let right = (x + integer).min(width - 1);
            let mut weighted = sum;
            let mut weight = (right - left + 1) as f32;
            if fraction > 0.0 {
                if let Some(extra) = x.checked_sub(integer + 1) {
                    weighted += row[extra] * fraction;
                    weight += fraction;
                }
                if x + integer + 1 < width {
                    weighted += row[x + integer + 1] * fraction;
                    weight += fraction;
                }
            }
            horizontal[y * width + x] = weighted / weight;
            if x >= integer {
                sum -= row[x - integer];
            }
            if x + integer + 1 < width {
                sum += row[x + integer + 1];
            }
        }
    }

    for x in 0..width {
        let mut sum = (0..=integer.min(height - 1))
            .map(|y| horizontal[y * width + x])
            .sum::<f32>();
        for y in 0..height {
            let top = y.saturating_sub(integer);
            let bottom = (y + integer).min(height - 1);
            let mut weighted = sum;
            let mut weight = (bottom - top + 1) as f32;
            if fraction > 0.0 {
                if let Some(extra) = y.checked_sub(integer + 1) {
                    weighted += horizontal[extra * width + x] * fraction;
                    weight += fraction;
                }
                if y + integer + 1 < height {
                    weighted += horizontal[(y + integer + 1) * width + x] * fraction;
                    weight += fraction;
                }
            }
            mask[y * width + x] = (weighted / weight).clamp(0.0, 1.0);
            if y >= integer {
                sum -= horizontal[(y - integer) * width + x];
            }
            if y + integer + 1 < height {
                sum += horizontal[(y + integer + 1) * width + x];
            }
        }
    }
    mask.iter_mut().zip(selected).for_each(|(value, selected)| {
        if selected {
            *value = (value.max(0.5) + 1e-6).min(1.0);
        } else {
            *value = (value.min(0.5) - 1e-6).max(0.0);
        }
    });
}

fn rasterize_luminance_range(
    width: u32,
    height: u32,
    source: &MaskRgbImage,
    low: f32,
    high: f32,
    feather: f32,
) -> Vec<f32> {
    let low = low.min(high).clamp(0.0, 1.0);
    let high = high.max(low).clamp(0.0, 1.0);
    let transition = feather.clamp(0.001, 1.0) * 0.35;
    sample_rgb_mask(width, height, source, |rgb| {
        let linear = rgb.map(srgb_to_linear);
        let luminance = 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
        let enter = smoothstep(low - transition, low, luminance);
        let leave = 1.0 - smoothstep(high, high + transition, luminance);
        enter * leave
    })
}

fn rasterize_color_range(
    width: u32,
    height: u32,
    source: &MaskRgbImage,
    sample: [f32; 3],
    tolerance: f32,
    feather: f32,
) -> Vec<f32> {
    let target = linear_srgb_to_oklab(sample.map(srgb_to_linear));
    let tolerance = tolerance.clamp(0.005, 1.0) * 0.42;
    let softness = feather.clamp(0.0, 1.0) * tolerance.max(0.01);
    sample_rgb_mask(width, height, source, |rgb| {
        let color = linear_srgb_to_oklab(rgb.map(srgb_to_linear));
        let distance = ((color[0] - target[0]).powi(2)
            + (color[1] - target[1]).powi(2)
            + (color[2] - target[2]).powi(2))
        .sqrt();
        1.0 - smoothstep(
            (tolerance - softness).max(0.0),
            tolerance + softness,
            distance,
        )
    })
}

fn sample_rgb_mask(
    width: u32,
    height: u32,
    source: &MaskRgbImage,
    coverage: impl Fn([f32; 3]) -> f32 + Sync,
) -> Vec<f32> {
    if width == 0 || height == 0 || source.width == 0 || source.height == 0 {
        return vec![0.0; width as usize * height as usize];
    }
    let row_stride = width as usize;
    let mut out = vec![0.0; row_stride * height as usize];
    out.par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, row)| {
            let sample_v = source.sampling_rect[1]
                + (y as f32 + 0.5) / height.max(1) as f32
                    * (source.sampling_rect[3] - source.sampling_rect[1]);
            let source_y = (sample_v * source.height as f32 - 0.5)
                .clamp(0.0, source.height.saturating_sub(1) as f32);
            let y0 = source_y.floor() as usize;
            let y1 = (y0 + 1).min(source.height as usize - 1);
            let fy = source_y - y0 as f32;
            for (x, value) in row.iter_mut().enumerate() {
                let sample_u = source.sampling_rect[0]
                    + (x as f32 + 0.5) / width.max(1) as f32
                        * (source.sampling_rect[2] - source.sampling_rect[0]);
                let source_x = (sample_u * source.width as f32 - 0.5)
                    .clamp(0.0, source.width.saturating_sub(1) as f32);
                let x0 = source_x.floor() as usize;
                let x1 = (x0 + 1).min(source.width as usize - 1);
                let fx = source_x - x0 as f32;
                let sample = |sx: usize, sy: usize, channel: usize| {
                    source.rgba[(sy * source.width as usize + sx) * 4 + channel] as f32 / 255.0
                };
                let rgb = std::array::from_fn(|channel| {
                    let top = sample(x0, y0, channel)
                        + (sample(x1, y0, channel) - sample(x0, y0, channel)) * fx;
                    let bottom = sample(x0, y1, channel)
                        + (sample(x1, y1, channel) - sample(x0, y1, channel)) * fx;
                    top + (bottom - top) * fy
                });
                *value = coverage(rgb).clamp(0.0, 1.0);
            }
        });
    out
}

fn srgb_to_linear(value: f32) -> f32 {
    if value <= 0.04045 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_srgb_to_oklab(rgb: [f32; 3]) -> [f32; 3] {
    let l = 0.412_221_46 * rgb[0] + 0.536_332_55 * rgb[1] + 0.051_445_995 * rgb[2];
    let m = 0.211_903_5 * rgb[0] + 0.680_699_5 * rgb[1] + 0.107_396_96 * rgb[2];
    let s = 0.088_302_46 * rgb[0] + 0.281_718_85 * rgb[1] + 0.629_978_7 * rgb[2];
    let l = l.cbrt();
    let m = m.cbrt();
    let s = s.cbrt();
    [
        0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s,
        1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s,
        0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s,
    ]
}

fn object_prompt_dabs(strokes: &[ObjectStroke], size: f32) -> Vec<BrushDab> {
    let dab_count = strokes.iter().map(|stroke| stroke.points.len()).sum();
    let mut dabs = Vec::with_capacity(dab_count);
    for stroke in strokes {
        let opacity = if stroke.positive { 1.0 } else { -1.0 };
        let captured_size = if stroke.brush_size > 0.0 {
            stroke.brush_size
        } else {
            size
        };
        dabs.extend(stroke.points.iter().copied().map(|center| BrushDab {
            center,
            opacity,
            size: captured_size,
            feather: 0.0,
        }));
    }
    dabs
}

#[derive(Clone, Copy)]
struct MaskRasterSpace {
    raster: [u32; 2],
    image: [u32; 2],
}

impl MaskRasterSpace {
    const fn new(width: u32, height: u32, image_width: u32, image_height: u32) -> Self {
        Self {
            raster: [width, height],
            image: [image_width, image_height],
        }
    }
}

#[derive(Clone, Copy)]
struct BrushRasterSpec {
    opacity: f32,
    center_x: f32,
    center_y: f32,
    radius_x: f32,
    radius_y: f32,
    min_x: i32,
    max_x: i32,
    min_y: i32,
    max_y: i32,
    antialias: f32,
    inner: f32,
}

pub fn rasterize_brush_dabs(
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
    dabs: &[BrushDab],
) -> Vec<u8> {
    rasterize_brush(
        MaskRasterSpace::new(width, height, image_width, image_height),
        dabs,
    )
    .into_iter()
    .map(|value| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8)
    .collect()
}

fn rasterize_brush(space: MaskRasterSpace, dabs: &[BrushDab]) -> Vec<f32> {
    let [width, height] = space.raster;
    if width == 0 || height == 0 || dabs.is_empty() {
        return vec![0.0; width as usize * height as usize];
    }

    let specs = brush_raster_specs(space, dabs);

    const ROW_BAND_HEIGHT: usize = 64;
    let row_stride = width as usize;
    let mut out = vec![0.0f32; row_stride * height as usize];
    out.par_chunks_mut(row_stride * ROW_BAND_HEIGHT)
        .enumerate()
        .for_each(|(band_index, band)| {
            let band_start_y = band_index * ROW_BAND_HEIGHT;
            let band_height = band.len() / row_stride;
            let band_end_y = band_start_y + band_height - 1;

            for spec in &specs {
                if spec.max_y < band_start_y as i32 || spec.min_y > band_end_y as i32 {
                    continue;
                }
                let min_y = spec.min_y.max(band_start_y as i32);
                let max_y = spec.max_y.min(band_end_y as i32);
                for y in min_y..=max_y {
                    let dy = (y as f32 + 0.5 - spec.center_y) / spec.radius_y.max(0.5);
                    let row_offset = (y as usize - band_start_y) * row_stride;
                    for x in spec.min_x..=spec.max_x {
                        let dx = (x as f32 + 0.5 - spec.center_x) / spec.radius_x.max(0.5);
                        let distance = (dx * dx + dy * dy).sqrt();
                        if distance >= 1.0 + spec.antialias {
                            continue;
                        }
                        let coverage = 1.0 - smoothstep(spec.inner, 1.0 + spec.antialias, distance);
                        let index = row_offset + x as usize;
                        if spec.opacity >= 0.0 {
                            band[index] = band[index].max(coverage * spec.opacity.clamp(0.0, 1.0));
                        } else {
                            band[index] *= 1.0 - coverage * (-spec.opacity).clamp(0.0, 1.0);
                        }
                    }
                }
            }
        });
    out
}

fn brush_raster_specs(space: MaskRasterSpace, dabs: &[BrushDab]) -> Vec<BrushRasterSpec> {
    let [width, height] = space.raster;
    let [image_width, image_height] = space.image;
    let image_min = image_width.min(image_height).max(1) as f32;
    let mut specs = Vec::with_capacity(dabs.len());
    for dab in dabs {
        let radius_image = dab.size.clamp(f32::EPSILON, 0.5) * image_min;
        let radius_x = radius_image * width as f32 / image_width.max(1) as f32;
        let radius_y = radius_image * height as f32 / image_height.max(1) as f32;
        let bbox_x = radius_x.ceil().max(1.0) as i32 + 1;
        let bbox_y = radius_y.ceil().max(1.0) as i32 + 1;
        let feather = dab.feather.clamp(0.0, 1.0);
        let center_x = dab.center[0] * width as f32;
        let center_y = dab.center[1] * height as f32;
        let min_x = (center_x.floor() as i32 - bbox_x).max(0);
        let max_x = (center_x.ceil() as i32 + bbox_x).min(width as i32 - 1);
        let min_y = (center_y.floor() as i32 - bbox_y).max(0);
        let max_y = (center_y.ceil() as i32 + bbox_y).min(height as i32 - 1);
        let antialias = (1.0 / radius_x.max(radius_y).max(1.0)).clamp(0.002, 0.25);
        let inner = (1.0 - feather).clamp(0.0, 1.0 - antialias);
        specs.push(BrushRasterSpec {
            opacity: dab.opacity,
            center_x,
            center_y,
            radius_x,
            radius_y,
            min_x,
            max_x,
            min_y,
            max_y,
            antialias,
            inner,
        });
    }
    specs
}

#[derive(Clone, Copy)]
struct BrushStrokeGroup {
    start: usize,
    end: usize,
    positive: bool,
}

fn recorded_brush_groups(
    dabs: &[BrushDab],
    stroke_starts: &[usize],
) -> (usize, Vec<BrushStrokeGroup>) {
    let mut starts = stroke_starts
        .iter()
        .copied()
        .filter(|&start| start < dabs.len())
        .collect::<Vec<_>>();
    starts.sort_unstable();
    starts.dedup();
    let Some(&legacy_end) = starts.first() else {
        return (dabs.len(), Vec::new());
    };

    let mut groups = Vec::with_capacity(starts.len());
    for (stroke_index, &stroke_start) in starts.iter().enumerate() {
        let stroke_end = starts.get(stroke_index + 1).copied().unwrap_or(dabs.len());
        let mut group_start = stroke_start;
        let mut positive = dabs[group_start].opacity >= 0.0;
        for (offset, dab) in dabs[stroke_start + 1..stroke_end].iter().enumerate() {
            let dab_index = stroke_start + 1 + offset;
            let next_positive = dab.opacity >= 0.0;
            if next_positive != positive {
                groups.push(BrushStrokeGroup {
                    start: group_start - legacy_end,
                    end: dab_index - legacy_end,
                    positive,
                });
                group_start = dab_index;
                positive = next_positive;
            }
        }
        groups.push(BrushStrokeGroup {
            start: group_start - legacy_end,
            end: stroke_end - legacy_end,
            positive,
        });
    }
    (legacy_end, groups)
}

fn rasterize_recorded_brush(
    space: MaskRasterSpace,
    dabs: &[BrushDab],
    overlap_enabled: bool,
    stroke_starts: &[usize],
) -> Vec<f32> {
    let [width, height] = space.raster;
    if width == 0 || height == 0 || dabs.is_empty() {
        return vec![0.0; width as usize * height as usize];
    }

    let (legacy_end, groups) = recorded_brush_groups(dabs, stroke_starts);
    if groups.is_empty() {
        return rasterize_brush(space, dabs);
    }

    let mut out = rasterize_brush(space, &dabs[..legacy_end]);
    let specs = brush_raster_specs(space, &dabs[legacy_end..]);

    const ROW_BAND_HEIGHT: usize = 64;
    let row_stride = width as usize;
    out.par_chunks_mut(row_stride * ROW_BAND_HEIGHT)
        .enumerate()
        .for_each(|(band_index, band)| {
            let band_start_y = band_index * ROW_BAND_HEIGHT;
            let band_height = band.len() / row_stride;
            let band_end_y = band_start_y + band_height - 1;
            let mut stroke_coverage = vec![0.0f32; band.len()];
            let mut touched = Vec::new();

            for group in &groups {
                for spec in &specs[group.start..group.end] {
                    if spec.max_y < band_start_y as i32 || spec.min_y > band_end_y as i32 {
                        continue;
                    }
                    let min_y = spec.min_y.max(band_start_y as i32);
                    let max_y = spec.max_y.min(band_end_y as i32);
                    for y in min_y..=max_y {
                        let dy = (y as f32 + 0.5 - spec.center_y) / spec.radius_y.max(0.5);
                        let row_offset = (y as usize - band_start_y) * row_stride;
                        for x in spec.min_x..=spec.max_x {
                            let dx = (x as f32 + 0.5 - spec.center_x) / spec.radius_x.max(0.5);
                            let distance = (dx * dx + dy * dy).sqrt();
                            if distance >= 1.0 + spec.antialias {
                                continue;
                            }
                            let coverage =
                                1.0 - smoothstep(spec.inner, 1.0 + spec.antialias, distance);
                            let alpha = coverage * spec.opacity.abs().clamp(0.0, 1.0);
                            let index = row_offset + x as usize;
                            if alpha > stroke_coverage[index] {
                                if stroke_coverage[index] == 0.0 {
                                    touched.push(index);
                                }
                                stroke_coverage[index] = alpha;
                            }
                        }
                    }
                }

                for index in touched.drain(..) {
                    let alpha = stroke_coverage[index];
                    if group.positive {
                        band[index] = if overlap_enabled {
                            band[index] + alpha * (1.0 - band[index])
                        } else {
                            band[index].max(alpha)
                        };
                    } else {
                        band[index] *= 1.0 - alpha;
                    }
                    stroke_coverage[index] = 0.0;
                }
            }
        });
    out
}

fn rasterize_subject_refinement_delta(
    space: MaskRasterSpace,
    refinement: &SubjectRefinement,
) -> Vec<f32> {
    let [width, height] = space.raster;
    if width == 0 || height == 0 || refinement.dabs.is_empty() {
        return vec![0.0; width as usize * height as usize];
    }

    let (legacy_end, groups) = recorded_brush_groups(&refinement.dabs, &refinement.stroke_starts);
    let specs = brush_raster_specs(space, &refinement.dabs);
    const ROW_BAND_HEIGHT: usize = 64;
    let row_stride = width as usize;
    let mut out = vec![0.0f32; row_stride * height as usize];

    out.par_chunks_mut(row_stride * ROW_BAND_HEIGHT)
        .enumerate()
        .for_each(|(band_index, band)| {
            let band_start_y = band_index * ROW_BAND_HEIGHT;
            let band_height = band.len() / row_stride;
            let band_end_y = band_start_y + band_height - 1;

            let apply_spec = |band: &mut [f32], spec: &BrushRasterSpec| {
                if spec.max_y < band_start_y as i32 || spec.min_y > band_end_y as i32 {
                    return;
                }
                let min_y = spec.min_y.max(band_start_y as i32);
                let max_y = spec.max_y.min(band_end_y as i32);
                for y in min_y..=max_y {
                    let dy = (y as f32 + 0.5 - spec.center_y) / spec.radius_y.max(0.5);
                    let row_offset = (y as usize - band_start_y) * row_stride;
                    for x in spec.min_x..=spec.max_x {
                        let dx = (x as f32 + 0.5 - spec.center_x) / spec.radius_x.max(0.5);
                        let distance = (dx * dx + dy * dy).sqrt();
                        if distance >= 1.0 + spec.antialias {
                            continue;
                        }
                        let coverage = 1.0 - smoothstep(spec.inner, 1.0 + spec.antialias, distance);
                        let index = row_offset + x as usize;
                        band[index] = (band[index] + coverage * spec.opacity.clamp(-1.0, 1.0))
                            .clamp(-1.0, 1.0);
                    }
                }
            };

            for spec in &specs[..legacy_end] {
                apply_spec(band, spec);
            }

            let grouped_specs = &specs[legacy_end..];
            let mut stroke_coverage = vec![0.0f32; band.len()];
            let mut touched = Vec::new();
            for group in &groups {
                for spec in &grouped_specs[group.start..group.end] {
                    if spec.max_y < band_start_y as i32 || spec.min_y > band_end_y as i32 {
                        continue;
                    }
                    let min_y = spec.min_y.max(band_start_y as i32);
                    let max_y = spec.max_y.min(band_end_y as i32);
                    for y in min_y..=max_y {
                        let dy = (y as f32 + 0.5 - spec.center_y) / spec.radius_y.max(0.5);
                        let row_offset = (y as usize - band_start_y) * row_stride;
                        for x in spec.min_x..=spec.max_x {
                            let dx = (x as f32 + 0.5 - spec.center_x) / spec.radius_x.max(0.5);
                            let distance = (dx * dx + dy * dy).sqrt();
                            if distance >= 1.0 + spec.antialias {
                                continue;
                            }
                            let coverage =
                                1.0 - smoothstep(spec.inner, 1.0 + spec.antialias, distance);
                            let alpha = coverage * spec.opacity.abs().clamp(0.0, 1.0);
                            let index = row_offset + x as usize;
                            if alpha > stroke_coverage[index] {
                                if stroke_coverage[index] == 0.0 {
                                    touched.push(index);
                                }
                                stroke_coverage[index] = alpha;
                            }
                        }
                    }
                }
                let sign = if group.positive { 1.0 } else { -1.0 };
                for index in touched.drain(..) {
                    band[index] = (band[index] + sign * stroke_coverage[index]).clamp(-1.0, 1.0);
                    stroke_coverage[index] = 0.0;
                }
            }
        });
    out
}

fn rasterize_radial(
    space: MaskRasterSpace,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    feather: f32,
) -> Vec<f32> {
    let [width, height] = space.raster;
    let [image_width, image_height] = space.image;
    let row_stride = width as usize;
    let mut out = vec![0.0f32; row_stride * height as usize];
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    let rx = (radius[0].abs() * image_width.max(1) as f32).max(1.0);
    let ry = (radius[1].abs() * image_height.max(1) as f32).max(1.0);
    let inner = (1.0 - feather.clamp(0.0, 1.0) * 0.98).clamp(0.0, 0.995);

    out.par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, row)| {
            let v = (y as f32 + 0.5) / height as f32;
            let dy = (v - center[1]) * image_height.max(1) as f32;
            for (x, value) in row.iter_mut().enumerate() {
                let u = (x as f32 + 0.5) / width as f32;
                let dx = (u - center[0]) * image_width.max(1) as f32;
                let local_x = cos_r * dx + sin_r * dy;
                let local_y = -sin_r * dx + cos_r * dy;
                let distance = ((local_x / rx).powi(2) + (local_y / ry).powi(2)).sqrt();
                *value = 1.0 - smoothstep(inner, 1.0, distance);
            }
        });
    out
}

fn rasterize_linear(
    space: MaskRasterSpace,
    start: [f32; 2],
    end: [f32; 2],
    feather: f32,
) -> Vec<f32> {
    let [width, height] = space.raster;
    let [image_width, image_height] = space.image;
    let row_stride = width as usize;
    let mut out = vec![0.0f32; row_stride * height as usize];
    let sx = start[0] * image_width.max(1) as f32;
    let sy = start[1] * image_height.max(1) as f32;
    let dx = (end[0] - start[0]) * image_width.max(1) as f32;
    let dy = (end[1] - start[1]) * image_height.max(1) as f32;
    let length_sq = (dx * dx + dy * dy).max(1.0);
    let width_factor = feather.clamp(0.02, 1.0);
    let edge0 = 0.5 - 0.5 * width_factor;
    let edge1 = 0.5 + 0.5 * width_factor;

    out.par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, row)| {
            let py = (y as f32 + 0.5) / height as f32 * image_height.max(1) as f32;
            for (x, value) in row.iter_mut().enumerate() {
                let px = (x as f32 + 0.5) / width as f32 * image_width.max(1) as f32;
                let t = ((px - sx) * dx + (py - sy) * dy) / length_sq;
                *value = 1.0 - smoothstep(edge0, edge1, t);
            }
        });
    out
}

fn smoothstep(edge0: f32, edge1: f32, value: f32) -> f32 {
    if edge1 <= edge0 {
        return if value < edge0 { 0.0 } else { 1.0 };
    }
    let t = ((value - edge0) / (edge1 - edge0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

pub fn ellipse_outline_points(
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    segments: usize,
) -> Vec<[f32; 2]> {
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    (0..=segments.max(12))
        .map(|index| {
            let angle = TAU * index as f32 / segments.max(12) as f32;
            let x = radius[0] * angle.cos();
            let y = radius[1] * angle.sin();
            [
                center[0] + cos_r * x - sin_r * y,
                center[1] + sin_r * x + cos_r * y,
            ]
        })
        .collect()
}

#[cfg(test)]
mod tests;
