use crate::file_ops::{replace_file, sync_parent_directory};
use crate::pipeline::remove::RemovePatchSidecarCache;
use crate::pipeline::{
    ExposureParams, GeometryTransform, MaskGeometry, MaskImage, MaskKind, MaskStack,
    RemoveEditState, SubjectRefinement, MAX_LOCAL_MASKS, MAX_MASK_COMPONENTS,
    REMOVE_MAX_PATCHES_PER_STROKE, REMOVE_MAX_STROKES,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::io::{Cursor, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

pub const SIDECAR_SCHEMA_VERSION: u32 = 16;
pub const DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT: u64 = 0x4155_5241_5700_0007;
pub const SIDECAR_SUFFIX: &str = ".auraw";
#[cfg(not(target_os = "android"))]
pub const DEVELOPED_THUMBNAIL_SUFFIX: &str = ".auraw-thumb.jpg";
#[cfg(any(not(target_os = "android"), test))]
pub const DEVELOPED_THUMBNAIL_CACHE_DIR: &str = crate::thumbnail_cache::DESKTOP_THUMBNAIL_CACHE_DIR;
#[cfg(not(target_os = "android"))]
const DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX: &str = ".auraw-thumb.fingerprint";
pub const MAX_SIDECAR_BYTES: u64 = if cfg!(target_os = "android") {
    128 * 1024 * 1024
} else {
    256 * 1024 * 1024
};

const SIDECAR_FORMAT: &str = "AuRaw edit sidecar";
const MAX_BRUSH_DABS: usize = 1_000_000;
const MAX_OBJECT_STROKES: usize = 4096;
const MAX_OBJECT_STROKE_POINTS: usize = 1_000_000;
const MAX_MASK_IMAGE_EDGE: u32 = 8192;
const MAX_MASK_ASSET_REFS: usize = MAX_LOCAL_MASKS * MAX_MASK_COMPONENTS;
const MAX_REMOVE_ASSET_REFS: usize = REMOVE_MAX_STROKES * REMOVE_MAX_PATCHES_PER_STROKE;
const MAX_DECODED_MASK_ASSET_BYTES: u64 = if cfg!(target_os = "android") {
    256 * 1024 * 1024
} else {
    512 * 1024 * 1024
};
const MAX_DECODED_REMOVE_ASSET_BYTES: u64 = if cfg!(target_os = "android") {
    256 * 1024 * 1024
} else {
    512 * 1024 * 1024
};
const MAX_EDIT_NAME_BYTES: usize = 4096;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SidecarTarget {
    Desktop {
        raw_path: PathBuf,
    },
    #[cfg(target_os = "android")]
    Android {
        raw_uri: String,
        display_name: String,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct LensEditState {
    pub enabled: bool,
    pub maker: String,
    pub model: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct AdjustmentCopySettings {
    #[serde(default = "default_true")]
    pub adjustments: bool,
    #[serde(default)]
    pub geometry: bool,
    #[serde(default = "default_true")]
    pub camera_profile: bool,
    #[serde(default = "default_true")]
    pub masks: bool,
    #[serde(default = "default_true")]
    pub ai_masks: bool,
    #[serde(default)]
    pub lens_correction: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AdjustmentPasteMode {
    #[default]
    Merge,
    Replace,
}

const fn default_true() -> bool {
    true
}

impl Default for AdjustmentCopySettings {
    fn default() -> Self {
        Self {
            adjustments: true,
            geometry: false,
            camera_profile: true,
            masks: true,
            ai_masks: true,
            lens_correction: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct EditState {
    pub exposure: ExposureParams,
    #[serde(default)]
    pub geometry: GeometryTransform,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_profile: Option<PathBuf>,
    pub masks: Arc<MaskStack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_refinement: Option<SubjectRefinement>,
    pub lens: LensEditState,
    #[serde(default, skip_serializing_if = "arc_remove_is_empty")]
    pub remove: Arc<RemoveEditState>,
    #[serde(default)]
    pub ai_masks_need_update: bool,
}

fn arc_remove_is_empty(remove: &Arc<RemoveEditState>) -> bool {
    remove.is_empty()
}

pub fn default_edit_state() -> EditState {
    EditState {
        exposure: ExposureParams::scene_referred_default(),
        geometry: GeometryTransform::default(),
        camera_profile: None,
        masks: Arc::new(MaskStack::default()),
        subject_refinement: None,
        lens: LensEditState::default(),
        remove: Arc::new(RemoveEditState::default()),
        ai_masks_need_update: false,
    }
}

fn is_manual_mask_kind(kind: MaskKind) -> bool {
    matches!(
        kind,
        MaskKind::Brush | MaskKind::Fullscreen | MaskKind::Radial | MaskKind::Linear
    )
}

fn filtered_mask_stack(masks: &MaskStack, include_manual: bool, include_ai: bool) -> MaskStack {
    if include_manual && include_ai {
        return masks.clone();
    }

    MaskStack {
        masks: masks
            .masks
            .iter()
            .filter_map(|mask| {
                let mut selected = mask.clone();
                selected.components.retain(|component| {
                    if is_manual_mask_kind(component.kind) {
                        include_manual
                    } else {
                        include_ai
                    }
                });
                (!selected.components.is_empty()).then_some(selected)
            })
            .collect(),
        subject_refinement: if include_ai {
            masks.subject_refinement.clone()
        } else {
            Default::default()
        },
        ..Default::default()
    }
}

fn replace_selected_mask_categories(
    destination: &mut MaskStack,
    source: &MaskStack,
    include_manual: bool,
    include_ai: bool,
) {
    if include_manual && include_ai {
        *destination = source.clone();
        return;
    }

    let mut merged = filtered_mask_stack(destination, !include_manual, !include_ai);
    let copied = filtered_mask_stack(source, include_manual, include_ai);
    if include_ai {
        merged.subject_refinement = copied.subject_refinement.clone();
    }
    merged.masks.extend(copied.masks);
    *destination = merged;
}

fn masks_contain_content_aware_components(masks: &MaskStack) -> bool {
    masks.masks.iter().any(|mask| {
        mask.components
            .iter()
            .any(|component| match (component.kind, &component.geometry) {
                (MaskKind::Subject | MaskKind::Background, MaskGeometry::Ai { .. }) => true,
                (MaskKind::Object, MaskGeometry::Object { strokes, .. }) => strokes
                    .iter()
                    .any(|stroke| stroke.positive && !stroke.points.is_empty()),
                (MaskKind::LuminanceRange, MaskGeometry::LuminanceRange { .. }) => true,
                (MaskKind::ColorRange, MaskGeometry::ColorRange { sampled: true, .. }) => true,
                _ => false,
            })
    })
}

pub fn edit_state_has_adjustments(edits: &EditState) -> bool {
    let default = default_edit_state();
    edits.exposure != default.exposure
        || edits.geometry != default.geometry
        || edits.camera_profile != default.camera_profile
        || edits.masks != default.masks
        || edits.subject_refinement != default.subject_refinement
        || edits.lens != default.lens
        || edits.remove != default.remove
}

pub fn apply_copied_adjustments(
    destination: &mut EditState,
    source: &EditState,
    settings: AdjustmentCopySettings,
) {
    apply_copied_adjustments_with_mode(destination, source, settings, AdjustmentPasteMode::Merge);
}

pub fn apply_copied_adjustments_with_mode(
    destination: &mut EditState,
    source: &EditState,
    settings: AdjustmentCopySettings,
    mode: AdjustmentPasteMode,
) {
    if mode == AdjustmentPasteMode::Replace {
        let remove = Arc::clone(&destination.remove);
        *destination = default_edit_state();
        destination.remove = remove;
    }
    if settings.adjustments {
        destination.exposure = source.exposure;
    }
    if settings.geometry {
        destination.geometry = source.geometry;
    }
    if settings.camera_profile {
        let camera_profile_changed = destination.camera_profile != source.camera_profile;
        destination.camera_profile = source.camera_profile.clone();
        if camera_profile_changed && masks_contain_content_aware_components(&destination.masks) {
            destination.ai_masks_need_update = true;
        }
    }
    if settings.masks || settings.ai_masks {
        let previous_ai_masks_need_update = destination.ai_masks_need_update;
        let previous_subject_refinement = destination.subject_refinement.clone();
        let mut masks = destination.masks.as_ref().clone();
        replace_selected_mask_categories(
            &mut masks,
            &source.masks,
            settings.masks,
            settings.ai_masks,
        );
        destination.masks = Arc::new(masks);
        destination.subject_refinement = if settings.ai_masks {
            source.subject_refinement.clone().or_else(|| {
                (!source.masks.subject_refinement.is_empty())
                    .then(|| source.masks.subject_refinement.clone())
            })
        } else {
            previous_subject_refinement
        };
        destination.ai_masks_need_update = if settings.ai_masks {
            source.ai_masks_need_update
                || masks_contain_content_aware_components(&destination.masks)
        } else {
            previous_ai_masks_need_update
        };
    }
    if settings.lens_correction {
        let lens_changed = destination.lens != source.lens;
        destination.lens = source.lens.clone();
        if lens_changed && masks_contain_content_aware_components(&destination.masks) {
            destination.ai_masks_need_update = true;
        }
    }
}

fn synchronize_subject_refinement(edits: &mut EditState) {
    let refinement = edits.subject_refinement.clone().or_else(|| {
        (!edits.masks.subject_refinement.is_empty()).then(|| edits.masks.subject_refinement.clone())
    });
    let refinement = refinement.filter(|refinement| !refinement.is_empty());
    Arc::make_mut(&mut edits.masks).subject_refinement = refinement.clone().unwrap_or_default();
    edits.subject_refinement = refinement;
}

fn migrate_legacy_retouch_opacity(edits: &mut EditState) {
    for stroke in &mut Arc::make_mut(&mut edits.remove).strokes {
        if let Some(retouch) = &mut stroke.retouch {
            let opacity = retouch.opacity.clamp(0.0, 1.0);
            stroke.opacity = opacity;
            retouch.baked_opacity = Some(opacity);
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct SidecarDocument {
    format: String,
    schema_version: u32,
    edits: EditState,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mask_assets: Vec<SidecarMaskAsset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    mask_asset_refs: Vec<SidecarMaskAssetRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remove_assets: Vec<SidecarRemoveAsset>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remove_asset_refs: Vec<SidecarRemoveAssetRef>,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct SidecarMaskAsset {
    width: u32,
    height: u32,
    #[serde(with = "base64_arc_bytes")]
    png: Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct SidecarMaskAssetRef {
    mask_index: usize,
    component_index: usize,
    asset_index: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SidecarRemoveEncoding {
    Scene16f,
    Srgb16,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
struct SidecarRemoveAsset {
    width: u32,
    height: u32,
    encoding: SidecarRemoveEncoding,
    #[serde(with = "base64_arc_bytes")]
    rgb_png: Arc<[u8]>,
    #[serde(with = "base64_arc_bytes")]
    alpha_png: Arc<[u8]>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
struct SidecarRemoveAssetRef {
    stroke_index: usize,
    patch_index: usize,
    asset_index: usize,
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

fn generated_mask(geometry: &MaskGeometry) -> Option<&Option<MaskImage>> {
    match geometry {
        MaskGeometry::Ai { mask, .. } | MaskGeometry::Object { mask, .. } => Some(mask),
        _ => None,
    }
}

fn generated_mask_mut(geometry: &mut MaskGeometry) -> Option<&mut Option<MaskImage>> {
    match geometry {
        MaskGeometry::Ai { mask, .. } | MaskGeometry::Object { mask, .. } => Some(mask),
        _ => None,
    }
}

fn mask_image_fingerprint(image: &MaskImage) -> u64 {
    let mut hasher = DefaultHasher::new();
    image.width.hash(&mut hasher);
    image.height.hash(&mut hasher);
    image.pixels.hash(&mut hasher);
    hasher.finish()
}

fn encode_mask_png(image: &MaskImage) -> Result<Arc<[u8]>, SidecarError> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, image.width, image.height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::Balanced);
        let mut writer = encoder.write_header().map_err(|error| {
            SidecarError::Invalid(format!("could not start mask PNG compression: {error}"))
        })?;
        writer.write_image_data(&image.pixels).map_err(|error| {
            SidecarError::Invalid(format!("could not compress generated mask: {error}"))
        })?;
    }
    Ok(encoded.into())
}

fn extract_mask_assets(
    edits: &mut EditState,
) -> Result<(Vec<SidecarMaskAsset>, Vec<SidecarMaskAssetRef>), SidecarError> {
    let mut assets = Vec::<SidecarMaskAsset>::new();
    let mut unique_images = Vec::<MaskImage>::new();
    let mut buckets = HashMap::<u64, Vec<usize>>::new();
    let mut references = Vec::new();
    let mut decoded_asset_bytes = 0u64;
    let mut encoded_asset_bytes = 0u64;

    for (mask_index, mask) in Arc::make_mut(&mut edits.masks).masks.iter_mut().enumerate() {
        for (component_index, component) in mask.components.iter_mut().enumerate() {
            let Some(image) = generated_mask_mut(&mut component.geometry).and_then(Option::take)
            else {
                continue;
            };
            let fingerprint = mask_image_fingerprint(&image);
            let existing = buckets.get(&fingerprint).and_then(|candidates| {
                candidates
                    .iter()
                    .copied()
                    .find(|index| unique_images[*index] == image)
            });
            let asset_index = if let Some(index) = existing {
                index
            } else {
                let pixels = u64::try_from(image.pixels.len())
                    .map_err(|_| SidecarError::TooLarge(u64::MAX))?;
                decoded_asset_bytes = decoded_asset_bytes
                    .checked_add(pixels)
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
                if decoded_asset_bytes > MAX_DECODED_MASK_ASSET_BYTES {
                    return invalid("generated masks exceed the decoded asset memory safety limit");
                }
                let png = encode_mask_png(&image)?;
                encoded_asset_bytes = encoded_asset_bytes
                    .checked_add(base64_json_string_bytes(png.len())?)
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
                if encoded_asset_bytes > MAX_SIDECAR_BYTES {
                    return Err(SidecarError::TooLarge(encoded_asset_bytes));
                }
                let index = assets.len();
                assets.push(SidecarMaskAsset {
                    width: image.width,
                    height: image.height,
                    png,
                });
                unique_images.push(image);
                buckets.entry(fingerprint).or_default().push(index);
                index
            };
            references.push(SidecarMaskAssetRef {
                mask_index,
                component_index,
                asset_index,
            });
            if references.len() > MAX_MASK_ASSET_REFS {
                return invalid("edit contains too many generated mask references");
            }
        }
    }

    Ok((assets, references))
}

fn decode_mask_png(asset: &SidecarMaskAsset) -> Result<MaskImage, SidecarError> {
    let decoder = png::Decoder::new(Cursor::new(asset.png.as_ref()));
    let mut reader = decoder.read_info().map_err(|error| {
        SidecarError::Invalid(format!("could not read compressed mask PNG: {error}"))
    })?;
    let info = reader.info();
    if info.width != asset.width
        || info.height != asset.height
        || info.color_type != png::ColorType::Grayscale
        || info.bit_depth != png::BitDepth::Eight
        || info.animation_control.is_some()
    {
        return invalid("compressed mask PNG metadata does not match its asset");
    }

    let expected = usize::try_from(asset.width)
        .ok()
        .and_then(|width| {
            usize::try_from(asset.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .ok_or(SidecarError::TooLarge(u64::MAX))?;
    let output_size = reader
        .output_buffer_size()
        .ok_or(SidecarError::TooLarge(u64::MAX))?;
    if output_size != expected {
        return invalid("compressed mask PNG does not contain one grayscale byte per pixel");
    }
    let mut pixels = Vec::new();
    pixels
        .try_reserve_exact(expected)
        .map_err(|_| SidecarError::TooLarge(expected as u64))?;
    pixels.resize(expected, 0);
    let output = reader.next_frame(&mut pixels).map_err(|error| {
        SidecarError::Invalid(format!("could not decompress generated mask: {error}"))
    })?;
    if output.width != asset.width
        || output.height != asset.height
        || output.color_type != png::ColorType::Grayscale
        || output.bit_depth != png::BitDepth::Eight
        || output.buffer_size() != expected
    {
        return invalid("decompressed mask PNG dimensions or format are invalid");
    }
    MaskImage::new(asset.width, asset.height, pixels)
        .ok_or_else(|| SidecarError::Invalid("decompressed mask pixels are invalid".to_owned()))
}

fn restore_mask_assets(
    edits: &mut EditState,
    assets: &[SidecarMaskAsset],
    references: &[SidecarMaskAssetRef],
) -> Result<(), SidecarError> {
    if assets.len() > MAX_MASK_ASSET_REFS || references.len() > MAX_MASK_ASSET_REFS {
        return invalid("sidecar contains too many generated mask assets");
    }

    for mask in &edits.masks.masks {
        for component in &mask.components {
            if generated_mask(&component.geometry).is_some_and(Option::is_some) {
                return invalid("current sidecar schema contains a legacy inline generated mask");
            }
        }
    }

    let mut decoded_bytes = 0u64;
    for asset in assets {
        let pixels = u64::from(asset.width)
            .checked_mul(u64::from(asset.height))
            .ok_or(SidecarError::TooLarge(u64::MAX))?;
        let pixels_usize = usize::try_from(pixels).map_err(|_| SidecarError::TooLarge(pixels))?;
        validate_image(asset.width, asset.height, pixels_usize, 1)?;
        decoded_bytes = decoded_bytes
            .checked_add(pixels)
            .ok_or(SidecarError::TooLarge(u64::MAX))?;
        if decoded_bytes > MAX_DECODED_MASK_ASSET_BYTES {
            return invalid("compressed mask assets exceed the decoded memory safety limit");
        }
    }

    let mut locations = HashSet::new();
    let mut referenced_assets = vec![false; assets.len()];
    for reference in references {
        let Some(asset_referenced) = referenced_assets.get_mut(reference.asset_index) else {
            return invalid("generated mask reference uses an invalid asset index");
        };
        let Some(component) = edits
            .masks
            .masks
            .get(reference.mask_index)
            .and_then(|mask| mask.components.get(reference.component_index))
        else {
            return invalid("generated mask reference uses an invalid component index");
        };
        if generated_mask(&component.geometry).is_none() {
            return invalid("generated mask reference targets an incompatible component");
        }
        if !locations.insert((reference.mask_index, reference.component_index)) {
            return invalid("sidecar contains duplicate references for a generated mask");
        }
        *asset_referenced = true;
    }
    if referenced_assets.iter().any(|referenced| !referenced) {
        return invalid("sidecar contains an unreferenced generated mask asset");
    }

    let decoded = assets
        .iter()
        .map(decode_mask_png)
        .collect::<Result<Vec<_>, _>>()?;
    let masks = &mut Arc::make_mut(&mut edits.masks).masks;
    for reference in references {
        let component = &mut masks[reference.mask_index].components[reference.component_index];
        let slot = generated_mask_mut(&mut component.geometry)
            .ok_or_else(|| SidecarError::Invalid("generated mask target disappeared".to_owned()))?;
        *slot = Some(decoded[reference.asset_index].clone());
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
struct RemoveAssetPayload {
    width: u32,
    height: u32,
    encoding: SidecarRemoveEncoding,
    rgb: Arc<[u16]>,
    alpha: Arc<[u8]>,
}

fn remove_payload_fingerprint(payload: &RemoveAssetPayload) -> u64 {
    let mut hasher = DefaultHasher::new();
    payload.width.hash(&mut hasher);
    payload.height.hash(&mut hasher);
    payload.encoding.hash(&mut hasher);
    payload.rgb.hash(&mut hasher);
    payload.alpha.hash(&mut hasher);
    hasher.finish()
}

fn encode_remove_rgb_png(payload: &RemoveAssetPayload) -> Result<Arc<[u8]>, SidecarError> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, payload.width, payload.height);
        encoder.set_color(png::ColorType::Rgb);
        encoder.set_depth(png::BitDepth::Sixteen);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().map_err(|error| {
            SidecarError::Invalid(format!("could not start retouch RGB compression: {error}"))
        })?;
        {
            let mut stream = writer.stream_writer_with_size(64 * 1024).map_err(|error| {
                SidecarError::Invalid(format!("could not stream retouch RGB compression: {error}"))
            })?;
            let values_per_row = payload.width as usize * 3;
            let mut row = Vec::new();
            row.try_reserve_exact(values_per_row.saturating_mul(2))
                .map_err(|_| SidecarError::TooLarge(values_per_row.saturating_mul(2) as u64))?;
            for values in payload.rgb.chunks_exact(values_per_row) {
                row.clear();
                for value in values {
                    row.extend_from_slice(&value.to_be_bytes());
                }
                stream.write_all(&row).map_err(|error| {
                    SidecarError::Invalid(format!("could not compress retouch RGB: {error}"))
                })?;
            }
            stream.finish().map_err(|error| {
                SidecarError::Invalid(format!("could not finish retouch RGB compression: {error}"))
            })?;
        }
        writer.finish().map_err(|error| {
            SidecarError::Invalid(format!("could not finalize retouch RGB PNG: {error}"))
        })?;
    }
    Ok(encoded.into())
}

fn encode_remove_alpha_png(payload: &RemoveAssetPayload) -> Result<Arc<[u8]>, SidecarError> {
    let mut encoded = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut encoded, payload.width, payload.height);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        encoder.set_compression(png::Compression::High);
        let mut writer = encoder.write_header().map_err(|error| {
            SidecarError::Invalid(format!(
                "could not start retouch alpha compression: {error}"
            ))
        })?;
        writer.write_image_data(&payload.alpha).map_err(|error| {
            SidecarError::Invalid(format!("could not compress retouch alpha: {error}"))
        })?;
    }
    Ok(encoded.into())
}

fn compressed_remove_payload(
    payload: &RemoveAssetPayload,
    fingerprint: u64,
    cache: &Arc<std::sync::OnceLock<RemovePatchSidecarCache>>,
) -> Result<RemovePatchSidecarCache, SidecarError> {
    if let Some(cached) = cache
        .get()
        .filter(|cached| cached.fingerprint == fingerprint)
    {
        return Ok(cached.clone());
    }
    let compressed = RemovePatchSidecarCache {
        fingerprint,
        rgb_png: encode_remove_rgb_png(payload)?,
        alpha_png: encode_remove_alpha_png(payload)?,
    };
    let _ = cache.set(compressed.clone());
    Ok(cache
        .get()
        .filter(|cached| cached.fingerprint == fingerprint)
        .cloned()
        .unwrap_or(compressed))
}

fn extract_remove_assets(
    edits: &mut EditState,
) -> Result<(Vec<SidecarRemoveAsset>, Vec<SidecarRemoveAssetRef>), SidecarError> {
    let mut assets = Vec::<SidecarRemoveAsset>::new();
    let mut unique_payloads = Vec::<RemoveAssetPayload>::new();
    let mut buckets = HashMap::<u64, Vec<usize>>::new();
    let mut references = Vec::new();
    let mut decoded_asset_bytes = 0u64;
    let mut encoded_asset_bytes = 0u64;

    for (stroke_index, stroke) in Arc::make_mut(&mut edits.remove)
        .strokes
        .iter_mut()
        .enumerate()
    {
        for (patch_index, patch) in stroke.patches.iter_mut().enumerate() {
            let (encoding, rgb) = if !patch.rgb_scene16f.is_empty() {
                (
                    SidecarRemoveEncoding::Scene16f,
                    Arc::clone(&patch.rgb_scene16f),
                )
            } else {
                (SidecarRemoveEncoding::Srgb16, Arc::clone(&patch.rgb_srgb16))
            };
            let payload = RemoveAssetPayload {
                width: patch.bounds.width,
                height: patch.bounds.height,
                encoding,
                rgb,
                alpha: Arc::clone(&patch.alpha),
            };
            let fingerprint = remove_payload_fingerprint(&payload);
            let existing = buckets.get(&fingerprint).and_then(|candidates| {
                candidates
                    .iter()
                    .copied()
                    .find(|index| unique_payloads[*index] == payload)
            });
            let asset_index = if let Some(index) = existing {
                let compressed = RemovePatchSidecarCache {
                    fingerprint,
                    rgb_png: Arc::clone(&assets[index].rgb_png),
                    alpha_png: Arc::clone(&assets[index].alpha_png),
                };
                let _ = patch.sidecar_cache.set(compressed);
                index
            } else {
                let pixels = u64::from(payload.width)
                    .checked_mul(u64::from(payload.height))
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
                let decoded_bytes = pixels
                    .checked_mul(7)
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
                decoded_asset_bytes = decoded_asset_bytes
                    .checked_add(decoded_bytes)
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
                if decoded_asset_bytes > MAX_DECODED_REMOVE_ASSET_BYTES {
                    return invalid("retouch patches exceed the decoded asset memory safety limit");
                }
                let compressed =
                    compressed_remove_payload(&payload, fingerprint, &patch.sidecar_cache)?;
                let rgb_bytes = base64_json_string_bytes(compressed.rgb_png.len())?;
                let alpha_bytes = base64_json_string_bytes(compressed.alpha_png.len())?;
                encoded_asset_bytes = encoded_asset_bytes
                    .checked_add(rgb_bytes)
                    .and_then(|bytes| bytes.checked_add(alpha_bytes))
                    .ok_or(SidecarError::TooLarge(u64::MAX))?;
                if encoded_asset_bytes > MAX_SIDECAR_BYTES {
                    return Err(SidecarError::TooLarge(encoded_asset_bytes));
                }
                let index = assets.len();
                assets.push(SidecarRemoveAsset {
                    width: payload.width,
                    height: payload.height,
                    encoding: payload.encoding,
                    rgb_png: compressed.rgb_png,
                    alpha_png: compressed.alpha_png,
                });
                unique_payloads.push(payload);
                buckets.entry(fingerprint).or_default().push(index);
                index
            };
            references.push(SidecarRemoveAssetRef {
                stroke_index,
                patch_index,
                asset_index,
            });
            if references.len() > MAX_REMOVE_ASSET_REFS {
                return invalid("edit contains too many retouch patch references");
            }
            patch.rgb_scene16f = Arc::from([]);
            patch.rgb_srgb16 = Arc::from([]);
            patch.alpha = Arc::from([]);
        }
    }
    Ok((assets, references))
}

fn remove_png_decoder(
    bytes: &Arc<[u8]>,
    expected_output: usize,
) -> Result<png::Reader<Cursor<&[u8]>>, SidecarError> {
    let mut decoder = png::Decoder::new(Cursor::new(bytes.as_ref()));
    let decoder_limit = expected_output
        .checked_add(16 * 1024 * 1024)
        .ok_or(SidecarError::TooLarge(u64::MAX))?;
    decoder.set_limits(png::Limits {
        bytes: decoder_limit,
    });
    decoder.read_info().map_err(|error| {
        SidecarError::Invalid(format!("could not read compressed retouch PNG: {error}"))
    })
}

fn decode_remove_rgb_png(asset: &SidecarRemoveAsset) -> Result<Arc<[u16]>, SidecarError> {
    let pixels = asset.width as usize * asset.height as usize;
    let expected = pixels
        .checked_mul(6)
        .ok_or(SidecarError::TooLarge(u64::MAX))?;
    let mut reader = remove_png_decoder(&asset.rgb_png, expected)?;
    let info = reader.info();
    if info.width != asset.width
        || info.height != asset.height
        || info.color_type != png::ColorType::Rgb
        || info.bit_depth != png::BitDepth::Sixteen
        || info.animation_control.is_some()
        || reader.output_buffer_size() != Some(expected)
    {
        return invalid("compressed retouch RGB PNG metadata is invalid");
    }
    let mut bytes = vec![0u8; expected];
    let output = reader.next_frame(&mut bytes).map_err(|error| {
        SidecarError::Invalid(format!("could not decompress retouch RGB: {error}"))
    })?;
    if output.buffer_size() != expected {
        return invalid("decompressed retouch RGB byte count is invalid");
    }
    Ok(bytes
        .chunks_exact(2)
        .map(|bytes| u16::from_be_bytes([bytes[0], bytes[1]]))
        .collect::<Vec<_>>()
        .into())
}

fn decode_remove_alpha_png(asset: &SidecarRemoveAsset) -> Result<Arc<[u8]>, SidecarError> {
    let expected = asset.width as usize * asset.height as usize;
    let mut reader = remove_png_decoder(&asset.alpha_png, expected)?;
    let info = reader.info();
    if info.width != asset.width
        || info.height != asset.height
        || info.color_type != png::ColorType::Grayscale
        || info.bit_depth != png::BitDepth::Eight
        || info.animation_control.is_some()
        || reader.output_buffer_size() != Some(expected)
    {
        return invalid("compressed retouch alpha PNG metadata is invalid");
    }
    let mut alpha = vec![0u8; expected];
    let output = reader.next_frame(&mut alpha).map_err(|error| {
        SidecarError::Invalid(format!("could not decompress retouch alpha: {error}"))
    })?;
    if output.buffer_size() != expected {
        return invalid("decompressed retouch alpha byte count is invalid");
    }
    Ok(alpha.into())
}

#[derive(Clone)]
struct DecodedRemoveAsset {
    encoding: SidecarRemoveEncoding,
    rgb: Arc<[u16]>,
    alpha: Arc<[u8]>,
    fingerprint: u64,
}

fn restore_remove_assets(
    edits: &mut EditState,
    assets: &[SidecarRemoveAsset],
    references: &[SidecarRemoveAssetRef],
) -> Result<(), SidecarError> {
    if assets.len() > MAX_REMOVE_ASSET_REFS || references.len() > MAX_REMOVE_ASSET_REFS {
        return invalid("sidecar contains too many retouch patch assets");
    }
    let mut patch_count = 0usize;
    for stroke in &edits.remove.strokes {
        patch_count = patch_count
            .checked_add(stroke.patches.len())
            .ok_or(SidecarError::TooLarge(u64::MAX))?;
        for patch in &stroke.patches {
            if !patch.rgb_scene16f.is_empty()
                || !patch.rgb_srgb16.is_empty()
                || !patch.alpha.is_empty()
            {
                return invalid("current sidecar schema contains a legacy inline retouch patch");
            }
        }
    }
    if references.len() != patch_count {
        return invalid("sidecar does not reference every retouch patch payload");
    }

    let mut decoded_bytes = 0u64;
    for asset in assets {
        if asset.width == 0 || asset.height == 0 || asset.width > 32_768 || asset.height > 32_768 {
            return invalid("compressed retouch patch has invalid dimensions");
        }
        let pixels = u64::from(asset.width)
            .checked_mul(u64::from(asset.height))
            .ok_or(SidecarError::TooLarge(u64::MAX))?;
        let bytes = pixels
            .checked_mul(7)
            .ok_or(SidecarError::TooLarge(u64::MAX))?;
        decoded_bytes = decoded_bytes
            .checked_add(bytes)
            .ok_or(SidecarError::TooLarge(u64::MAX))?;
        if decoded_bytes > MAX_DECODED_REMOVE_ASSET_BYTES {
            return invalid("compressed retouch patches exceed the decoded memory safety limit");
        }
    }

    let mut locations = HashSet::new();
    let mut referenced_assets = vec![false; assets.len()];
    for reference in references {
        let Some(asset_referenced) = referenced_assets.get_mut(reference.asset_index) else {
            return invalid("retouch patch reference uses an invalid asset index");
        };
        let Some(patch) = edits
            .remove
            .strokes
            .get(reference.stroke_index)
            .and_then(|stroke| stroke.patches.get(reference.patch_index))
        else {
            return invalid("retouch patch reference uses an invalid patch index");
        };
        let asset = &assets[reference.asset_index];
        if patch.bounds.width != asset.width || patch.bounds.height != asset.height {
            return invalid("retouch patch reference dimensions do not match its asset");
        }
        if !locations.insert((reference.stroke_index, reference.patch_index)) {
            return invalid("sidecar contains duplicate references for a retouch patch");
        }
        *asset_referenced = true;
    }
    if referenced_assets.iter().any(|referenced| !referenced) {
        return invalid("sidecar contains an unreferenced retouch patch asset");
    }

    let decoded = assets
        .iter()
        .map(|asset| {
            let rgb = decode_remove_rgb_png(asset)?;
            let alpha = decode_remove_alpha_png(asset)?;
            let fingerprint = remove_payload_fingerprint(&RemoveAssetPayload {
                width: asset.width,
                height: asset.height,
                encoding: asset.encoding,
                rgb: Arc::clone(&rgb),
                alpha: Arc::clone(&alpha),
            });
            Ok(DecodedRemoveAsset {
                encoding: asset.encoding,
                rgb,
                alpha,
                fingerprint,
            })
        })
        .collect::<Result<Vec<_>, SidecarError>>()?;
    let remove = Arc::make_mut(&mut edits.remove);
    for reference in references {
        let asset = &assets[reference.asset_index];
        let decoded = &decoded[reference.asset_index];
        let patch = &mut remove.strokes[reference.stroke_index].patches[reference.patch_index];
        match decoded.encoding {
            SidecarRemoveEncoding::Scene16f => {
                patch.rgb_scene16f = Arc::clone(&decoded.rgb);
                patch.rgb_srgb16 = Arc::from([]);
            }
            SidecarRemoveEncoding::Srgb16 => {
                patch.rgb_scene16f = Arc::from([]);
                patch.rgb_srgb16 = Arc::clone(&decoded.rgb);
            }
        }
        patch.alpha = Arc::clone(&decoded.alpha);
        let _ = patch.sidecar_cache.set(RemovePatchSidecarCache {
            fingerprint: decoded.fingerprint,
            rgb_png: Arc::clone(&asset.rgb_png),
            alpha_png: Arc::clone(&asset.alpha_png),
        });
    }
    Ok(())
}

#[derive(Clone, Debug, PartialEq)]
pub struct LoadedSidecar {
    pub edits: EditState,
    pub migrated: bool,
}

#[derive(Debug)]
pub enum SidecarError {
    Io(std::io::Error),
    Invalid(String),
    Unsupported(String),
    Platform(String),
    TooLarge(u64),
}

impl fmt::Display for SidecarError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Invalid(message) | Self::Unsupported(message) | Self::Platform(message) => {
                formatter.write_str(message)
            }
            Self::TooLarge(bytes) => write!(
                formatter,
                "sidecar is {bytes} bytes; the safety limit is {MAX_SIDECAR_BYTES} bytes"
            ),
        }
    }
}

impl std::error::Error for SidecarError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Invalid(_) | Self::Unsupported(_) | Self::Platform(_) | Self::TooLarge(_) => None,
        }
    }
}

impl From<std::io::Error> for SidecarError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

mod desktop;

pub use desktop::sidecar_path_for_raw;
#[cfg(not(target_os = "android"))]
pub use desktop::{
    copy_developed_thumbnail_cache, desktop_sidecar_fingerprint,
    developed_thumbnail_cache_is_fresh, developed_thumbnail_path_for_raw,
    invalidate_developed_thumbnail_cache, load_developed_thumbnail_cache, remove_desktop_edits,
    save_developed_thumbnail_cache,
};

pub fn encode(mut edits: EditState) -> Result<Vec<u8>, SidecarError> {
    synchronize_subject_refinement(&mut edits);
    validate_edit_state(&edits)?;
    let (mask_assets, mask_asset_refs) = extract_mask_assets(&mut edits)?;
    let (remove_assets, remove_asset_refs) = extract_remove_assets(&mut edits)?;
    let document = SidecarDocument {
        format: SIDECAR_FORMAT.to_owned(),
        schema_version: SIDECAR_SCHEMA_VERSION,
        edits,
        mask_assets,
        mask_asset_refs,
        remove_assets,
        remove_asset_refs,
    };
    let mut writer = CappedVec::new(MAX_SIDECAR_BYTES);
    serde_json::to_writer(&mut writer, &document).map_err(|error| {
        if writer.limit_reached {
            SidecarError::TooLarge(MAX_SIDECAR_BYTES + 1)
        } else {
            SidecarError::Invalid(format!("could not serialize edit: {error}"))
        }
    })?;
    Ok(writer.bytes)
}

pub fn decode(bytes: &[u8]) -> Result<LoadedSidecar, SidecarError> {
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(bytes.len() as u64));
    }
    let mut document: SidecarDocument = serde_json::from_slice(bytes)
        .map_err(|error| SidecarError::Invalid(format!("invalid sidecar JSON: {error}")))?;
    if document.format != SIDECAR_FORMAT {
        return Err(SidecarError::Invalid(
            "not an AuRaw edit sidecar".to_owned(),
        ));
    }
    if document.schema_version > SIDECAR_SCHEMA_VERSION {
        return Err(SidecarError::Unsupported(format!(
            "sidecar schema {} is newer than supported schema {}",
            document.schema_version, SIDECAR_SCHEMA_VERSION
        )));
    }
    let original_schema = document.schema_version;
    if original_schema >= 5 {
        restore_mask_assets(
            &mut document.edits,
            &document.mask_assets,
            &document.mask_asset_refs,
        )?;
    } else if !document.mask_assets.is_empty() || !document.mask_asset_refs.is_empty() {
        return invalid("legacy sidecar unexpectedly contains current mask assets");
    }
    if original_schema >= 15 {
        restore_remove_assets(
            &mut document.edits,
            &document.remove_assets,
            &document.remove_asset_refs,
        )?;
    } else if !document.remove_assets.is_empty() || !document.remove_asset_refs.is_empty() {
        return invalid("legacy sidecar unexpectedly contains current retouch assets");
    }
    if original_schema < 16 {
        migrate_legacy_retouch_opacity(&mut document.edits);
    }

    synchronize_subject_refinement(&mut document.edits);

    validate_edit_state(&document.edits)?;
    document.edits.exposure.sanitize_tone_curves();
    for mask in &mut Arc::make_mut(&mut document.edits.masks).masks {
        mask.adjustments.sanitize_tone_curves();
    }
    validate_edit_state(&document.edits)?;

    Ok(LoadedSidecar {
        edits: document.edits,
        migrated: original_schema != SIDECAR_SCHEMA_VERSION,
    })
}

pub fn load_desktop(raw_path: &Path) -> Result<Option<LoadedSidecar>, SidecarError> {
    let path = sidecar_path_for_raw(raw_path);
    match read_bounded(&path) {
        Ok(bytes) => decode(&bytes).map(Some),
        Err(SidecarError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

pub fn save_desktop(raw_path: &Path, edits: EditState) -> Result<PathBuf, SidecarError> {
    let path = sidecar_path_for_raw(raw_path);
    let bytes = encode(edits)?;
    atomic_write(&path, &bytes)?;
    Ok(path)
}

#[cfg(not(target_os = "android"))]
pub fn reset_desktop_adjustments(raw_path: &Path) -> Result<bool, String> {
    remove_desktop_edits(raw_path)
}

pub fn read_bounded(path: &Path) -> Result<Vec<u8>, SidecarError> {
    let file = File::open(path)?;
    let declared = file.metadata()?.len();
    if declared > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(declared));
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    file.take(MAX_SIDECAR_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(bytes.len() as u64));
    }
    Ok(bytes)
}

#[cfg(target_os = "android")]
pub fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), SidecarError> {
    if bytes.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(SidecarError::TooLarge(bytes.len() as u64));
    }
    let mut file = OpenOptions::new().write(true).truncate(true).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), SidecarError> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let file_name = path
        .file_name()
        .ok_or_else(|| SidecarError::Invalid("sidecar path has no file name".to_owned()))?;
    let mut temporary_name = OsString::from(".");
    temporary_name.push(file_name);
    temporary_name.push(format!(
        ".tmp-{}-{}",
        std::process::id(),
        NEXT_TEMPORARY_ID.fetch_add(1, Ordering::Relaxed)
    ));
    let temporary = parent.join(temporary_name);

    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        replace_file(&temporary, path)?;
        sync_parent_directory(parent)?;
        Ok::<(), std::io::Error>(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result.map_err(SidecarError::Io)
}

mod validation;
use validation::{invalid, validate_edit_state, validate_image};

struct CappedVec {
    bytes: Vec<u8>,
    limit: u64,
    limit_reached: bool,
}

impl CappedVec {
    fn new(limit: u64) -> Self {
        Self {
            bytes: Vec::new(),
            limit,
            limit_reached: false,
        }
    }
}

impl Write for CappedVec {
    fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        let next = self.bytes.len() as u64 + buffer.len() as u64;
        if next > self.limit {
            self.limit_reached = true;
            return Err(std::io::Error::other("sidecar size limit reached"));
        }
        self.bytes.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub fn preflight_mask_change(masks: &MaskStack) -> Result<(), SidecarError> {
    preflight_sidecar_dynamic_data_with_limit(masks, MAX_SIDECAR_BYTES)
}

fn preflight_sidecar_dynamic_data_with_limit(
    masks: &MaskStack,
    limit: u64,
) -> Result<(), SidecarError> {
    let conservative = estimate_sidecar_bytes(masks)?;
    if conservative <= limit {
        return Ok(());
    }
    let measured = measure_sidecar_dynamic_bytes(masks)?;
    enforce_size_limit(measured, limit)
}

fn enforce_size_limit(estimated: u64, limit: u64) -> Result<(), SidecarError> {
    if estimated > limit {
        Err(SidecarError::TooLarge(estimated))
    } else {
        Ok(())
    }
}

fn estimate_sidecar_bytes(masks: &MaskStack) -> Result<u64, SidecarError> {
    const DOCUMENT_HEADROOM: u64 = 1024 * 1024;
    const MASK_HEADROOM: u64 = 16 * 1024;
    const COMPONENT_HEADROOM: u64 = 2 * 1024;
    const BRUSH_DAB_HEADROOM: u64 = 256;
    const OBJECT_STROKE_HEADROOM: u64 = 128;
    const OBJECT_POINT_HEADROOM: u64 = 96;
    const MASK_PNG_FIXED_HEADROOM: u64 = 64 * 1024;

    let mut estimated = DOCUMENT_HEADROOM;
    checked_add_scaled(
        &mut estimated,
        masks.subject_refinement.dabs.len(),
        BRUSH_DAB_HEADROOM,
    )?;
    let mut unique_images = Vec::<&MaskImage>::new();
    let mut image_buckets = HashMap::<u64, Vec<usize>>::new();
    for mask in &masks.masks {
        checked_add(&mut estimated, MASK_HEADROOM)?;
        checked_add(&mut estimated, escaped_json_string_bound(&mask.name)?)?;
        for component in &mask.components {
            checked_add(&mut estimated, COMPONENT_HEADROOM)?;
            checked_add(&mut estimated, escaped_json_string_bound(&component.name)?)?;
            match &component.geometry {
                MaskGeometry::Brush { dabs, .. } => {
                    checked_add_scaled(&mut estimated, dabs.len(), BRUSH_DAB_HEADROOM)?
                }
                MaskGeometry::Ai {
                    mask: Some(image), ..
                } => add_unique_mask_asset_bound(
                    &mut estimated,
                    image,
                    &mut unique_images,
                    &mut image_buckets,
                    MASK_PNG_FIXED_HEADROOM,
                )?,
                MaskGeometry::Object { mask, strokes, .. } => {
                    if let Some(image) = mask {
                        add_unique_mask_asset_bound(
                            &mut estimated,
                            image,
                            &mut unique_images,
                            &mut image_buckets,
                            MASK_PNG_FIXED_HEADROOM,
                        )?;
                    }
                    checked_add_scaled(&mut estimated, strokes.len(), OBJECT_STROKE_HEADROOM)?;
                    for stroke in strokes {
                        checked_add_scaled(
                            &mut estimated,
                            stroke.points.len(),
                            OBJECT_POINT_HEADROOM,
                        )?;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(estimated)
}

fn measure_sidecar_dynamic_bytes(masks: &MaskStack) -> Result<u64, SidecarError> {
    const DOCUMENT_HEADROOM: u64 = 1024 * 1024;
    const MASK_HEADROOM: u64 = 16 * 1024;
    const COMPONENT_HEADROOM: u64 = 2 * 1024;
    const OBJECT_STROKE_HEADROOM: u64 = 128;
    const OBJECT_POINT_HEADROOM: u64 = 96;
    const BRUSH_DAB_HEADROOM: u64 = 256;

    let mut measured = DOCUMENT_HEADROOM;
    checked_add_scaled(
        &mut measured,
        masks.subject_refinement.dabs.len(),
        BRUSH_DAB_HEADROOM,
    )?;
    let mut unique_images = Vec::<&MaskImage>::new();
    let mut image_buckets = HashMap::<u64, Vec<usize>>::new();
    for mask in &masks.masks {
        checked_add(&mut measured, MASK_HEADROOM)?;
        checked_add(&mut measured, escaped_json_string_bound(&mask.name)?)?;
        for component in &mask.components {
            checked_add(&mut measured, COMPONENT_HEADROOM)?;
            checked_add(&mut measured, escaped_json_string_bound(&component.name)?)?;
            match &component.geometry {
                MaskGeometry::Brush { dabs, .. } => {
                    checked_add_scaled(&mut measured, dabs.len(), BRUSH_DAB_HEADROOM)?
                }
                MaskGeometry::Ai {
                    mask: Some(image), ..
                } => add_unique_mask_asset_measured(
                    &mut measured,
                    image,
                    &mut unique_images,
                    &mut image_buckets,
                )?,
                MaskGeometry::Object { mask, strokes, .. } => {
                    if let Some(image) = mask {
                        add_unique_mask_asset_measured(
                            &mut measured,
                            image,
                            &mut unique_images,
                            &mut image_buckets,
                        )?;
                    }
                    checked_add_scaled(&mut measured, strokes.len(), OBJECT_STROKE_HEADROOM)?;
                    for stroke in strokes {
                        checked_add_scaled(
                            &mut measured,
                            stroke.points.len(),
                            OBJECT_POINT_HEADROOM,
                        )?;
                    }
                }
                _ => {}
            }
        }
    }

    Ok(measured)
}

fn add_unique_mask_asset_measured<'a>(
    measured: &mut u64,
    image: &'a MaskImage,
    unique_images: &mut Vec<&'a MaskImage>,
    buckets: &mut HashMap<u64, Vec<usize>>,
) -> Result<(), SidecarError> {
    let fingerprint = mask_image_fingerprint(image);
    if buckets.get(&fingerprint).is_some_and(|candidates| {
        candidates
            .iter()
            .any(|index| *unique_images[*index] == *image)
    }) {
        return Ok(());
    }

    let index = unique_images.len();
    unique_images.push(image);
    buckets.entry(fingerprint).or_default().push(index);
    let png = encode_mask_png(image)?;
    checked_add(measured, base64_json_string_bytes(png.len())?)
}

fn add_unique_mask_asset_bound<'a>(
    estimated: &mut u64,
    image: &'a MaskImage,
    unique_images: &mut Vec<&'a MaskImage>,
    buckets: &mut HashMap<u64, Vec<usize>>,
    fixed_headroom: u64,
) -> Result<(), SidecarError> {
    let fingerprint = mask_image_fingerprint(image);
    if buckets.get(&fingerprint).is_some_and(|candidates| {
        candidates
            .iter()
            .any(|index| *unique_images[*index] == *image)
    }) {
        return Ok(());
    }

    let index = unique_images.len();
    unique_images.push(image);
    buckets.entry(fingerprint).or_default().push(index);
    let raw_bytes =
        u64::try_from(image.pixels.len()).map_err(|_| SidecarError::TooLarge(u64::MAX))?;
    let png_bound = raw_bytes
        .checked_add(raw_bytes.div_ceil(64))
        .and_then(|bytes| bytes.checked_add(fixed_headroom))
        .ok_or(SidecarError::TooLarge(u64::MAX))?;
    let png_bound = usize::try_from(png_bound).map_err(|_| SidecarError::TooLarge(png_bound))?;
    checked_add(estimated, base64_json_string_bytes(png_bound)?)
}

fn checked_add(total: &mut u64, value: u64) -> Result<(), SidecarError> {
    *total = total
        .checked_add(value)
        .ok_or(SidecarError::TooLarge(u64::MAX))?;
    Ok(())
}

fn checked_add_scaled(
    total: &mut u64,
    count: usize,
    bytes_per_item: u64,
) -> Result<(), SidecarError> {
    let count = u64::try_from(count).map_err(|_| SidecarError::TooLarge(u64::MAX))?;
    let bytes = count
        .checked_mul(bytes_per_item)
        .ok_or(SidecarError::TooLarge(u64::MAX))?;
    checked_add(total, bytes)
}

fn escaped_json_string_bound(value: &str) -> Result<u64, SidecarError> {
    let bytes = u64::try_from(value.len()).map_err(|_| SidecarError::TooLarge(u64::MAX))?;
    bytes
        .checked_mul(6)
        .and_then(|bytes| bytes.checked_add(2))
        .ok_or(SidecarError::TooLarge(u64::MAX))
}

fn base64_json_string_bytes(byte_count: usize) -> Result<u64, SidecarError> {
    let byte_count = u64::try_from(byte_count).map_err(|_| SidecarError::TooLarge(u64::MAX))?;
    byte_count
        .div_ceil(3)
        .checked_mul(4)
        .and_then(|bytes| bytes.checked_add(2))
        .ok_or(SidecarError::TooLarge(u64::MAX))
}

#[cfg(test)]
mod tests;
