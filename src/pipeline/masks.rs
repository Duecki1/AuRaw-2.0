use rayon::prelude::*;
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
            Self::Background => "Select Not Subject",
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
    pub name: String,
    pub kind: MaskKind,
    pub combine: MaskCombineMode,
    pub enabled: bool,
    pub invert: bool,
    pub geometry: MaskGeometry,
}

impl MaskComponent {
    pub fn new(kind: MaskKind, combine: MaskCombineMode) -> Self {
        Self {
            name: kind.label().to_owned(),
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
                    MaskGeometry::Ai { mask, .. } => {
                        *mask = mask
                            .as_ref()
                            .map(|source| crop_mask_image(source, u0, v0, du, dv));
                    }
                    MaskGeometry::LuminanceRange { source, .. }
                    | MaskGeometry::ColorRange { source, .. } => {
                        *source = source
                            .as_ref()
                            .map(|source| crop_rgb_image(source, u0, v0, du, dv));
                    }
                    MaskGeometry::Placeholder => {}
                }
            }
        }
        cropped
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
            );
            if component.invert {
                for value in &mut coverage {
                    *value = 1.0 - *value;
                }
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
                    for (dst, src) in existing.iter_mut().zip(coverage) {
                        *dst = dst.max(src);
                    }
                }
                MaskCombineMode::Subtract => {
                    for (dst, src) in existing.iter_mut().zip(coverage) {
                        *dst *= 1.0 - src;
                    }
                }
                MaskCombineMode::Intersect => {
                    for (dst, src) in existing.iter_mut().zip(coverage) {
                        *dst *= src;
                    }
                }
            }
        }

        let Some(combined) = combined else {
            return vec![0; len];
        };
        let opacity = mask.opacity.clamp(0.0, 1.0);
        combined
            .into_par_iter()
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

fn crop_mask_image(source: &MaskImage, u0: f32, v0: f32, du: f32, dv: f32) -> MaskImage {
    if source.width == 0 || source.height == 0 {
        return source.clone();
    }
    let x0 = (u0 * source.width as f32)
        .floor()
        .clamp(0.0, source.width.saturating_sub(1) as f32) as u32;
    let y0 = (v0 * source.height as f32)
        .floor()
        .clamp(0.0, source.height.saturating_sub(1) as f32) as u32;
    let x1 = ((u0 + du) * source.width as f32)
        .ceil()
        .clamp((x0 + 1) as f32, source.width as f32) as u32;
    let y1 = ((v0 + dv) * source.height as f32)
        .ceil()
        .clamp((y0 + 1) as f32, source.height as f32) as u32;
    let width = x1 - x0;
    let height = y1 - y0;
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for row in y0..y1 {
        let start = (row * source.width + x0) as usize;
        pixels.extend_from_slice(&source.pixels[start..start + width as usize]);
    }
    MaskImage::new(width, height, pixels).expect("cropped mask image dimensions are valid")
}

fn crop_rgb_image(source: &MaskRgbImage, u0: f32, v0: f32, du: f32, dv: f32) -> MaskRgbImage {
    if source.width == 0 || source.height == 0 {
        return source.clone();
    }
    let x0 = (u0 * source.width as f32)
        .floor()
        .clamp(0.0, source.width.saturating_sub(1) as f32) as u32;
    let y0 = (v0 * source.height as f32)
        .floor()
        .clamp(0.0, source.height.saturating_sub(1) as f32) as u32;
    let x1 = ((u0 + du) * source.width as f32)
        .ceil()
        .clamp((x0 + 1) as f32, source.width as f32) as u32;
    let y1 = ((v0 + dv) * source.height as f32)
        .ceil()
        .clamp((y0 + 1) as f32, source.height as f32) as u32;
    let width = x1 - x0;
    let height = y1 - y0;
    let mut rgba = Vec::with_capacity((width * height * 4) as usize);
    for row in y0..y1 {
        let start = ((row * source.width + x0) * 4) as usize;
        rgba.extend_from_slice(&source.rgba[start..start + width as usize * 4]);
    }
    MaskRgbImage::new(width, height, rgba).expect("cropped RGB mask dimensions are valid")
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
            // Feather the subject boundary once, then complement it for Not
            // Subject. This keeps the two AI masks exact opposites at every
            // feather value instead of separately blurring an inverted map.
            feather_probability_mask(&mut coverage, width, height, *feather);
            if component.kind == MaskKind::Background {
                coverage.par_iter_mut().for_each(|value| *value = 1.0 - *value);
            }
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
    if width == 0 || height == 0 || mask.width == 0 || mask.height == 0 {
        return vec![0.0; width as usize * height as usize];
    }
    let row_stride = width as usize;
    let mut out = vec![0.0; row_stride * height as usize];
    out.par_chunks_mut(row_stride)
        .enumerate()
        .for_each(|(y, row)| {
            let source_y = ((y as f32 + 0.5) * mask.height as f32 / height as f32 - 0.5)
                .clamp(0.0, mask.height.saturating_sub(1) as f32);
            let y0 = source_y.floor() as usize;
            let y1 = (y0 + 1).min(mask.height as usize - 1);
            let fy = source_y - y0 as f32;
            for (x, value) in row.iter_mut().enumerate() {
                let source_x = ((x as f32 + 0.5) * mask.width as f32 / width as f32 - 0.5)
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

fn feather_probability_mask(mask: &mut [f32], width: u32, height: u32, feather: f32) {
    if width == 0 || height == 0 || mask.is_empty() {
        return;
    }
    let feather = feather.clamp(0.0, 1.0);
    if feather <= 1e-5 {
        return;
    }

    // Feather is an image-relative boundary width, so thumbnails, the preview
    // overlay, the 2048px mask atlas, and export all show the same shape. The old
    // fixed 32-texel box blur changed apparent feathering with every resolution
    // and washed probability across the entire subject.
    let radius = (feather.powf(1.30) * width.min(height) as f32 * 0.045).max(0.75);
    let width = width as usize;
    let height = height as usize;
    let binary = mask
        .iter()
        .map(|value| u8::from(*value >= 0.5))
        .collect::<Vec<_>>();
    let distance_to_inside = chamfer_distance(&binary, width, height, 1);
    let distance_to_outside = chamfer_distance(&binary, width, height, 0);

    mask.par_iter_mut().enumerate().for_each(|(index, value)| {
        let confidence_offset = (*value - 0.5) * 1.5;
        let signed_distance = distance_to_outside[index] - distance_to_inside[index]
            + confidence_offset;
        *value = smoothstep(-radius, radius, signed_distance);
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

fn rasterize_brush(
    width: u32,
    height: u32,
    image_width: u32,
    image_height: u32,
    dabs: &[BrushDab],
) -> Vec<f32> {
    if width == 0 || height == 0 || dabs.is_empty() {
        return vec![0.0; width as usize * height as usize];
    }

    let image_min = image_width.min(image_height).max(1) as f32;
    let mut specs = Vec::with_capacity(dabs.len());
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
                        let coverage =
                            1.0 - smoothstep(spec.inner, 1.0 + spec.antialias, distance);
                        let index = row_offset + x as usize;
                        if spec.opacity >= 0.0 {
                            band[index] =
                                band[index].max(coverage * spec.opacity.clamp(0.0, 1.0));
                        } else {
                            band[index] *=
                                1.0 - coverage * (-spec.opacity).clamp(0.0, 1.0);
                        }
                    }
                }
            }
        });
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
    fn cropped_mask_remaps_geometry_to_the_visible_region() {
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Radial);
        if let MaskGeometry::Radial {
            center,
            radius,
            initialized,
            ..
        } = &mut stack.selected_component_mut().unwrap().geometry
        {
            *center = [0.75, 0.5];
            *radius = [0.1, 0.2];
            *initialized = true;
        }

        let cropped = stack.cropped_for_region(50, 0, 50, 100, 100, 100);
        let MaskGeometry::Radial { center, radius, .. } =
            &cropped.selected_component().unwrap().geometry
        else {
            panic!("expected radial mask");
        };
        assert!((center[0] - 0.5).abs() < 1e-6);
        assert!((center[1] - 0.5).abs() < 1e-6);
        assert!((radius[0] - 0.2).abs() < 1e-6);
        assert!((radius[1] - 0.2).abs() < 1e-6);
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
    fn feathered_background_is_the_exact_subject_complement() {
        let mut pixels = vec![0u8; 8 * 8];
        for y in 2..6 {
            for x in 2..6 {
                pixels[y * 8 + x] = 255;
            }
        }
        let subject = MaskImage::new(8, 8, pixels).unwrap();
        let mut stack = MaskStack::default();
        stack.add_mask(MaskKind::Subject);
        if let MaskGeometry::Ai { mask, feather } =
            &mut stack.selected_component_mut().unwrap().geometry
        {
            *mask = Some(subject.clone());
            *feather = 0.65;
        }
        stack.add_mask(MaskKind::Background);
        if let MaskGeometry::Ai { mask, feather } =
            &mut stack.selected_component_mut().unwrap().geometry
        {
            *mask = Some(subject);
            *feather = 0.65;
        }

        let foreground = stack.rasterize_layer(0, 96, 64, 800, 533);
        let background = stack.rasterize_layer(1, 96, 64, 800, 533);
        assert!(foreground
            .iter()
            .zip(background.iter())
            .all(|(subject, not_subject)| *subject as u16 + *not_subject as u16 == 255));
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
