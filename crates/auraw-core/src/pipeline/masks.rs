use half::f16;
use rayon::prelude::*;
use std::f32::consts::TAU;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

mod effects;

pub use effects::{
    BlurEffectSettings, EdgeGlowEffectSettings, FogEffectSettings, GlowEffectSettings,
    LensBlurEffectSettings, LightRaysEffectSettings, MaskEffect, MaskEffectCategory,
    MaskEffectSettings, MotionBlurEffectSettings, NeonEffectSettings, PixelateEffectSettings,
    RadialBlurEffectSettings, RadialBlurMode, SmokeEffectSettings, TiltShiftEffectSettings,
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

    /// Returns the signed opacity captured by a newly emitted brush dab.
    /// Positive values paint and negative values erase. Keeping this value on
    /// each dab means later tool-setting changes never alter existing strokes.
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
    /// Positive dabs paint; negative dabs erase.
    pub opacity: f32,
    /// Captured when the dab is painted so changing the tool does not reshape
    /// previous strokes. Radius is relative to the shorter image edge.
    pub size: f32,
    pub feather: f32,
}

/// Shared, non-destructive correction layer for BiRefNet subject probability.
///
/// The stored dabs are resolution independent and always describe corrections
/// to the *subject* probability. Positive opacity paints subject; negative
/// opacity paints background. Subject and Not Subject components consume this
/// same layer, so the latter can remain the exact complement of the former.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct SubjectRefinement {
    /// Radius as a fraction of the image's shorter edge for newly drawn dabs.
    #[serde(default = "default_subject_refinement_size")]
    pub size: f32,
    #[serde(default = "default_subject_refinement_feather")]
    pub feather: f32,
    /// Signed dab magnitude is captured when painted; this is the default for
    /// newly emitted dabs only and changing it never alters existing strokes.
    #[serde(default = "default_subject_refinement_flow")]
    pub flow: f32,
    /// Dab indexes that begin pointer/touch strokes. Within one continuous
    /// stroke, overlapping dabs collapse to one coverage field so a slow drag
    /// does not accidentally become stronger than a fast drag.
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

    /// Applies the stored signed delta to a raw 8-bit AI probability map.
    /// This helper is useful for non-atlas consumers; the normal mask pipeline
    /// applies the same math directly at its target raster resolution.
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
    /// Normalized full-image coordinates. Positive strokes add foreground;
    /// negative strokes explicitly mark background.
    pub points: Vec<[f32; 2]>,
    pub positive: bool,
    /// Image-relative radius captured when the stroke starts. New code stores
    /// the zoom-adjusted value so the tool remains a constant on-screen size.
    /// Zero means a legacy sidecar and falls back to the component tool size.
    #[serde(default)]
    pub brush_size: f32,
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskImage {
    pub width: u32,
    pub height: u32,
    #[serde(with = "base64_arc_bytes")]
    pub pixels: Arc<[u8]>,
    /// Transient normalized view into `pixels`. Cropped preview/export stacks
    /// share the original matte and change only this sampling transform, which
    /// keeps sub-pixel alignment identical across adjacent tiles.
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

#[derive(Clone, Debug, PartialEq)]
pub struct InpaintLayer {
    /// Sparse, ordered full-image-coordinate patches. Later patches overwrite
    /// earlier patches where they overlap.
    pub patches: Arc<[InpaintPatch]>,
}

impl InpaintLayer {
    pub fn new(patches: Vec<InpaintPatch>) -> Option<Self> {
        (!patches.is_empty() && patches.iter().all(InpaintPatch::is_valid)).then(|| Self {
            patches: patches.into(),
        })
    }
}

/// Compact persisted result for one released inpainting brush stroke.
///
/// Replacement pixels use scene-linear Rec.2020 RGBA16F. Placement remains in
/// full-resolution source coordinates, while newer patches may retain LaMa's
/// smaller native raster instead of persisting a redundant upscale. `rgba` is
/// kept for backward compatibility with early 8-bit sRGB patches.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct InpaintPatch {
    pub source_width: u32,
    pub source_height: u32,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Zero means the stored raster has the legacy `width` by `height` layout.
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub raster_width: u32,
    #[serde(default, skip_serializing_if = "u32_is_zero")]
    pub raster_height: u32,
    /// Zero identifies RGBA16F patches written by the former camera-RGB
    /// capture bug. Version one is neutral scene-linear Rec.2020.
    #[serde(default)]
    pub working_space_version: u8,
    #[serde(
        default,
        with = "base64_arc_u16",
        skip_serializing_if = "arc_u16_is_empty"
    )]
    pub rgba16f: Arc<[u16]>,
    /// Legacy AuRaw 2.0 8-bit sRGB storage. New patches leave this empty.
    #[serde(
        default,
        with = "compressed_base64_arc_bytes",
        skip_serializing_if = "arc_u8_is_empty"
    )]
    pub rgba: Arc<[u8]>,
    #[serde(with = "compressed_base64_arc_bytes")]
    pub mask: Arc<[u8]>,
}

fn arc_u16_is_empty(values: &Arc<[u16]>) -> bool {
    values.is_empty()
}

fn arc_u8_is_empty(values: &Arc<[u8]>) -> bool {
    values.is_empty()
}

fn u32_is_zero(value: &u32) -> bool {
    *value == 0
}

impl InpaintPatch {
    #[allow(clippy::too_many_arguments)]
    pub fn new_linear(
        source_width: u32,
        source_height: u32,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        rgba16f: Vec<u16>,
        mask: Vec<u8>,
    ) -> Option<Self> {
        Self::new_linear_resampled(
            [source_width, source_height],
            [x, y],
            [width, height],
            [width, height],
            rgba16f,
            mask,
        )
    }

    pub fn new_linear_resampled(
        source_dimensions: [u32; 2],
        origin: [u32; 2],
        extent: [u32; 2],
        raster_dimensions: [u32; 2],
        rgba16f: Vec<u16>,
        mask: Vec<u8>,
    ) -> Option<Self> {
        let [source_width, source_height] = source_dimensions;
        let [x, y] = origin;
        let [width, height] = extent;
        let [stored_width, stored_height] = raster_dimensions;
        let pixels = usize::try_from(stored_width)
            .ok()?
            .checked_mul(usize::try_from(stored_height).ok()?)?;
        let legacy_sized = stored_width == width && stored_height == height;
        let patch = Self {
            source_width,
            source_height,
            x,
            y,
            width,
            height,
            raster_width: if legacy_sized { 0 } else { stored_width },
            raster_height: if legacy_sized { 0 } else { stored_height },
            working_space_version: 1,
            rgba16f: rgba16f.into(),
            rgba: Vec::<u8>::new().into(),
            mask: mask.into(),
        };
        (patch.rgba16f.len() == pixels.checked_mul(4)? && patch.is_valid()).then_some(patch)
    }

    pub fn raster_dimensions(&self) -> [u32; 2] {
        if self.raster_width == 0 && self.raster_height == 0 {
            [self.width, self.height]
        } else {
            [self.raster_width, self.raster_height]
        }
    }

    fn has_valid_storage_layout(&self) -> bool {
        if self.source_width == 0
            || self.source_height == 0
            || self.width == 0
            || self.height == 0
            || self
                .x
                .checked_add(self.width)
                .is_none_or(|right| right > self.source_width)
            || self
                .y
                .checked_add(self.height)
                .is_none_or(|bottom| bottom > self.source_height)
            || (self.raster_width == 0) != (self.raster_height == 0)
        {
            return false;
        }
        let [raster_width, raster_height] = self.raster_dimensions();
        let Some(pixels) = usize::try_from(raster_width).ok().and_then(|width| {
            usize::try_from(raster_height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        }) else {
            return false;
        };
        if self.mask.len() != pixels || self.working_space_version > 1 {
            return false;
        }
        let Some(expected) = pixels.checked_mul(4) else {
            return false;
        };
        if self.rgba16f.is_empty() {
            return self.rgba.len() == expected;
        }
        self.rgba16f.len() == expected && (self.rgba.is_empty() || self.rgba.len() == expected)
    }

    pub fn is_valid(&self) -> bool {
        self.has_valid_storage_layout()
            && (self.rgba16f.is_empty()
                || self
                    .rgba16f
                    .iter()
                    .all(|value| half::f16::from_bits(*value).is_finite()))
    }

    pub fn needs_legacy_camera_to_working(&self) -> bool {
        self.working_space_version == 0 && !self.rgba16f.is_empty()
    }

    pub fn resolve_neutral_working_rgb(
        &self,
        rgb: [f32; 3],
        legacy_camera_to_working: [[f32; 4]; 3],
    ) -> [f32; 3] {
        if !self.needs_legacy_camera_to_working() {
            return rgb;
        }
        legacy_camera_to_working.map(|row| row[0] * rgb[0] + row[1] * rgb[1] + row[2] * rgb[2])
    }

    /// Returns one replacement pixel in scene-linear Rec.2020 RGBA16F.
    /// Legacy sRGB8 patches are converted on demand so old sidecars remain usable.
    pub fn linear_rgba16f_at(&self, index: usize) -> Option<[u16; 4]> {
        let base = index.checked_mul(4)?;
        if self.rgba16f.len() >= base + 4 {
            return Some([
                self.rgba16f[base],
                self.rgba16f[base + 1],
                self.rgba16f[base + 2],
                self.rgba16f[base + 3],
            ]);
        }
        if self.rgba.len() < base + 4 {
            return None;
        }
        use half::f16;
        let decode = |value: u8| {
            let encoded = f32::from(value) / 255.0;
            if encoded <= 0.04045 {
                encoded / 12.92
            } else {
                ((encoded + 0.055) / 1.055).powf(2.4)
            }
        };
        let r = decode(self.rgba[base]);
        let g = decode(self.rgba[base + 1]);
        let b = decode(self.rgba[base + 2]);
        let rec2020 = [
            0.627_403_9 * r + 0.329_283 * g + 0.043_313_1 * b,
            0.069_097_3 * r + 0.919_540_4 * g + 0.011_362_3 * b,
            0.016_391_4 * r + 0.088_013_3 * g + 0.895_595_3 * b,
        ];
        Some([
            f16::from_f32(rec2020[0]).to_bits(),
            f16::from_f32(rec2020[1]).to_bits(),
            f16::from_f32(rec2020[2]).to_bits(),
            f16::from_f32(1.0).to_bits(),
        ])
    }

    /// Bilinearly samples replacement RGB and its independently persisted
    /// coverage. New patches have a short full-opacity-to-zero edge ramp;
    /// legacy binary masks remain compatible and gain antialiasing when scaled.
    pub fn sample_linear_rec2020_bilinear(
        &self,
        source_x: f32,
        source_y: f32,
    ) -> Option<([f32; 3], f32)> {
        if !self.has_valid_storage_layout() || !source_x.is_finite() || !source_y.is_finite() {
            return None;
        }
        use half::f16;
        let patch_x0 = i64::from(self.x);
        let patch_y0 = i64::from(self.y);
        let patch_x1 = i64::from(self.x + self.width);
        let patch_y1 = i64::from(self.y + self.height);

        let nearest_x = (source_x + 0.5).floor() as i64;
        let nearest_y = (source_y + 0.5).floor() as i64;
        if nearest_x < patch_x0
            || nearest_y < patch_y0
            || nearest_x >= patch_x1
            || nearest_y >= patch_y1
        {
            return None;
        }
        let [raster_width, raster_height] = self.raster_dimensions();
        let raster_x =
            (((source_x - self.x as f32 + 0.5) * raster_width as f32 / self.width as f32) - 0.5)
                .clamp(0.0, (raster_width - 1) as f32);
        let raster_y =
            (((source_y - self.y as f32 + 0.5) * raster_height as f32 / self.height as f32) - 0.5)
                .clamp(0.0, (raster_height - 1) as f32);
        let x0 = raster_x.floor() as u32;
        let y0 = raster_y.floor() as u32;
        let x1 = (x0 + 1).min(raster_width - 1);
        let y1 = (y0 + 1).min(raster_height - 1);
        let tx = raster_x - x0 as f32;
        let ty = raster_y - y0 as f32;
        let samples = [
            (x0, y0, (1.0 - tx) * (1.0 - ty)),
            (x1, y0, tx * (1.0 - ty)),
            (x0, y1, (1.0 - tx) * ty),
            (x1, y1, tx * ty),
        ];
        let mut rgb = [0.0f32; 3];
        let mut alpha = 0.0f32;
        for (sample_x, sample_y, weight) in samples {
            let index = sample_y as usize * raster_width as usize + sample_x as usize;
            let pixel = self.linear_rgba16f_at(index)?;
            let linear = pixel.map(|value| f16::from_bits(value).to_f32());
            if !linear.iter().all(|value| value.is_finite()) {
                return None;
            }
            rgb[0] += linear[0] * weight;
            rgb[1] += linear[1] * weight;
            rgb[2] += linear[2] * weight;
            alpha += f32::from(self.mask[index]) * (weight / 255.0);
        }
        (alpha > 1e-6).then_some((rgb, alpha.clamp(0.0, 1.0)))
    }
}

#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct InpaintStroke {
    /// The tool that produced this replacement. The patch remains the
    /// authoritative rendered result; this metadata makes the edit legible and
    /// allows source-based strokes to be regenerated without touching the RAW.
    #[serde(default, skip_serializing_if = "InpaintStrokeKind::is_remove")]
    pub kind: InpaintStrokeKind,
    /// Source-to-destination translation in normalized full-image coordinates.
    /// Only Heal and Clone strokes carry an offset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_offset: Option<[f32; 2]>,
    #[serde(default)]
    pub dabs: Vec<BrushDab>,
    pub patch: InpaintPatch,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum InpaintStrokeKind {
    /// Content-aware removal generated by LaMa.
    #[default]
    Remove,
    /// Source texture adapted to the destination's low-frequency color.
    Heal,
    /// Exact source pixels copied into the destination.
    Clone,
}

impl InpaintStrokeKind {
    pub const ALL: [Self; 3] = [Self::Remove, Self::Heal, Self::Clone];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Remove => "Remove",
            Self::Heal => "Heal",
            Self::Clone => "Clone",
        }
    }

    pub const fn requires_source(self) -> bool {
        matches!(self, Self::Heal | Self::Clone)
    }

    fn is_remove(&self) -> bool {
        *self == Self::Remove
    }
}

impl InpaintStroke {
    pub fn from_result(dabs: Vec<BrushDab>, patch: InpaintPatch) -> Option<Self> {
        Self::from_tool_result(InpaintStrokeKind::Remove, None, dabs, patch)
    }

    pub fn from_tool_result(
        kind: InpaintStrokeKind,
        source_offset: Option<[f32; 2]>,
        mut dabs: Vec<BrushDab>,
        patch: InpaintPatch,
    ) -> Option<Self> {
        for dab in &mut dabs {
            dab.opacity = if dab.opacity > 0.0 { 1.0 } else { 0.0 };
            dab.feather = 0.0;
        }
        let source_offset = if kind.requires_source() {
            source_offset.filter(|offset| offset.iter().all(|value| value.is_finite()))
        } else {
            None
        };
        (patch.is_valid() && (!kind.requires_source() || source_offset.is_some())).then_some(Self {
            kind,
            source_offset,
            dabs,
            patch,
        })
    }
}

/// Builds a sparse, immutable Heal or Clone result from two neutral
/// scene-linear Rec.2020 working rasters. Both rasters may be small GPU
/// readbacks; their full-image placement keeps source sampling resolution
/// independent and lets the returned patch participate in the same ordered,
/// non-destructive layer as LaMa results.
#[allow(clippy::too_many_arguments)]
pub fn build_retouch_patch(
    kind: InpaintStrokeKind,
    full_dimensions: [u32; 2],
    destination_origin: [u32; 2],
    destination_extent: [u32; 2],
    destination_rgb: &[f32],
    source_origin: [u32; 2],
    source_extent: [u32; 2],
    source_rgb: &[f32],
    raster_dimensions: [u32; 2],
    source_offset: [f32; 2],
    dabs: &[BrushDab],
) -> Option<InpaintPatch> {
    if !kind.requires_source()
        || full_dimensions.contains(&0)
        || destination_extent.contains(&0)
        || source_extent.contains(&0)
        || raster_dimensions.contains(&0)
        || !source_offset.iter().all(|value| value.is_finite())
        || dabs.is_empty()
    {
        return None;
    }
    let [full_width, full_height] = full_dimensions;
    let [raster_width, raster_height] = raster_dimensions;
    let raster_pixels = usize::try_from(raster_width)
        .ok()?
        .checked_mul(usize::try_from(raster_height).ok()?)?;
    let expected_values = raster_pixels.checked_mul(3)?;
    if destination_rgb.len() != expected_values
        || source_rgb.len() != expected_values
        || destination_rgb.iter().any(|value| !value.is_finite())
        || source_rgb.iter().any(|value| !value.is_finite())
        || destination_origin[0].checked_add(destination_extent[0])? > full_width
        || destination_origin[1].checked_add(destination_extent[1])? > full_height
        || source_origin[0].checked_add(source_extent[0])? > full_width
        || source_origin[1].checked_add(source_extent[1])? > full_height
    {
        return None;
    }

    let mut sampled_source = vec![0.0f32; expected_values];
    let offset_pixels = [
        source_offset[0] * full_width as f32,
        source_offset[1] * full_height as f32,
    ];
    for y in 0..raster_height {
        let destination_y = destination_origin[1] as f32
            + (y as f32 + 0.5) * destination_extent[1] as f32 / raster_height as f32
            - 0.5;
        for x in 0..raster_width {
            let destination_x = destination_origin[0] as f32
                + (x as f32 + 0.5) * destination_extent[0] as f32 / raster_width as f32
                - 0.5;
            let source_x = ((destination_x + offset_pixels[0] - source_origin[0] as f32 + 0.5)
                * raster_width as f32
                / source_extent[0] as f32)
                - 0.5;
            let source_y = ((destination_y + offset_pixels[1] - source_origin[1] as f32 + 0.5)
                * raster_height as f32
                / source_extent[1] as f32)
                - 0.5;
            let destination = (y as usize * raster_width as usize + x as usize) * 3;
            let sample = sample_retouch_rgb_bilinear(
                source_rgb,
                raster_width,
                raster_height,
                source_x,
                source_y,
            )
            .unwrap_or([
                destination_rgb[destination],
                destination_rgb[destination + 1],
                destination_rgb[destination + 2],
            ]);
            sampled_source[destination..destination + 3].copy_from_slice(&sample);
        }
    }

    let full_min = full_width.min(full_height).max(1) as f32;
    let destination_min = destination_extent[0].min(destination_extent[1]).max(1) as f32;
    let local_dabs = dabs
        .iter()
        .filter_map(|dab| {
            let center = [
                (dab.center[0] * full_width as f32 - destination_origin[0] as f32)
                    / destination_extent[0] as f32,
                (dab.center[1] * full_height as f32 - destination_origin[1] as f32)
                    / destination_extent[1] as f32,
            ];
            let size = dab.size.clamp(f32::EPSILON, 0.5) * full_min / destination_min;
            (center.iter().all(|value| value.is_finite()) && size.is_finite()).then_some(BrushDab {
                center,
                opacity: 1.0,
                size,
                feather: 0.0,
            })
        })
        .collect::<Vec<_>>();
    if local_dabs.is_empty() {
        return None;
    }

    let heal_delta = if kind == InpaintStrokeKind::Heal {
        // A healing brush transfers high-frequency texture while retaining the
        // destination's illumination and color field. Subtracting the source's
        // local low frequencies and adding the destination's is stable in
        // scene-linear space and continues to respond naturally to later RAW
        // exposure/white-balance edits.
        let image_min = full_width.min(full_height).max(1) as f32;
        let average_radius = dabs
            .iter()
            .map(|dab| dab.size.max(0.0) * image_min)
            .sum::<f32>()
            / dabs.len().max(1) as f32;
        let raster_scale = (raster_width as f32 / destination_extent[0] as f32)
            .min(raster_height as f32 / destination_extent[1] as f32);
        let blur_radius = (average_radius * raster_scale * 0.55)
            .round()
            .clamp(3.0, 24.0) as usize;
        let source_low = box_blur_rgb(&sampled_source, raster_width, raster_height, blur_radius);
        let destination_low =
            box_blur_rgb(destination_rgb, raster_width, raster_height, blur_radius);
        source_low
            .iter()
            .zip(destination_low.iter())
            .map(|(source_base, destination_base)| destination_base - source_base)
            .collect::<Vec<_>>()
    } else {
        vec![0.0; expected_values]
    };
    let static_generated = sampled_source
        .iter()
        .zip(&heal_delta)
        .map(|(sample, delta)| (sample + delta).clamp(-65_504.0, 65_504.0))
        .collect::<Vec<_>>();
    let generated = if retouch_source_may_cross_active_stroke(dabs, source_offset, full_dimensions)
    {
        apply_retouch_dabs_causally(
            destination_rgb,
            &sampled_source,
            &heal_delta,
            RetouchRasterGeometry {
                raster_dimensions,
                destination_origin,
                destination_extent,
                full_dimensions,
                source_offset,
            },
            &local_dabs,
        )?
    } else {
        static_generated
    };
    let painted_mask = rasterize_inpaint_dabs_binary(
        raster_width,
        raster_height,
        raster_width,
        raster_height,
        &local_dabs,
    );
    let feather_pixels = 3.0f32;
    let composite_dabs = local_dabs
        .iter()
        .map(|dab| {
            let inner_radius = dab.size * raster_width.min(raster_height) as f32;
            let outer_radius = inner_radius + feather_pixels;
            BrushDab {
                center: dab.center,
                opacity: 1.0,
                size: outer_radius / raster_width.min(raster_height).max(1) as f32,
                feather: (feather_pixels / outer_radius.max(feather_pixels)).clamp(0.0, 1.0),
            }
        })
        .collect::<Vec<_>>();
    let mut composite_mask = rasterize_brush_dabs(
        raster_width,
        raster_height,
        raster_width,
        raster_height,
        &composite_dabs,
    );
    for (composite, painted) in composite_mask.iter_mut().zip(&painted_mask) {
        if *painted > 0 {
            *composite = 255;
        }
    }
    let bounds = retouch_storage_bounds(&composite_mask, raster_width, raster_height)?;

    let full_x0 = destination_origin[0]
        + ((u64::from(bounds[0]) * u64::from(destination_extent[0])) / u64::from(raster_width))
            as u32;
    let full_y0 = destination_origin[1]
        + ((u64::from(bounds[1]) * u64::from(destination_extent[1])) / u64::from(raster_height))
            as u32;
    let full_x1 = destination_origin[0]
        + ((u64::from(bounds[0] + bounds[2]) * u64::from(destination_extent[0]))
            .div_ceil(u64::from(raster_width))) as u32;
    let full_y1 = destination_origin[1]
        + ((u64::from(bounds[1] + bounds[3]) * u64::from(destination_extent[1]))
            .div_ceil(u64::from(raster_height))) as u32;
    let extent = [
        full_x1.saturating_sub(full_x0),
        full_y1.saturating_sub(full_y0),
    ];
    if extent.contains(&0) {
        return None;
    }
    let stored_width = ((u64::from(extent[0]) * u64::from(raster_width))
        .div_ceil(u64::from(destination_extent[0]))) as u32;
    let stored_height = ((u64::from(extent[1]) * u64::from(raster_height))
        .div_ceil(u64::from(destination_extent[1]))) as u32;
    let stored_pixels = usize::try_from(stored_width)
        .ok()?
        .checked_mul(usize::try_from(stored_height).ok()?)?;
    let mut rgba16f = vec![0u16; stored_pixels.checked_mul(4)?];
    let mut mask = vec![0u8; stored_pixels];
    for y in 0..stored_height {
        let global_y =
            full_y0 as f32 + (y as f32 + 0.5) * extent[1] as f32 / stored_height as f32 - 0.5;
        let raster_y = ((global_y - destination_origin[1] as f32 + 0.5) * raster_height as f32
            / destination_extent[1] as f32)
            - 0.5;
        for x in 0..stored_width {
            let global_x =
                full_x0 as f32 + (x as f32 + 0.5) * extent[0] as f32 / stored_width as f32 - 0.5;
            let raster_x = ((global_x - destination_origin[0] as f32 + 0.5) * raster_width as f32
                / destination_extent[0] as f32)
                - 0.5;
            let rgb = sample_retouch_rgb_bilinear(
                &generated,
                raster_width,
                raster_height,
                raster_x,
                raster_y,
            )?;
            let alpha = sample_retouch_mask_bilinear(
                &composite_mask,
                raster_width,
                raster_height,
                raster_x,
                raster_y,
            );
            let index = y as usize * stored_width as usize + x as usize;
            let output = index * 4;
            rgba16f[output] = f16::from_f32(rgb[0].clamp(-65_504.0, 65_504.0)).to_bits();
            rgba16f[output + 1] = f16::from_f32(rgb[1].clamp(-65_504.0, 65_504.0)).to_bits();
            rgba16f[output + 2] = f16::from_f32(rgb[2].clamp(-65_504.0, 65_504.0)).to_bits();
            rgba16f[output + 3] = f16::from_f32(1.0).to_bits();
            mask[index] = alpha;
        }
    }
    InpaintPatch::new_linear_resampled(
        full_dimensions,
        [full_x0, full_y0],
        extent,
        [stored_width, stored_height],
        rgba16f,
        mask,
    )
}

fn retouch_source_may_cross_active_stroke(
    dabs: &[BrushDab],
    source_offset: [f32; 2],
    full_dimensions: [u32; 2],
) -> bool {
    let image_min = full_dimensions[0].min(full_dimensions[1]).max(1) as f32;
    let mut bounds = [
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    ];
    for dab in dabs {
        let radius = dab.size.max(0.0) * image_min;
        let center = [
            dab.center[0] * full_dimensions[0] as f32,
            dab.center[1] * full_dimensions[1] as f32,
        ];
        bounds[0] = bounds[0].min(center[0] - radius);
        bounds[1] = bounds[1].min(center[1] - radius);
        bounds[2] = bounds[2].max(center[0] + radius);
        bounds[3] = bounds[3].max(center[1] + radius);
    }
    if !bounds.iter().all(|value| value.is_finite()) {
        return false;
    }
    let offset = [
        source_offset[0] * full_dimensions[0] as f32,
        source_offset[1] * full_dimensions[1] as f32,
    ];
    let source_bounds = [
        bounds[0] + offset[0],
        bounds[1] + offset[1],
        bounds[2] + offset[0],
        bounds[3] + offset[1],
    ];
    source_bounds[0] < bounds[2]
        && source_bounds[2] > bounds[0]
        && source_bounds[1] < bounds[3]
        && source_bounds[3] > bounds[1]
}

#[derive(Clone, Copy)]
struct RetouchRasterGeometry {
    raster_dimensions: [u32; 2],
    destination_origin: [u32; 2],
    destination_extent: [u32; 2],
    full_dimensions: [u32; 2],
    source_offset: [f32; 2],
}

fn apply_retouch_dabs_causally(
    destination_rgb: &[f32],
    sampled_source: &[f32],
    heal_delta: &[f32],
    geometry: RetouchRasterGeometry,
    local_dabs: &[BrushDab],
) -> Option<Vec<f32>> {
    let RetouchRasterGeometry {
        raster_dimensions,
        destination_origin,
        destination_extent,
        full_dimensions,
        source_offset,
    } = geometry;

    let [width, height] = raster_dimensions;
    let expected = usize::try_from(width)
        .ok()?
        .checked_mul(usize::try_from(height).ok()?)?
        .checked_mul(3)?;
    if width == 0
        || height == 0
        || destination_rgb.len() != expected
        || sampled_source.len() != expected
        || heal_delta.len() != expected
    {
        return None;
    }
    let mut canvas = destination_rgb.to_vec();
    let mut painted = vec![0.0f32; width as usize * height as usize];
    let mut generated = sampled_source
        .iter()
        .zip(heal_delta)
        .map(|(sample, delta)| (sample + delta).clamp(-65_504.0, 65_504.0))
        .collect::<Vec<_>>();
    let offset_pixels = [
        source_offset[0] * full_dimensions[0] as f32,
        source_offset[1] * full_dimensions[1] as f32,
    ];

    // A dab reads from one stable version of the canvas, then publishes all of
    // its pixels together. The next dab can therefore sample those pixels when
    // an aligned Heal/Clone source crosses the stroke currently being painted.
    for dab in local_dabs {
        let spec = brush_raster_specs(
            MaskRasterSpace::new(width, height, width, height),
            std::slice::from_ref(dab),
        )
        .into_iter()
        .next()?;
        let mut updates = Vec::new();
        for y in spec.min_y..=spec.max_y {
            let dy = (y as f32 + 0.5 - spec.center_y) / spec.radius_y.max(0.5);
            for x in spec.min_x..=spec.max_x {
                let dx = (x as f32 + 0.5 - spec.center_x) / spec.radius_x.max(0.5);
                let distance = (dx * dx + dy * dy).sqrt();
                if distance >= 1.0 + spec.antialias {
                    continue;
                }
                let coverage = 1.0 - smoothstep(spec.inner, 1.0 + spec.antialias, distance);
                if coverage <= 0.0 {
                    continue;
                }
                let destination_x = destination_origin[0] as f32
                    + (x as f32 + 0.5) * destination_extent[0] as f32 / width as f32
                    - 0.5;
                let destination_y = destination_origin[1] as f32
                    + (y as f32 + 0.5) * destination_extent[1] as f32 / height as f32
                    - 0.5;
                let source_raster_x =
                    ((destination_x + offset_pixels[0] - destination_origin[0] as f32 + 0.5)
                        * width as f32
                        / destination_extent[0] as f32)
                        - 0.5;
                let source_raster_y =
                    ((destination_y + offset_pixels[1] - destination_origin[1] as f32 + 0.5)
                        * height as f32
                        / destination_extent[1] as f32)
                        - 0.5;
                let pixel = (y as usize * width as usize + x as usize) * 3;
                let static_sample = [
                    sampled_source[pixel],
                    sampled_source[pixel + 1],
                    sampled_source[pixel + 2],
                ];
                let dynamic_sample = sample_retouch_rgb_bilinear(
                    &canvas,
                    width,
                    height,
                    source_raster_x,
                    source_raster_y,
                );
                let dynamic_alpha = sample_retouch_alpha_bilinear(
                    &painted,
                    width,
                    height,
                    source_raster_x,
                    source_raster_y,
                )
                .unwrap_or(0.0);
                let sampled = dynamic_sample.map_or(static_sample, |dynamic| {
                    std::array::from_fn(|channel| {
                        static_sample[channel]
                            + (dynamic[channel] - static_sample[channel]) * dynamic_alpha
                    })
                });
                let target: [f32; 3] = std::array::from_fn(|channel| {
                    (sampled[channel] + heal_delta[pixel + channel]).clamp(-65_504.0, 65_504.0)
                });
                updates.push((pixel, target, coverage));
            }
        }
        for (pixel, target, coverage) in updates {
            for (channel, value) in target.into_iter().enumerate() {
                canvas[pixel + channel] += (value - canvas[pixel + channel]) * coverage;
                generated[pixel + channel] = value;
            }
            let mask_pixel = pixel / 3;
            painted[mask_pixel] += (1.0 - painted[mask_pixel]) * coverage;
        }
    }
    Some(generated)
}

fn sample_retouch_alpha_bilinear(
    alpha: &[f32],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> Option<f32> {
    if width == 0
        || height == 0
        || alpha.len() != width as usize * height as usize
        || !x.is_finite()
        || !y.is_finite()
        || x < -0.5
        || y < -0.5
        || x > width as f32 - 0.5
        || y > height as f32 - 0.5
    {
        return None;
    }
    let x = x.clamp(0.0, width.saturating_sub(1) as f32);
    let y = y.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let sample = |sample_x: u32, sample_y: u32| alpha[(sample_y * width + sample_x) as usize];
    let top = sample(x0, y0) + (sample(x1, y0) - sample(x0, y0)) * tx;
    let bottom = sample(x0, y1) + (sample(x1, y1) - sample(x0, y1)) * tx;
    Some(top + (bottom - top) * ty)
}

fn sample_retouch_rgb_bilinear(
    rgb: &[f32],
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> Option<[f32; 3]> {
    if width == 0
        || height == 0
        || rgb.len() != width as usize * height as usize * 3
        || !x.is_finite()
        || !y.is_finite()
        || x < -0.5
        || y < -0.5
        || x > width as f32 - 0.5
        || y > height as f32 - 0.5
    {
        return None;
    }
    let x = x.clamp(0.0, width.saturating_sub(1) as f32);
    let y = y.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let samples = [
        (x0, y0, (1.0 - tx) * (1.0 - ty)),
        (x1, y0, tx * (1.0 - ty)),
        (x0, y1, (1.0 - tx) * ty),
        (x1, y1, tx * ty),
    ];
    let mut result = [0.0f32; 3];
    for (sample_x, sample_y, weight) in samples {
        let index = (sample_y as usize * width as usize + sample_x as usize) * 3;
        for channel in 0..3 {
            result[channel] += rgb[index + channel] * weight;
        }
    }
    Some(result)
}

fn sample_retouch_mask_bilinear(mask: &[u8], width: u32, height: u32, x: f32, y: f32) -> u8 {
    if width == 0 || height == 0 || mask.len() != width as usize * height as usize {
        return 0;
    }
    let x = x.clamp(0.0, width.saturating_sub(1) as f32);
    let y = y.clamp(0.0, height.saturating_sub(1) as f32);
    let x0 = x.floor() as u32;
    let y0 = y.floor() as u32;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let tx = x - x0 as f32;
    let ty = y - y0 as f32;
    let sample = |sample_x: u32, sample_y: u32| {
        f32::from(mask[(sample_y as usize * width as usize) + sample_x as usize])
    };
    let top = sample(x0, y0) + (sample(x1, y0) - sample(x0, y0)) * tx;
    let bottom = sample(x0, y1) + (sample(x1, y1) - sample(x0, y1)) * tx;
    (top + (bottom - top) * ty).round().clamp(0.0, 255.0) as u8
}

fn box_blur_rgb(rgb: &[f32], width: u32, height: u32, radius: usize) -> Vec<f32> {
    if radius == 0 || width == 0 || height == 0 {
        return rgb.to_vec();
    }
    let width = width as usize;
    let height = height as usize;
    let mut horizontal = vec![0.0f32; rgb.len()];
    horizontal
        .par_chunks_mut(width * 3)
        .enumerate()
        .for_each(|(y, row)| {
            for channel in 0..3 {
                let mut start = 0usize;
                let mut end = (radius + 1).min(width);
                let mut sum = (start..end)
                    .map(|x| rgb[(y * width + x) * 3 + channel])
                    .sum::<f32>();
                for x in 0..width {
                    row[x * 3 + channel] = sum / (end - start).max(1) as f32;
                    let next_start = (x + 1).saturating_sub(radius);
                    let next_end = (x + radius + 2).min(width);
                    for removed in start..next_start {
                        sum -= rgb[(y * width + removed) * 3 + channel];
                    }
                    for added in end..next_end {
                        sum += rgb[(y * width + added) * 3 + channel];
                    }
                    start = next_start;
                    end = next_end;
                }
            }
        });
    let mut output = vec![0.0f32; rgb.len()];
    for x in 0..width {
        for channel in 0..3 {
            let mut start = 0usize;
            let mut end = (radius + 1).min(height);
            let mut sum = (start..end)
                .map(|y| horizontal[(y * width + x) * 3 + channel])
                .sum::<f32>();
            for y in 0..height {
                output[(y * width + x) * 3 + channel] = sum / (end - start).max(1) as f32;
                let next_start = (y + 1).saturating_sub(radius);
                let next_end = (y + radius + 2).min(height);
                for removed in start..next_start {
                    sum -= horizontal[(removed * width + x) * 3 + channel];
                }
                for added in end..next_end {
                    sum += horizontal[(added * width + x) * 3 + channel];
                }
                start = next_start;
                end = next_end;
            }
        }
    }
    output
}

fn retouch_storage_bounds(mask: &[u8], width: u32, height: u32) -> Option<[u32; 4]> {
    if width == 0 || height == 0 || mask.len() != width as usize * height as usize {
        return None;
    }
    let mut min_x = width;
    let mut min_y = height;
    let mut max_x = 0;
    let mut max_y = 0;
    let mut found = false;
    for y in 0..height {
        for x in 0..width {
            if mask[(y as usize * width as usize) + x as usize] >= 4 {
                found = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }
    found.then_some([
        min_x.saturating_sub(1),
        min_y.saturating_sub(1),
        max_x.saturating_add(2).min(width) - min_x.saturating_sub(1),
        max_y.saturating_add(2).min(height) - min_y.saturating_sub(1),
    ])
}

/// Rebuilds the live display/export layer without expanding patches into a
/// preview-sized or full-resolution framebuffer. Keeping the patch list sparse
/// avoids proxy upscaling artifacts and prevents distant strokes from forcing a
/// huge dense allocation.
pub fn compose_inpaint_strokes(strokes: &[InpaintStroke]) -> Option<InpaintLayer> {
    InpaintLayer::new(strokes.iter().map(|stroke| stroke.patch.clone()).collect())
}

const COMPRESSED_BINARY_PREFIX: &str = "z1:";
const MAX_INPAINT_BINARY_FIELD_BYTES: u64 = if cfg!(target_os = "android") {
    128 * 1024 * 1024
} else {
    512 * 1024 * 1024
};

fn encode_binary_field(bytes: &[u8]) -> Result<String, std::io::Error> {
    use base64::Engine as _;
    use flate2::{write::ZlibEncoder, Compression};
    use std::io::Write as _;

    let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    let compressed = encoder.finish()?;
    let engine = &base64::engine::general_purpose::STANDARD;
    if compressed
        .len()
        .saturating_add(COMPRESSED_BINARY_PREFIX.len())
        < bytes.len()
    {
        Ok(format!(
            "{COMPRESSED_BINARY_PREFIX}{}",
            engine.encode(compressed)
        ))
    } else {
        Ok(engine.encode(bytes))
    }
}

fn decode_binary_field(encoded: &str) -> Result<Vec<u8>, String> {
    use base64::Engine as _;
    use flate2::read::ZlibDecoder;
    use std::io::Read as _;

    let engine = &base64::engine::general_purpose::STANDARD;
    let Some(compressed) = encoded.strip_prefix(COMPRESSED_BINARY_PREFIX) else {
        return engine.decode(encoded).map_err(|error| error.to_string());
    };
    let compressed = engine
        .decode(compressed)
        .map_err(|error| error.to_string())?;
    let decoder = ZlibDecoder::new(compressed.as_slice());
    let mut limited = decoder.take(MAX_INPAINT_BINARY_FIELD_BYTES + 1);
    let mut bytes = Vec::new();
    limited
        .read_to_end(&mut bytes)
        .map_err(|error| error.to_string())?;
    if bytes.len() as u64 > MAX_INPAINT_BINARY_FIELD_BYTES {
        return Err("compressed inpainting payload exceeds the decoded safety limit".to_owned());
    }
    Ok(bytes)
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

mod compressed_base64_arc_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S>(bytes: &Arc<[u8]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(
            &super::encode_binary_field(bytes.as_ref()).map_err(serde::ser::Error::custom)?,
        )
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u8]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        super::decode_binary_field(&encoded)
            .map(Arc::from)
            .map_err(serde::de::Error::custom)
    }
}

mod base64_arc_u16 {
    use serde::{Deserialize, Deserializer, Serializer};
    use std::sync::Arc;

    pub fn serialize<S>(values: &Arc<[u16]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut bytes = Vec::with_capacity(values.len() * 2);
        for value in values.iter().copied() {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        serializer
            .serialize_str(&super::encode_binary_field(&bytes).map_err(serde::ser::Error::custom)?)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u16]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = super::decode_binary_field(&encoded).map_err(serde::de::Error::custom)?;
        if bytes.len() % 2 != 0 {
            return Err(serde::de::Error::custom(
                "RGBA16F payload has an odd byte length",
            ));
        }
        let values = bytes
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        Ok(values.into())
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
    /// Constant coverage across the complete image. This has no editable
    /// geometry and is immediately initialized when created.
    Fullscreen,
    Brush {
        /// Radius as a fraction of the image's shorter edge.
        size: f32,
        feather: f32,
        /// Whether newly drawn brush and eraser strokes use `opacity`.
        /// Legacy sidecars default to full-strength strokes.
        #[serde(default)]
        opacity_enabled: bool,
        #[serde(default = "default_brush_opacity")]
        opacity: f32,
        /// Whether separate recorded strokes build coverage where they overlap.
        #[serde(default = "default_brush_overlap_enabled")]
        overlap_enabled: bool,
        /// Dab indexes that begin strokes recorded by overlap-aware versions.
        /// Dabs before the first index are a legacy prefix and keep their exact
        /// historical compositing behavior.
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
    Object {
        mask: Option<MaskImage>,
        #[serde(default)]
        grow: f32,
        feather: f32,
        /// Radius as a fraction of the image's shorter edge. This controls the
        /// on-canvas object prompt brush and is captured before SAM inference.
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
                mask: None,
                grow: 0.0,
                feather: 0.0,
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
                mask.is_some()
            }
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
    /// Uniform hue rotation in degrees.
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
        Self {
            exposure: 0.0,
            contrast: 0.0,
            highlights: 0.0,
            shadows: 0.0,
            whites: 0.0,
            blacks: 0.0,
            temperature: 0.0,
            tint: 0.0,
            hue: 0.0,
            saturation: 0.0,
            texture: 0.0,
            clarity: 0.0,
            dehaze: 0.0,
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
        // Hue is intentionally remembered when a wheel is pulled back to the
        // center. With zero saturation/luminance it must still count as a
        // neutral local adjustment and take the exact bypass path.
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
    /// What the combined mask coverage does. Missing in older sidecars, which
    /// must retain the original adjustment-mask behavior.
    #[serde(default)]
    pub effect: MaskEffect,
    /// Editable, non-destructive parameters for implemented effect types.
    /// Keeping these separate means switching mask types never discards the
    /// settings belonging to another type.
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
    /// One shared correction layer for every Subject / Not Subject component.
    /// Sidecars persist this at `EditState.subject_refinement`; the runtime
    /// stack keeps a synchronized copy so all rasterization paths see it.
    #[serde(skip, default)]
    pub subject_refinement: SubjectRefinement,
}

impl MaskStack {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Returns a mask stack remapped to a cropped image region. The region is
    /// expressed in full-image pixels, so geometric masks and cached AI/range
    /// sources continue to line up with a zoomed detail preview.
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

    /// Source-pixel context needed around a partial mask raster so grow and
    /// feather produce the same boundary as a full-frame raster. Procedural
    /// brushes and gradients do not need a halo because their geometry remains
    /// available after cropping; distance-shaped masks do.
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
            // A complementary duplicate keeps coverage but intentionally starts
            // from neutral local adjustments, matching the existing UI action.
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
        if mask.components.len() >= MAX_MASK_COMPONENTS || component_index >= mask.components.len() {
            return false;
        }
        component.name = copied_name(&component.name, |candidate| {
            mask.components.iter().any(|component| component.name == candidate)
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
                    // The common one-component Brush case can take ownership
                    // directly instead of allocating and copying a second
                    // full-size f32 atlas.
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

    /// Full-precision GPU mask coverage. R16F avoids the 1/255 opacity steps
    /// becoming visible at feathered boundaries under strong local exposure.
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
            // Feather the subject boundary once, then complement it for Not
            // Subject. This keeps the two AI masks exact opposites at every
            // feather value instead of separately blurring an inverted map.
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
        MaskGeometry::Ai { grow, feather, .. }
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

    // Cropped viewport atlases scale these normalized controls so the source-
    // pixel grow/feather radius remains identical to a full-frame raster.
    // Persisted/UI values stay in [-1,1]/[0,1]; the wider internal range is
    // only needed by those cropped clones.
    let grow = grow.clamp(-32.0, 32.0);
    let feather = feather.clamp(0.0, 32.0);
    if grow.abs() <= 1e-5 && feather <= 1e-5 {
        // ViTMatte deliberately returns fractional alpha for hair, fur,
        // translucent fabric, and sub-pixel contours. Zero feather means "use
        // the generated matte as-is", not "threshold it back to a coarse
        // binary segmentation".
        mask.par_iter_mut()
            .for_each(|value| *value = value.clamp(0.0, 1.0));
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
    // Feather is an image-relative boundary width, so thumbnails, the preview
    // overlay, the mask atlas, and export all show the same shape. The ramp is
    // centered on the original 0.5 contour: increasing feather changes only
    // the edge transition and cannot grow or contract the selected contour.
    let feather_radius = (feather.powf(1.30) * edge * 0.045).max(0.75);

    mask.par_iter_mut().enumerate().for_each(|(index, value)| {
        // Preserve sub-pixel model confidence without letting it overpower the
        // user-visible grow radius. This offset is zero at alpha=0.5 and is
        // smaller than the one-pixel sign separating inside from outside, so
        // feathering retains the exact same selected contour.
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

/// Rasterizes the inpainting brush as a strict binary mask.
///
/// Unlike the display-compositing coverage, the model mask has no feather ramp
/// or sub-pixel coverage. `BrushDab::feather` is intentionally ignored because
/// LaMa requires a binary input mask.
pub fn rasterize_inpaint_dabs_binary(
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
    dabs: &[BrushDab],
) -> Vec<u8> {
    if width == 0 || height == 0 || dabs.is_empty() {
        return vec![0; width as usize * height as usize];
    }
    let space = MaskRasterSpace::new(width, height, image_width, image_height);
    let [width, height] = space.raster;
    let [image_width, image_height] = space.image;

    #[derive(Clone, Copy)]
    struct BinaryDab {
        center_x: f32,
        center_y: f32,
        radius_x: f32,
        radius_y: f32,
        min_x: i32,
        max_x: i32,
        min_y: i32,
        max_y: i32,
    }

    let image_min = image_width.min(image_height).max(1) as f32;
    let specs = dabs
        .iter()
        .filter(|dab| dab.opacity > 0.0)
        .map(|dab| {
            let radius_image = dab.size.clamp(f32::EPSILON, 0.5) * image_min;
            let radius_x = radius_image * width as f32 / image_width.max(1) as f32;
            let radius_y = radius_image * height as f32 / image_height.max(1) as f32;
            let center_x = dab.center[0] * width as f32;
            let center_y = dab.center[1] * height as f32;
            let bbox_x = radius_x.ceil().max(1.0) as i32;
            let bbox_y = radius_y.ceil().max(1.0) as i32;
            BinaryDab {
                center_x,
                center_y,
                radius_x,
                radius_y,
                min_x: (center_x.floor() as i32 - bbox_x).max(0),
                max_x: (center_x.ceil() as i32 + bbox_x).min(width as i32 - 1),
                min_y: (center_y.floor() as i32 - bbox_y).max(0),
                max_y: (center_y.ceil() as i32 + bbox_y).min(height as i32 - 1),
            }
        })
        .collect::<Vec<_>>();

    if specs.is_empty() {
        return vec![0; width as usize * height as usize];
    }

    const ROW_BAND_HEIGHT: usize = 64;
    let row_stride = width as usize;
    let mut out = vec![0u8; row_stride * height as usize];
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
                        if dx * dx + dy * dy <= 1.0 {
                            band[row_offset + x as usize] = 255;
                        }
                    }
                }
            }
        });
    out
}

fn rasterize_brush(space: MaskRasterSpace, dabs: &[BrushDab]) -> Vec<f32> {
    let [width, height] = space.raster;
    if width == 0 || height == 0 || dabs.is_empty() {
        return vec![0.0; width as usize * height as usize];
    }

    let specs = brush_raster_specs(space, dabs);

    // Each row band is independent, so the expensive full-resolution atlas can
    // use all CPU cores while preserving the exact original dab order inside
    // every pixel. Paint/erase semantics therefore remain bit-for-bit the same
    // as the serial implementation.
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
        // UV coordinates describe continuous image space; texel samples live
        // at x + 0.5/y + 0.5. Keeping the center in continuous texel space
        // makes even-sized atlases symmetric around a centered brush dab.
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
        // Sidecars created before stroke boundaries were recorded retain the
        // exact historical per-dab compositing behavior.
        return rasterize_brush(space, dabs);
    }

    let mut out = rasterize_brush(space, &dabs[..legacy_end]);
    let specs = brush_raster_specs(space, &dabs[legacy_end..]);

    // Collapse overlapping dabs within one pointer stroke to a single coverage
    // field. Separate strokes can then alpha-build without a slow continuous
    // stroke becoming opaque merely because its regularly spaced dabs overlap.
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

            // A refinement sidecar should always have stroke starts, but this
            // prefix keeps manually constructed/forward-compatible data useful.
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
