use std::f32::consts::TAU;
use std::sync::Arc;

pub const MAX_LOCAL_MASKS: usize = 8;
pub const MASK_ATLAS_EDGE_DESKTOP: u32 = 2048;
pub const MASK_ATLAS_EDGE_ANDROID: u32 = 1024;

pub const fn mask_atlas_edge() -> u32 {
    if cfg!(target_os = "android") {
        MASK_ATLAS_EDGE_ANDROID
    } else {
        MASK_ATLAS_EDGE_DESKTOP
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MaskKind {
    #[default]
    Brush,
    Radial,
    Linear,
    Subject,
    Background,
    Object,
    Landscape,
    LuminanceRange,
    ColorRange,
    DepthRange,
}

impl MaskKind {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Brush => "Brush",
            Self::Radial => "Radial Gradient",
            Self::Linear => "Linear Gradient",
            Self::Subject => "Select Subject",
            Self::Background => "Select Background",
            Self::Object => "Select Object",
            Self::Landscape => "Select Landscape",
            Self::LuminanceRange => "Luminance Range",
            Self::ColorRange => "Color Range",
            Self::DepthRange => "Depth Range",
        }
    }

    pub const fn short_label(self) -> &'static str {
        match self {
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
                | Self::Radial
                | Self::Linear
                | Self::Subject
                | Self::Background
                | Self::LuminanceRange
                | Self::ColorRange
        )
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
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
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BrushDab {
    pub center: [f32; 2],
    /// Positive dabs paint; negative dabs erase.
    pub opacity: f32,
    /// Captured when the dab is painted so changing the tool does not reshape
    /// previous strokes. Radius is relative to the shorter image edge.
    pub size: f32,
    pub feather: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaskImage {
    pub width: u32,
    pub height: u32,
    pub pixels: Arc<[u8]>,
}

impl MaskImage {
    pub fn new(width: u32, height: u32, pixels: Vec<u8>) -> Option<Self> {
        (pixels.len() == width as usize * height as usize).then(|| Self {
            width,
            height,
            pixels: pixels.into(),
        })
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaskRgbImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Arc<[u8]>,
}

impl MaskRgbImage {
    pub fn new(width: u32, height: u32, rgba: Vec<u8>) -> Option<Self> {
        (rgba.len() == width as usize * height as usize * 4).then(|| Self {
            width,
            height,
            rgba: rgba.into(),
        })
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

#[derive(Clone, Debug, PartialEq)]
pub enum MaskGeometry {
    Brush {
        /// Radius as a fraction of the image's shorter edge.
        size: f32,
        feather: f32,
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
        feather: f32,
    },
    LuminanceRange {
        source: Option<MaskRgbImage>,
        low: f32,
        high: f32,
        feather: f32,
    },
    ColorRange {
        source: Option<MaskRgbImage>,
        sample: [f32; 3],
        tolerance: f32,
        feather: f32,
        sampled: bool,
    },
    Placeholder,
}

impl MaskGeometry {
    pub fn for_kind(kind: MaskKind) -> Self {
        match kind {
            MaskKind::Brush => Self::Brush {
                size: 0.055,
                feather: 0.55,
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
                feather: 0.0,
            },
            MaskKind::LuminanceRange => Self::LuminanceRange {
                source: None,
                low: 0.2,
                high: 0.8,
                feather: 0.15,
            },
            MaskKind::ColorRange => Self::ColorRange {
                source: None,
                sample: [0.5; 3],
                tolerance: 0.18,
                feather: 0.12,
                sampled: false,
            },
            _ => Self::Placeholder,
        }
    }

    pub fn is_initialized(&self) -> bool {
        match self {
            Self::Brush { dabs, .. } => !dabs.is_empty(),
            Self::Radial { initialized, .. } | Self::Linear { initialized, .. } => *initialized,
            Self::Ai { mask, .. } => mask.is_some(),
            Self::LuminanceRange { source, .. } => source.is_some(),
            Self::ColorRange {
                source, sampled, ..
            } => source.is_some() && *sampled,
            Self::Placeholder => false,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct MaskComponent {
    pub kind: MaskKind,
    pub combine: MaskCombineMode,
    pub enabled: bool,
    pub invert: bool,
    pub geometry: MaskGeometry,
}

impl MaskComponent {
    pub fn new(kind: MaskKind, combine: MaskCombineMode) -> Self {
        Self {
            kind,
            combine,
            enabled: true,
            invert: false,
            geometry: MaskGeometry::for_kind(kind),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LocalAdjustments {
    pub exposure: f32,
    pub contrast: f32,
    pub highlights: f32,
    pub shadows: f32,
    pub whites: f32,
    pub blacks: f32,
    pub temperature: f32,
    pub tint: f32,
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
        }
    }
}

impl LocalAdjustments {
    pub fn is_neutral(self) -> bool {
        self == Self::default()
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

#[derive(Clone, Debug, PartialEq)]
pub struct LocalMask {
    pub name: String,
    pub enabled: bool,
    pub opacity: f32,
    pub components: Vec<MaskComponent>,
    pub adjustments: LocalAdjustments,
}

impl LocalMask {
    pub fn new(kind: MaskKind, number: usize) -> Self {
        Self {
            name: format!("Mask {number}"),
            enabled: true,
            opacity: 1.0,
            components: vec![MaskComponent::new(kind, MaskCombineMode::Add)],
            adjustments: LocalAdjustments::default(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct MaskStack {
    pub masks: Vec<LocalMask>,
    pub selected_mask: Option<usize>,
    pub selected_component: Option<usize>,
}

impl MaskStack {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn add_mask(&mut self, kind: MaskKind) -> Option<(usize, usize)> {
        if self.masks.len() >= MAX_LOCAL_MASKS || !kind.is_available() {
            return None;
        }
        let mask_index = self.masks.len();
        self.masks.push(LocalMask::new(kind, mask_index + 1));
        self.selected_mask = Some(mask_index);
        self.selected_component = Some(0);
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

    pub fn remove_selected_mask(&mut self) -> Option<usize> {
        let index = self.selected_mask?;
        if index >= self.masks.len() {
            return None;
        }
        self.masks.remove(index);
        for (number, mask) in self.masks.iter_mut().enumerate() {
            if mask.name.starts_with("Mask ") {
                mask.name = format!("Mask {}", number + 1);
            }
        }
        if self.masks.is_empty() {
            self.selected_mask = None;
            self.selected_component = None;
        } else {
            self.selected_mask = Some(index.min(self.masks.len() - 1));
            self.selected_component = Some(0);
        }
        Some(index)
    }

    pub fn remove_selected_component(&mut self) -> Option<(usize, usize)> {
        let mask_index = self.selected_mask?;
        let component_index = self.selected_component?;
        let mask = self.masks.get_mut(mask_index)?;
        if mask.components.len() <= 1 || component_index >= mask.components.len() {
            return None;
        }
        mask.components.remove(component_index);
        self.selected_component = Some(component_index.min(mask.components.len() - 1));
        Some((mask_index, component_index))
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

    pub fn rasterize_layer(
        &self,
        layer: usize,
        atlas_width: u32,
        atlas_height: u32,
        image_width: u32,
        image_height: u32,
    ) -> Vec<u8> {
        let len = atlas_width as usize * atlas_height as usize;
        let Some(mask) = self.masks.get(layer) else {
            return vec![0; len];
        };
        if mask.components.is_empty() {
            return vec![0; len];
        }

        let mut combined = vec![0.0f32; len];
        let mut has_component = false;
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
            );
            if component.invert {
                for value in &mut coverage {
                    *value = 1.0 - *value;
                }
            }

            if !has_component {
                if component.combine == MaskCombineMode::Add {
                    combined.copy_from_slice(&coverage);
                }
                has_component = true;
                continue;
            }
            match component.combine {
                MaskCombineMode::Add => {
                    for (dst, src) in combined.iter_mut().zip(coverage) {
                        *dst = dst.max(src);
                    }
                }
                MaskCombineMode::Subtract => {
                    for (dst, src) in combined.iter_mut().zip(coverage) {
                        *dst *= 1.0 - src;
                    }
                }
                MaskCombineMode::Intersect => {
                    for (dst, src) in combined.iter_mut().zip(coverage) {
                        *dst *= src;
                    }
                }
            }
        }

        if !has_component {
            return vec![0; len];
        }
        let opacity = mask.opacity.clamp(0.0, 1.0);
        combined
            .into_iter()
            .map(|value| (value.clamp(0.0, 1.0) * opacity * 255.0 + 0.5) as u8)
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
        let mut coverage = rasterize_component(component, width, height, image_width, image_height);
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
) -> Vec<f32> {
    match &component.geometry {
        MaskGeometry::Brush { dabs, .. } => {
            rasterize_brush(width, height, image_width, image_height, dabs)
        }
        MaskGeometry::Radial {
            center,
            radius,
            rotation,
            feather,
            initialized: true,
        } => rasterize_radial(
            width,
            height,
            image_width,
            image_height,
            *center,
            *radius,
            *rotation,
            *feather,
        ),
        MaskGeometry::Linear {
            start,
            end,
            feather,
            initialized: true,
        } => rasterize_linear(
            width,
            height,
            image_width,
            image_height,
            *start,
            *end,
            *feather,
        ),
        MaskGeometry::Ai {
            mask: Some(mask),
            feather,
        } => {
            let mut coverage = rasterize_mask_image(width, height, mask);
            if component.kind == MaskKind::Background {
                for value in &mut coverage {
                    *value = 1.0 - *value;
                }
            }
            feather_probability_mask(&mut coverage, width, height, *feather);
            coverage
        }
        MaskGeometry::LuminanceRange {
            source: Some(source),
            low,
            high,
            feather,
        } => rasterize_luminance_range(width, height, source, *low, *high, *feather),
        MaskGeometry::ColorRange {
            source: Some(source),
            sample,
            tolerance,
            feather,
            sampled: true,
        } => rasterize_color_range(width, height, source, *sample, *tolerance, *feather),
        _ => vec![0.0; width as usize * height as usize],
    }
}

fn rasterize_mask_image(width: u32, height: u32, mask: &MaskImage) -> Vec<f32> {
    let mut out = vec![0.0; width as usize * height as usize];
    for y in 0..height {
        let source_y = ((y as f32 + 0.5) * mask.height as f32 / height as f32 - 0.5)
            .round()
            .clamp(0.0, mask.height.saturating_sub(1) as f32) as usize;
        for x in 0..width {
            let source_x = ((x as f32 + 0.5) * mask.width as f32 / width as f32 - 0.5)
                .round()
                .clamp(0.0, mask.width.saturating_sub(1) as f32)
                as usize;
            out[y as usize * width as usize + x as usize] =
                mask.pixels[source_y * mask.width as usize + source_x] as f32 / 255.0;
        }
    }
    out
}

fn feather_probability_mask(mask: &mut [f32], width: u32, height: u32, feather: f32) {
    let radius = (feather.clamp(0.0, 1.0).powf(1.4) * 32.0).round() as usize;
    if radius == 0 || width == 0 || height == 0 {
        return;
    }
    let width = width as usize;
    let height = height as usize;
    let mut horizontal = vec![0.0; mask.len()];
    let mut row_prefix = vec![0.0f32; width + 1];
    for y in 0..height {
        let row = &mask[y * width..(y + 1) * width];
        row_prefix.fill(0.0);
        for x in 0..width {
            row_prefix[x + 1] = row_prefix[x] + row[x];
        }
        for x in 0..width {
            let from = x.saturating_sub(radius);
            let to = (x + radius + 1).min(width);
            horizontal[y * width + x] = (row_prefix[to] - row_prefix[from]) / (to - from) as f32;
        }
    }
    let mut prefix = vec![0.0f32; height + 1];
    for x in 0..width {
        prefix.fill(0.0);
        for y in 0..height {
            prefix[y + 1] = prefix[y] + horizontal[y * width + x];
        }
        for y in 0..height {
            let from = y.saturating_sub(radius);
            let to = (y + radius + 1).min(height);
            mask[y * width + x] = (prefix[to] - prefix[from]) / (to - from) as f32;
        }
    }
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
    coverage: impl Fn([f32; 3]) -> f32,
) -> Vec<f32> {
    let mut out = vec![0.0; width as usize * height as usize];
    for y in 0..height {
        let source_y = (y as u64 * source.height as u64 / height.max(1) as u64)
            .min(source.height.saturating_sub(1) as u64) as usize;
        for x in 0..width {
            let source_x = (x as u64 * source.width as u64 / width.max(1) as u64)
                .min(source.width.saturating_sub(1) as u64) as usize;
            let index = (source_y * source.width as usize + source_x) * 4;
            let rgb = [
                source.rgba[index] as f32 / 255.0,
                source.rgba[index + 1] as f32 / 255.0,
                source.rgba[index + 2] as f32 / 255.0,
            ];
            out[y as usize * width as usize + x as usize] = coverage(rgb).clamp(0.0, 1.0);
        }
    }
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

fn rasterize_brush(
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
    dabs: &[BrushDab],
) -> Vec<f32> {
    let mut out = vec![0.0f32; width as usize * height as usize];
    let image_min = image_width.min(image_height).max(1) as f32;

    for dab in dabs {
        let radius_image = dab.size.clamp(0.0025, 0.5) * image_min;
        let radius_x = radius_image * width as f32 / image_width.max(1) as f32;
        let radius_y = radius_image * height as f32 / image_height.max(1) as f32;
        let bbox_x = radius_x.ceil().max(1.0) as i32 + 1;
        let bbox_y = radius_y.ceil().max(1.0) as i32 + 1;
        let feather = dab.feather.clamp(0.0, 1.0);
        // UV coordinates describe continuous image space; texel samples live
        // at x + 0.5/y + 0.5. Keeping the center in continuous texel space
        // makes even-sized atlases symmetric around a centered brush dab.
        let center_x = dab.center[0].clamp(0.0, 1.0) * width as f32;
        let center_y = dab.center[1].clamp(0.0, 1.0) * height as f32;
        let min_x = (center_x.floor() as i32 - bbox_x).max(0);
        let max_x = (center_x.ceil() as i32 + bbox_x).min(width as i32 - 1);
        let min_y = (center_y.floor() as i32 - bbox_y).max(0);
        let max_y = (center_y.ceil() as i32 + bbox_y).min(height as i32 - 1);
        let antialias = (1.0 / radius_x.max(radius_y).max(1.0)).clamp(0.002, 0.25);
        let inner = (1.0 - feather).clamp(0.0, 1.0 - antialias);

        for y in min_y..=max_y {
            let dy = (y as f32 + 0.5 - center_y) / radius_y.max(0.5);
            for x in min_x..=max_x {
                let dx = (x as f32 + 0.5 - center_x) / radius_x.max(0.5);
                let distance = (dx * dx + dy * dy).sqrt();
                if distance >= 1.0 + antialias {
                    continue;
                }
                let coverage = 1.0 - smoothstep(inner, 1.0 + antialias, distance);
                let index = y as usize * width as usize + x as usize;
                if dab.opacity >= 0.0 {
                    out[index] = out[index].max(coverage * dab.opacity.clamp(0.0, 1.0));
                } else {
                    out[index] *= 1.0 - coverage * (-dab.opacity).clamp(0.0, 1.0);
                }
            }
        }
    }
    out
}

fn rasterize_radial(
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
    center: [f32; 2],
    radius: [f32; 2],
    rotation: f32,
    feather: f32,
) -> Vec<f32> {
    let mut out = vec![0.0f32; width as usize * height as usize];
    let cos_r = rotation.cos();
    let sin_r = rotation.sin();
    let rx = (radius[0].abs() * image_width.max(1) as f32).max(1.0);
    let ry = (radius[1].abs() * image_height.max(1) as f32).max(1.0);
    let inner = (1.0 - feather.clamp(0.0, 1.0) * 0.98).clamp(0.0, 0.995);

    for y in 0..height {
        let v = (y as f32 + 0.5) / height as f32;
        let dy = (v - center[1]) * image_height.max(1) as f32;
        for x in 0..width {
            let u = (x as f32 + 0.5) / width as f32;
            let dx = (u - center[0]) * image_width.max(1) as f32;
            let local_x = cos_r * dx + sin_r * dy;
            let local_y = -sin_r * dx + cos_r * dy;
            let distance = ((local_x / rx).powi(2) + (local_y / ry).powi(2)).sqrt();
            out[y as usize * width as usize + x as usize] = 1.0 - smoothstep(inner, 1.0, distance);
        }
    }
    out
}

fn rasterize_linear(
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
    start: [f32; 2],
    end: [f32; 2],
    feather: f32,
) -> Vec<f32> {
    let mut out = vec![0.0f32; width as usize * height as usize];
    let sx = start[0] * image_width.max(1) as f32;
    let sy = start[1] * image_height.max(1) as f32;
    let dx = (end[0] - start[0]) * image_width.max(1) as f32;
    let dy = (end[1] - start[1]) * image_height.max(1) as f32;
    let length_sq = (dx * dx + dy * dy).max(1.0);
    let width_factor = feather.clamp(0.02, 1.0);
    let edge0 = 0.5 - 0.5 * width_factor;
    let edge1 = 0.5 + 0.5 * width_factor;

    for y in 0..height {
        let py = (y as f32 + 0.5) / height as f32 * image_height.max(1) as f32;
        for x in 0..width {
            let px = (x as f32 + 0.5) / width as f32 * image_width.max(1) as f32;
            let t = ((px - sx) * dx + (py - sy) * dy) / length_sq;
            out[y as usize * width as usize + x as usize] = 1.0 - smoothstep(edge0, edge1, t);
        }
    }
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
mod tests {
    use super::*;

    #[test]
    fn new_brush_is_selected_and_paint_ready() {
        let mut stack = MaskStack::default();
        assert_eq!(stack.add_mask(MaskKind::Brush), Some((0, 0)));
        assert_eq!(stack.selected_mask, Some(0));
        assert_eq!(stack.selected_component, Some(0));
        assert!(matches!(
            stack.selected_component().unwrap().geometry,
            MaskGeometry::Brush { .. }
        ));
    }

    #[test]
    fn radial_layer_has_soft_center_and_clear_corners() {
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Radial);
        if let MaskGeometry::Radial { initialized, .. } =
            &mut stack.selected_component_mut().unwrap().geometry
        {
            *initialized = true;
        }
        let layer = stack.rasterize_layer(0, 64, 64, 100, 100);
        assert!(layer[32 * 64 + 32] > 240);
        assert!(layer[0] < 8);
    }

    #[test]
    fn centered_brush_is_symmetric_on_even_atlas() {
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Brush);
        if let MaskGeometry::Brush { dabs, .. } =
            &mut stack.selected_component_mut().unwrap().geometry
        {
            dabs.push(BrushDab {
                center: [0.5, 0.5],
                size: 0.2,
                feather: 0.5,
                opacity: 1.0,
            });
        }
        let layer = stack.rasterize_layer(0, 32, 32, 100, 100);
        assert_eq!(layer[15 * 32 + 15], layer[15 * 32 + 16]);
        assert_eq!(layer[16 * 32 + 15], layer[16 * 32 + 16]);
    }

    #[test]
    fn brush_eraser_removes_existing_coverage() {
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Brush);
        if let MaskGeometry::Brush { dabs, .. } =
            &mut stack.selected_component_mut().unwrap().geometry
        {
            dabs.push(BrushDab {
                center: [0.5, 0.5],
                size: 0.25,
                feather: 0.2,
                opacity: 1.0,
            });
            dabs.push(BrushDab {
                center: [0.5, 0.5],
                size: 0.1,
                feather: 0.2,
                opacity: -1.0,
            });
        }
        let layer = stack.rasterize_layer(0, 64, 64, 100, 100);
        assert!(layer[32 * 64 + 32] < 8);
        assert!(layer[32 * 64 + 40] > 200);
    }

    #[test]
    fn reordering_tracks_selected_mask_and_component() {
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Brush);
        stack.add_mask(MaskKind::Radial);
        stack.add_mask(MaskKind::Linear);
        assert!(stack.move_mask(2, 0));
        assert_eq!(stack.selected_mask, Some(0));
        assert_eq!(stack.masks[0].components[0].kind, MaskKind::Linear);

        stack.add_component(MaskKind::Brush, MaskCombineMode::Subtract);
        assert!(stack.move_component(1, 0));
        assert_eq!(stack.selected_component, Some(0));
        assert_eq!(stack.masks[0].components[0].kind, MaskKind::Brush);
    }

    #[test]
    fn background_reuses_and_inverts_subject_probability() {
        let subject = MaskImage::new(2, 1, vec![0, 255]).unwrap();
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Subject);
        if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
        {
            *mask = Some(subject.clone());
        }
        stack.add_mask(MaskKind::Background);
        if let MaskGeometry::Ai { mask, .. } = &mut stack.selected_component_mut().unwrap().geometry
        {
            *mask = Some(subject);
        }
        let foreground = stack.rasterize_layer(0, 2, 1, 2, 1);
        let background = stack.rasterize_layer(1, 2, 1, 2, 1);
        assert_eq!(foreground, vec![0, 255]);
        assert_eq!(background, vec![255, 0]);
    }

    #[test]
    fn luminance_and_color_ranges_use_the_cached_preview() {
        let source = MaskRgbImage::new(2, 1, vec![0, 0, 0, 255, 255, 0, 0, 255]).unwrap();
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::LuminanceRange);
        if let MaskGeometry::LuminanceRange {
            source: target,
            low,
            high,
            ..
        } = &mut stack.selected_component_mut().unwrap().geometry
        {
            *target = Some(source.clone());
            *low = 0.1;
            *high = 0.4;
        }
        let luminance = stack.rasterize_layer(0, 2, 1, 2, 1);
        assert!(luminance[0] < 8);
        assert!(luminance[1] > 240);

        stack.add_mask(MaskKind::ColorRange);
        if let MaskGeometry::ColorRange {
            source: target,
            sample,
            tolerance,
            sampled,
            ..
        } = &mut stack.selected_component_mut().unwrap().geometry
        {
            *target = Some(source);
            *sample = [1.0, 0.0, 0.0];
            *tolerance = 0.1;
            *sampled = true;
        }
        let color = stack.rasterize_layer(1, 2, 1, 2, 1);
        assert!(color[0] < 8);
        assert!(color[1] > 240);
    }

    #[test]
    fn subtract_component_removes_coverage() {
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Radial);
        if let MaskGeometry::Radial { initialized, .. } =
            &mut stack.selected_component_mut().unwrap().geometry
        {
            *initialized = true;
        }
        stack.add_component(MaskKind::Brush, MaskCombineMode::Subtract);
        if let MaskGeometry::Brush { dabs, .. } =
            &mut stack.selected_component_mut().unwrap().geometry
        {
            dabs.push(BrushDab::default());
        }
        let layer = stack.rasterize_layer(0, 64, 64, 100, 100);
        assert!(layer[32 * 64 + 32] < 32);
    }
}
