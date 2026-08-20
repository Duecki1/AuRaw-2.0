use super::{ExposureParams, LoadedRaw};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::sync::Arc;

pub const BIG_LAMA_INPUT_EDGE: u32 = 512;
pub const REMOVE_MAX_STROKES: usize = 512;
pub const REMOVE_MAX_POINTS_PER_STROKE: usize = 65_536;
pub const REMOVE_MAX_PATCHES_PER_STROKE: usize = 256;

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveBrushPoint {
    /// Native source-image X coordinate in pixels.
    pub x: f32,
    /// Native source-image Y coordinate in pixels.
    pub y: f32,
    /// Native source-image brush radius in pixels.
    pub radius: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveBrushStroke {
    pub points: Vec<RemoveBrushPoint>,
    #[serde(default)]
    pub dilation_radius: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeRect {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

impl NativeRect {
    pub fn right(self) -> u32 {
        self.x.saturating_add(self.width)
    }

    pub fn bottom(self) -> u32 {
        self.y.saturating_add(self.height)
    }

    pub fn intersect(self, other: Self) -> Option<Self> {
        let x0 = self.x.max(other.x);
        let y0 = self.y.max(other.y);
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        (x1 > x0 && y1 > y0).then_some(Self {
            x: x0,
            y: y0,
            width: x1 - x0,
            height: y1 - y0,
        })
    }
}

/// One local Big-LaMa job: `context` is the square RGB crop presented to the
/// model, while `target` is the overlapping native core whose mask may be
/// changed by this job. Keeping those geometries separate preserves real image
/// context even when one Remove stroke spans many model calls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RemoveContextCrop {
    pub context: NativeRect,
    pub target: NativeRect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RemovePatch {
    pub bounds: NativeRect,
    /// Canonical scene-boundary RGB stored as IEEE-754 binary16 bits.
    ///
    /// For sensor RAW files this is camera RGB with the white-balance gains
    /// divided back out. At render time the current white balance is reapplied
    /// before the patch is uploaded into the GPU scene texture. For rendered
    /// raster sources this is scene-linear Rec.2020, which is already the
    /// pipeline's source-scene boundary. Storing Remove here keeps every
    /// downstream Develop adjustment live without rerunning Big-LaMa.
    #[serde(default, with = "arc_u16_le_base64")]
    pub rgb_scene16f: Arc<[u16]>,
    /// Legacy post-adjustment sRGB cache used by sidecars written by the first
    /// Remove implementation. New strokes never populate this field.
    #[serde(
        default,
        with = "arc_u16_le_base64",
        skip_serializing_if = "arc_u16_is_empty"
    )]
    pub rgb_srgb16: Arc<[u16]>,
    /// Feathered compositing coverage, one byte per patch pixel. The feather is
    /// already composited into `rgb_scene16f`; alpha remains so scaled preview
    /// uploads can avoid touching pixels outside the affected mask.
    #[serde(with = "arc_u8_base64")]
    pub alpha: Arc<[u8]>,
}

fn arc_u16_is_empty(values: &Arc<[u16]>) -> bool {
    values.is_empty()
}

impl RemovePatch {
    pub fn new_scene(
        bounds: NativeRect,
        rgb_scene16f: Vec<u16>,
        alpha: Vec<u8>,
    ) -> Result<Self, &'static str> {
        let pixels = (bounds.width as usize)
            .checked_mul(bounds.height as usize)
            .ok_or("remove patch pixel count overflows")?;
        if pixels == 0 {
            return Err("remove patch is empty");
        }
        if rgb_scene16f.len() != pixels.saturating_mul(3) {
            return Err("remove scene RGB length does not match bounds");
        }
        if alpha.len() != pixels {
            return Err("remove patch alpha length does not match bounds");
        }
        Ok(Self {
            bounds,
            rgb_scene16f: Arc::from(rgb_scene16f),
            rgb_srgb16: Arc::from([]),
            alpha: Arc::from(alpha),
        })
    }

    /// Compatibility constructor for schema-12 Remove patches.
    pub fn new(
        bounds: NativeRect,
        rgb_srgb16: Vec<u16>,
        alpha: Vec<u8>,
    ) -> Result<Self, &'static str> {
        let pixels = (bounds.width as usize)
            .checked_mul(bounds.height as usize)
            .ok_or("remove patch pixel count overflows")?;
        if pixels == 0 {
            return Err("remove patch is empty");
        }
        if rgb_srgb16.len() != pixels.saturating_mul(3) {
            return Err("remove patch RGB length does not match bounds");
        }
        if alpha.len() != pixels {
            return Err("remove patch alpha length does not match bounds");
        }
        Ok(Self {
            bounds,
            rgb_scene16f: Arc::from([]),
            rgb_srgb16: Arc::from(rgb_srgb16),
            alpha: Arc::from(alpha),
        })
    }

    pub fn has_scene_pixels(&self) -> bool {
        !self.rgb_scene16f.is_empty()
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveStroke {
    pub brush: RemoveBrushStroke,
    #[serde(default)]
    pub patches: Vec<RemovePatch>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RemoveEditState {
    #[serde(default)]
    pub strokes: Vec<RemoveStroke>,
}

impl RemoveEditState {
    pub fn is_empty(&self) -> bool {
        self.strokes.is_empty()
    }
}

/// A compact native-resolution binary brush mask. `pixels` is local to `bounds`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemoveMask {
    pub bounds: NativeRect,
    pub pixels: Vec<u8>,
}

impl RemoveMask {
    pub fn is_empty(&self) -> bool {
        self.pixels.iter().all(|value| *value == 0)
    }

    pub fn contains_global(&self, x: u32, y: u32) -> bool {
        if x < self.bounds.x
            || y < self.bounds.y
            || x >= self.bounds.right()
            || y >= self.bounds.bottom()
        {
            return false;
        }
        let local_x = (x - self.bounds.x) as usize;
        let local_y = (y - self.bounds.y) as usize;
        self.pixels[local_y * self.bounds.width as usize + local_x] != 0
    }
}

pub fn adaptive_remove_dilation(stroke: &[RemoveBrushPoint]) -> u32 {
    if stroke.is_empty() {
        return 0;
    }
    let average_radius = stroke
        .iter()
        .map(|point| point.radius.max(0.0))
        .sum::<f32>()
        / stroke.len() as f32;
    (average_radius * 0.06).round().clamp(1.0, 12.0) as u32
}

pub fn rasterize_remove_brush(
    image_width: u32,
    image_height: u32,
    brush: &RemoveBrushStroke,
) -> Option<RemoveMask> {
    if image_width == 0 || image_height == 0 || brush.points.is_empty() {
        return None;
    }
    let dilation = brush.dilation_radius as f32;
    let mut x0 = image_width as f32;
    let mut y0 = image_height as f32;
    let mut x1 = 0.0f32;
    let mut y1 = 0.0f32;
    for point in &brush.points {
        if !point.x.is_finite() || !point.y.is_finite() || !point.radius.is_finite() {
            continue;
        }
        let radius = point.radius.max(0.5) + dilation;
        x0 = x0.min(point.x - radius - 1.0);
        y0 = y0.min(point.y - radius - 1.0);
        x1 = x1.max(point.x + radius + 1.0);
        y1 = y1.max(point.y + radius + 1.0);
    }
    let left = x0.floor().max(0.0).min(image_width as f32) as u32;
    let top = y0.floor().max(0.0).min(image_height as f32) as u32;
    let right = x1.ceil().max(0.0).min(image_width as f32) as u32;
    let bottom = y1.ceil().max(0.0).min(image_height as f32) as u32;
    if right <= left || bottom <= top {
        return None;
    }
    let bounds = NativeRect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    };
    let mut pixels = vec![0u8; bounds.width as usize * bounds.height as usize];

    // The UI records points at roughly 0.22 brush radii, so filled native-pixel
    // discs overlap substantially. This both preserves the actual sampled brush
    // geometry and avoids gaps without inventing a second, lower-resolution path.
    for point in &brush.points {
        let radius = point.radius.max(0.5) + dilation;
        paint_disc(&mut pixels, bounds, point.x, point.y, radius);
    }

    Some(RemoveMask { bounds, pixels })
}

fn paint_disc(pixels: &mut [u8], bounds: NativeRect, center_x: f32, center_y: f32, radius: f32) {
    let y_start = (center_y - radius)
        .floor()
        .max(bounds.y as f32)
        .min(bounds.bottom() as f32) as u32;
    let y_end = (center_y + radius)
        .ceil()
        .max(bounds.y as f32)
        .min(bounds.bottom() as f32) as u32;
    let radius_sq = radius * radius;
    for y in y_start..y_end {
        let dy = y as f32 + 0.5 - center_y;
        let remaining = (radius_sq - dy * dy).max(0.0).sqrt();
        let x_start = (center_x - remaining)
            .floor()
            .max(bounds.x as f32)
            .min(bounds.right() as f32) as u32;
        let x_end = (center_x + remaining)
            .ceil()
            .max(bounds.x as f32)
            .min(bounds.right() as f32) as u32;
        let row = (y - bounds.y) as usize * bounds.width as usize;
        for x in x_start..x_end {
            pixels[row + (x - bounds.x) as usize] = 255;
        }
    }
}

/// Plan square local-context crops. Small strokes get one ~3x context square.
/// Large strokes are covered by overlapping 1024-native-pixel context windows
/// rather than shrinking an enormous source region into the 512 model tensor.
pub fn plan_remove_context_crops(
    image_width: u32,
    image_height: u32,
    mask: &RemoveMask,
) -> Vec<RemoveContextCrop> {
    if image_width == 0 || image_height == 0 || mask.is_empty() {
        return Vec::new();
    }
    let shortest = image_width.min(image_height).max(1);
    let max_context = (BIG_LAMA_INPUT_EDGE * 2).min(shortest).max(1);
    let mask_edge = mask.bounds.width.max(mask.bounds.height).max(1);
    let desired = mask_edge.saturating_mul(3).max(384).min(shortest);
    if desired <= max_context || mask_edge <= max_context / 2 {
        let context = square_inside_image(
            image_width,
            image_height,
            mask.bounds.x + mask.bounds.width / 2,
            mask.bounds.y + mask.bounds.height / 2,
            desired.min(max_context),
        );
        return vec![RemoveContextCrop {
            context,
            target: context,
        }];
    }

    // For masks larger than one context window, use overlapping target cores.
    // Start at a 2x context ratio; for truly enormous masks, grow the target
    // core (while retaining overlap) so one stroke can never exceed the
    // persisted patch safety cap. The model still sees a full local context
    // square rather than a globally downscaled photograph.
    let mut core_edge = (max_context / 2).max(96);
    let max_core_edge = (max_context * 3 / 4).max(core_edge);
    loop {
        let overlap = (core_edge / 4).max(24);
        let stride = core_edge.saturating_sub(overlap).max(1);
        let tiles_x = tile_count_for_span(mask.bounds.width, core_edge, stride);
        let tiles_y = tile_count_for_span(mask.bounds.height, core_edge, stride);
        if tiles_x.saturating_mul(tiles_y) <= REMOVE_MAX_PATCHES_PER_STROKE
            || core_edge >= max_core_edge
        {
            break;
        }
        core_edge = core_edge.saturating_add(32).min(max_core_edge);
    }
    let overlap = (core_edge / 4).max(24);
    let stride = core_edge.saturating_sub(overlap).max(1);
    let mut crops = Vec::new();
    let mut core_y = mask.bounds.y;
    while core_y < mask.bounds.bottom() {
        let mut core_x = mask.bounds.x;
        while core_x < mask.bounds.right() {
            let core_w = core_edge.min(mask.bounds.right() - core_x);
            let core_h = core_edge.min(mask.bounds.bottom() - core_y);
            if mask_region_has_pixels(mask, NativeRect {
                x: core_x,
                y: core_y,
                width: core_w,
                height: core_h,
            }) {
                let center_x = core_x + core_w / 2;
                let center_y = core_y + core_h / 2;
                let context = square_inside_image(
                    image_width,
                    image_height,
                    center_x,
                    center_y,
                    max_context,
                );
                let planned = RemoveContextCrop {
                    context,
                    target: NativeRect {
                        x: core_x,
                        y: core_y,
                        width: core_w,
                        height: core_h,
                    },
                };
                if !crops.contains(&planned) {
                    crops.push(planned);
                }
            }
            if core_x.saturating_add(core_w) >= mask.bounds.right() {
                break;
            }
            core_x = core_x.saturating_add(stride);
        }
        let core_h = core_edge.min(mask.bounds.bottom() - core_y);
        if core_y.saturating_add(core_h) >= mask.bounds.bottom() {
            break;
        }
        core_y = core_y.saturating_add(stride);
    }
    crops
}

fn tile_count_for_span(span: u32, core_edge: u32, stride: u32) -> usize {
    if span <= core_edge {
        return 1;
    }
    1usize.saturating_add(
        span.saturating_sub(core_edge)
            .div_ceil(stride.max(1)) as usize,
    )
}

fn mask_region_has_pixels(mask: &RemoveMask, rect: NativeRect) -> bool {
    let Some(intersection) = mask.bounds.intersect(rect) else {
        return false;
    };
    for y in intersection.y..intersection.bottom() {
        let row = (y - mask.bounds.y) as usize * mask.bounds.width as usize;
        for x in intersection.x..intersection.right() {
            if mask.pixels[row + (x - mask.bounds.x) as usize] != 0 {
                return true;
            }
        }
    }
    false
}

fn square_inside_image(
    image_width: u32,
    image_height: u32,
    center_x: u32,
    center_y: u32,
    requested_edge: u32,
) -> NativeRect {
    let edge = requested_edge
        .max(1)
        .min(image_width.max(1))
        .min(image_height.max(1));
    let half = edge / 2;
    let mut x = center_x.saturating_sub(half);
    let mut y = center_y.saturating_sub(half);
    if x.saturating_add(edge) > image_width {
        x = image_width.saturating_sub(edge);
    }
    if y.saturating_add(edge) > image_height {
        y = image_height.saturating_sub(edge);
    }
    NativeRect {
        x,
        y,
        width: edge,
        height: edge,
    }
}

/// Returns the logical RGB white-balance gains used by the demosaiced scene
/// texture. The two physical green planes are averaged because Remove patches
/// live after demosaic, where they have already collapsed to logical RGB.
pub fn remove_scene_white_balance(raw: &LoadedRaw, exposure: &ExposureParams) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return [1.0; 3];
    }
    let (wb, _, _) = raw.adjusted_white_balance_and_camera_transform(
        exposure.temperature,
        exposure.tint,
    );
    let green = 0.5 * (wb[1] + wb[3]);
    [wb[0].max(1e-8), green.max(1e-8), wb[2].max(1e-8)]
}

pub fn pipeline_scene_to_canonical_remove_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    rgb: [f32; 3],
) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let wb = remove_scene_white_balance(raw, exposure);
    [rgb[0] / wb[0], rgb[1] / wb[1], rgb[2] / wb[2]]
}

pub fn canonical_remove_scene_to_pipeline_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    rgb: [f32; 3],
) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let wb = remove_scene_white_balance(raw, exposure);
    [rgb[0] * wb[0], rgb[1] * wb[1], rgb[2] * wb[2]]
}

pub fn pipeline_scene_to_working_rec2020(raw: &LoadedRaw, rgb: [f32; 3]) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let m = &raw.cam_to_srgb;
    [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
    ]
}

fn invert_remove_camera_matrix(raw: &LoadedRaw) -> Option<[[f32; 3]; 3]> {
    if raw.is_pre_demosaiced_raster() {
        return Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
    }
    let m = &raw.cam_to_srgb;
    let a = m[0][0];
    let b = m[0][1];
    let c = m[0][2];
    let d = m[1][0];
    let e = m[1][1];
    let f = m[1][2];
    let g = m[2][0];
    let h = m[2][1];
    let i = m[2][2];
    let det = a * (e * i - f * h) - b * (d * i - f * g) + c * (d * h - e * g);
    if !det.is_finite() || det.abs() <= 1e-10 {
        return None;
    }
    let inv = 1.0 / det;
    Some([
        [
            (e * i - f * h) * inv,
            (c * h - b * i) * inv,
            (b * f - c * e) * inv,
        ],
        [
            (f * g - d * i) * inv,
            (a * i - c * g) * inv,
            (c * d - a * f) * inv,
        ],
        [
            (d * h - e * g) * inv,
            (b * g - a * h) * inv,
            (a * e - b * d) * inv,
        ],
    ])
}

pub fn working_rec2020_to_canonical_remove_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    rgb: [f32; 3],
) -> [f32; 3] {
    if raw.is_pre_demosaiced_raster() {
        return rgb;
    }
    let Some(m) = invert_remove_camera_matrix(raw) else {
        return rgb;
    };
    let camera_wb = [
        m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2],
        m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2],
        m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2],
    ];
    pipeline_scene_to_canonical_remove_scene(raw, exposure, camera_wb)
}

/// Build a stable, photographic model view from the scene boundary. The
/// exposure gain is crop-global and the luminance shoulder is analytically
/// invertible, so Big-LaMa sees a normal gamma-encoded image while its result
/// can still be cached upstream of all editable Develop controls.
pub fn remove_scene_to_model_srgb(
    raw: &LoadedRaw,
    scene_rgb: [f32; 3],
    view_gain: f32,
) -> [f32; 3] {
    let working = pipeline_scene_to_working_rec2020(raw, scene_rgb);
    let scaled = working.map(|value| value.max(0.0) * view_gain.max(1e-6));
    let luma = (scaled[0] * 0.2627 + scaled[1] * 0.6780 + scaled[2] * 0.0593).max(0.0);
    let shoulder = 1.0 / (1.0 + luma);
    display_linear_rec2020_to_model_srgb(scaled.map(|value| value * shoulder))
}

pub fn remove_model_srgb_to_canonical_scene(
    raw: &LoadedRaw,
    exposure: &ExposureParams,
    srgb: [f32; 3],
    view_gain: f32,
) -> [f32; 3] {
    let mapped = model_srgb_to_display_linear_rec2020(srgb);
    let mapped_luma =
        (mapped[0] * 0.2627 + mapped[1] * 0.6780 + mapped[2] * 0.0593).clamp(0.0, 0.985);
    let undo_shoulder = 1.0 / (1.0 - mapped_luma).max(0.015);
    let gain = view_gain.max(1e-6);
    let working = mapped.map(|value| value * undo_shoulder / gain);
    working_rec2020_to_canonical_remove_scene(raw, exposure, working)
}

pub fn remove_model_view_gain(raw: &LoadedRaw, scene_rgb: &[f32]) -> f32 {
    let mut luminance = Vec::new();
    for pixel in scene_rgb.chunks_exact(3).step_by(4) {
        let working = pipeline_scene_to_working_rec2020(raw, [pixel[0], pixel[1], pixel[2]]);
        let value = working[0] * 0.2627 + working[1] * 0.6780 + working[2] * 0.0593;
        if value.is_finite() && value > 1e-6 {
            luminance.push(value.min(64.0));
        }
    }
    if luminance.is_empty() {
        return 1.0;
    }
    luminance.sort_by(f32::total_cmp);
    let index = ((luminance.len() - 1) as f32 * 0.75).round() as usize;
    let p75 = luminance[index].max(1e-5);
    // Map the 75th percentile to about 0.55 after the reversible shoulder.
    let target_linear = 0.55 / (1.0 - 0.55);
    (target_linear / p75).clamp(0.25, 64.0)
}

/// Apply cached Remove patches, in stroke order, to one display-linear Rec.2020
/// region. This is used by both local model input construction and export.
pub fn composite_remove_edits_into_linear_region(
    edits: &RemoveEditState,
    region: NativeRect,
    rgb: &mut [f32],
) {
    if rgb.len() != region.width as usize * region.height as usize * 3 {
        return;
    }
    for stroke in &edits.strokes {
        for patch in &stroke.patches {
            composite_patch_into_linear_region(patch, region, rgb);
        }
    }
}

pub fn composite_patch_into_linear_region(
    patch: &RemovePatch,
    region: NativeRect,
    rgb: &mut [f32],
) {
    // Scene-space patches belong upstream of the Develop graph and are applied
    // directly to the GPU scene texture. This post-adjustment helper remains
    // only for loading schema-12 legacy patches.
    if patch.has_scene_pixels() {
        return;
    }
    let Some(intersection) = patch.bounds.intersect(region) else {
        return;
    };
    for y in intersection.y..intersection.bottom() {
        let patch_y = (y - patch.bounds.y) as usize;
        let region_y = (y - region.y) as usize;
        for x in intersection.x..intersection.right() {
            let patch_x = (x - patch.bounds.x) as usize;
            let region_x = (x - region.x) as usize;
            let patch_index = patch_y * patch.bounds.width as usize + patch_x;
            let alpha = patch.alpha[patch_index] as f32 / 255.0;
            if alpha <= 0.0 {
                continue;
            }
            let rgb_index = patch_index * 3;
            let repaired = model_srgb_to_display_linear_rec2020([
                patch.rgb_srgb16[rgb_index] as f32 / 65535.0,
                patch.rgb_srgb16[rgb_index + 1] as f32 / 65535.0,
                patch.rgb_srgb16[rgb_index + 2] as f32 / 65535.0,
            ]);
            let out_index = (region_y * region.width as usize + region_x) * 3;
            for channel in 0..3 {
                rgb[out_index + channel] =
                    rgb[out_index + channel] * (1.0 - alpha) + repaired[channel] * alpha;
            }
        }
    }
}

pub fn display_linear_rec2020_to_model_srgb(rgb: [f32; 3]) -> [f32; 3] {
    let linear = [
        1.660_491 * rgb[0] - 0.587_641_1 * rgb[1] - 0.072_849_9 * rgb[2],
        -0.124_550_5 * rgb[0] + 1.132_899_9 * rgb[1] - 0.008_349_4 * rgb[2],
        -0.018_150_8 * rgb[0] - 0.100_578_9 * rgb[1] + 1.118_729_7 * rgb[2],
    ];
    perceptual_gamut_compress(linear).map(srgb_encode)
}

pub fn model_srgb_to_display_linear_rec2020(rgb: [f32; 3]) -> [f32; 3] {
    let linear = rgb.map(srgb_decode);
    [
        0.627_403_9 * linear[0] + 0.329_283 * linear[1] + 0.043_313_1 * linear[2],
        0.069_097_3 * linear[0] + 0.919_540_4 * linear[1] + 0.011_362_3 * linear[2],
        0.016_391_4 * linear[0] + 0.088_013_3 * linear[1] + 0.895_595_3 * linear[2],
    ]
}

fn perceptual_gamut_compress(rgb: [f32; 3]) -> [f32; 3] {
    let min = rgb[0].min(rgb[1]).min(rgb[2]);
    let max = rgb[0].max(rgb[1]).max(rgb[2]);
    if min >= 0.0 && max <= 1.0 {
        return rgb;
    }
    let luma = (rgb[0] * 0.212_672_9 + rgb[1] * 0.715_152_2 + rgb[2] * 0.072_175)
        .clamp(0.0, 1.0);
    let mut scale: f32 = 1.0;
    for value in rgb {
        let delta = value - luma;
        if delta > 0.0 {
            scale = scale.min((1.0 - luma) / delta);
        } else if delta < 0.0 {
            scale = scale.min((0.0 - luma) / delta);
        }
    }
    rgb.map(|value| (luma + (value - luma) * scale.clamp(0.0, 1.0)).clamp(0.0, 1.0))
}

fn srgb_encode(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.003_130_8 {
        value * 12.92
    } else {
        1.055 * value.powf(1.0 / 2.4) - 0.055
    }
}

fn srgb_decode(value: f32) -> f32 {
    let value = value.clamp(0.0, 1.0);
    if value <= 0.040_45 {
        value / 12.92
    } else {
        ((value + 0.055) / 1.055).powf(2.4)
    }
}

mod arc_u8_base64 {
    use super::*;
    use base64::Engine as _;

    pub fn serialize<S>(bytes: &Arc<[u8]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes.as_ref()))
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

mod arc_u16_le_base64 {
    use super::*;
    use base64::Engine as _;

    pub fn serialize<S>(values: &Arc<[u16]>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut bytes = Vec::with_capacity(values.len().saturating_mul(2));
        for value in values.iter().copied() {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        serializer.serialize_str(&base64::engine::general_purpose::STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[u16]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded = String::deserialize(deserializer)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(serde::de::Error::custom)?;
        if bytes.len() % 2 != 0 {
            return Err(serde::de::Error::custom("RGB16 patch byte length is odd"));
        }
        let values = bytes
            .chunks_exact(2)
            .map(|chunk| u16::from_le_bytes([chunk[0], chunk[1]]))
            .collect::<Vec<_>>();
        Ok(Arc::from(values))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remove_brush_raster_is_hard_binary_before_dilation() {
        let brush = RemoveBrushStroke {
            points: vec![RemoveBrushPoint {
                x: 32.0,
                y: 24.0,
                radius: 7.5,
            }],
            dilation_radius: 0,
        };
        let mask = rasterize_remove_brush(64, 48, &brush).unwrap();
        assert!(mask.pixels.iter().all(|value| *value == 0 || *value == 255));
        assert!(mask.pixels.iter().any(|value| *value == 255));
        assert!(mask.pixels.iter().any(|value| *value == 0));
    }

    #[test]
    fn small_mask_gets_three_x_context_square() {
        let brush = RemoveBrushStroke {
            points: vec![RemoveBrushPoint {
                x: 3500.0,
                y: 3000.0,
                radius: 150.0,
            }],
            dilation_radius: 4,
        };
        let mask = rasterize_remove_brush(7000, 6000, &brush).unwrap();
        let crops = plan_remove_context_crops(7000, 6000, &mask);
        assert_eq!(crops.len(), 1);
        assert_eq!(crops[0].context.width, crops[0].context.height);
        assert!(crops[0].context.width >= 900);
        assert!(crops[0].context.width <= BIG_LAMA_INPUT_EDGE * 2);
        assert_eq!(crops[0].target, crops[0].context);
    }

    #[test]
    fn large_mask_uses_overlapping_local_crops() {
        let mut points = Vec::new();
        for x in (1000..4000).step_by(120) {
            points.push(RemoveBrushPoint {
                x: x as f32,
                y: 2500.0,
                radius: 90.0,
            });
        }
        let brush = RemoveBrushStroke {
            points,
            dilation_radius: 5,
        };
        let mask = rasterize_remove_brush(7000, 6000, &brush).unwrap();
        let crops = plan_remove_context_crops(7000, 6000, &mask);
        assert!(crops.len() > 1);
        assert!(crops.iter().all(|crop| {
            crop.context.width == crop.context.height
                && crop.context.width <= BIG_LAMA_INPUT_EDGE * 2
                && crop.target.width < crop.context.width
        }));
    }

    #[test]
    fn huge_contiguous_mask_keeps_unmasked_context_per_tile() {
        let mask = RemoveMask {
            bounds: NativeRect {
                x: 1000,
                y: 1000,
                width: 3000,
                height: 2200,
            },
            pixels: vec![255; 3000 * 2200],
        };
        let crops = plan_remove_context_crops(7000, 6000, &mask);
        assert!(crops.len() > 1);
        assert!(crops.iter().all(|crop| {
            crop.target.width < crop.context.width
                && crop.target.height < crop.context.height
                && crop.context.intersect(crop.target) == Some(crop.target)
        }));
    }

    #[test]
    fn full_large_image_stays_within_patch_cap() {
        let mask = RemoveMask {
            bounds: NativeRect {
                x: 0,
                y: 0,
                width: 7000,
                height: 6000,
            },
            pixels: vec![255; 7000 * 6000],
        };
        let crops = plan_remove_context_crops(7000, 6000, &mask);
        assert!(!crops.is_empty());
        assert!(crops.len() <= REMOVE_MAX_PATCHES_PER_STROKE);
    }

    #[test]
    fn remove_model_view_round_trips_scene_linear_raster() {
        let raw = LoadedRaw::from_scene_linear_rec2020(1, 1, vec![0.12, 0.18, 0.09]).unwrap();
        let exposure = ExposureParams::default();
        let scene = [0.12, 0.18, 0.09];
        let view_gain = 1.75;
        let model = remove_scene_to_model_srgb(&raw, scene, view_gain);
        let restored = remove_model_srgb_to_canonical_scene(&raw, &exposure, model, view_gain);
        for channel in 0..3 {
            assert!(
                (restored[channel] - scene[channel]).abs() < 2e-4,
                "channel {channel}: scene={} restored={}",
                scene[channel],
                restored[channel]
            );
        }
    }

    #[test]
    fn compositing_does_not_touch_outside_patch() {
        let patch = RemovePatch::new(
            NativeRect {
                x: 1,
                y: 1,
                width: 1,
                height: 1,
            },
            vec![65535, 0, 0],
            vec![255],
        )
        .unwrap();
        let mut rgb = vec![0.25f32; 3 * 3 * 3];
        let before = rgb.clone();
        composite_patch_into_linear_region(
            &patch,
            NativeRect {
                x: 0,
                y: 0,
                width: 3,
                height: 3,
            },
            &mut rgb,
        );
        for pixel in 0..9 {
            if pixel != 4 {
                assert_eq!(&rgb[pixel * 3..pixel * 3 + 3], &before[pixel * 3..pixel * 3 + 3]);
            }
        }
        assert_ne!(&rgb[12..15], &before[12..15]);
    }
}
