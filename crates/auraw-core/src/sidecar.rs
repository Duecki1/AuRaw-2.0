use crate::file_ops::{replace_file, sync_parent_directory};
#[cfg(not(target_os = "android"))]
use crate::pipeline::RawThumbnail;
use crate::pipeline::{
    ExposureParams, GeometryTransform, InpaintStroke, MaskGeometry, MaskImage, MaskKind, MaskStack,
    MAX_LOCAL_MASKS, MAX_MASK_COMPONENTS,
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

pub const SIDECAR_SCHEMA_VERSION: u32 = 5;
/// Bump when developed-thumbnail rendering semantics change without changing the sidecar bytes.
pub const DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT: u64 = 0x4155_5241_5700_0004;
pub const SIDECAR_SUFFIX: &str = ".auraw";
#[cfg(not(target_os = "android"))]
pub const DEVELOPED_THUMBNAIL_SUFFIX: &str = ".auraw-thumb.jpg";
#[cfg(not(target_os = "android"))]
#[cfg(any(not(target_os = "android"), test))]
pub const DEVELOPED_THUMBNAIL_CACHE_DIR: &str = crate::thumbnail_cache::DESKTOP_THUMBNAIL_CACHE_DIR;
#[cfg(not(target_os = "android"))]
const DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX: &str = ".auraw-thumb.fingerprint";
pub const MAX_SIDECAR_BYTES: u64 = if cfg!(target_os = "android") {
    32 * 1024 * 1024
} else {
    64 * 1024 * 1024
};

const SIDECAR_FORMAT: &str = "AuRaw edit sidecar";
const MAX_BRUSH_DABS: usize = 1_000_000;
const MAX_OBJECT_STROKES: usize = 4096;
const MAX_OBJECT_STROKE_POINTS: usize = 1_000_000;
const MAX_INPAINT_STROKES: usize = 4096;
const MAX_INPAINT_DABS: usize = 1_000_000;
const MAX_MASK_IMAGE_EDGE: u32 = 8192;
const MAX_MASK_ASSET_REFS: usize = MAX_LOCAL_MASKS * MAX_MASK_COMPONENTS;
const MAX_DECODED_MASK_ASSET_BYTES: u64 = if cfg!(target_os = "android") {
    256 * 1024 * 1024
} else {
    512 * 1024 * 1024
};
const MAX_EDIT_NAME_BYTES: usize = 4096;
static NEXT_TEMPORARY_ID: AtomicU64 = AtomicU64::new(0);

/// Stable location used by a background save worker. Android targets retain
/// the MediaStore URI because the native decode path itself uses a disposable
/// cache file.
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
    /// Manual Brush, Radial, and Linear mask components.
    #[serde(default = "default_true")]
    pub masks: bool,
    /// AI and source-dependent mask components.
    #[serde(default = "default_true")]
    pub ai_masks: bool,
    #[serde(default)]
    pub inpainting: bool,
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
            inpainting: false,
            lens_correction: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
pub struct EditState {
    pub exposure: ExposureParams,
    #[serde(default)]
    pub geometry: GeometryTransform,
    /// Explicit per-image DCP selection relative to the configured camera-profile root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_profile: Option<PathBuf>,
    pub masks: Arc<MaskStack>,
    #[serde(default)]
    pub inpainting: Arc<Vec<InpaintStroke>>,
    pub lens: LensEditState,
    /// Persisted because copied content-aware masks belong to the source image
    /// until they are explicitly regenerated on the destination image.
    #[serde(default)]
    pub ai_masks_need_update: bool,
}

pub fn default_edit_state() -> EditState {
    EditState {
        exposure: ExposureParams::scene_referred_default(),
        geometry: GeometryTransform::default(),
        camera_profile: None,
        masks: Arc::new(MaskStack::default()),
        inpainting: Arc::new(Vec::new()),
        lens: LensEditState::default(),
        ai_masks_need_update: false,
    }
}

fn is_manual_mask_kind(kind: MaskKind) -> bool {
    matches!(kind, MaskKind::Brush | MaskKind::Radial | MaskKind::Linear)
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
                // A local-mask group can combine a brush/radial/linear component
                // with an AI or range component. Filter those components one by
                // one: classifying the whole group as AI used to leak the manual
                // refinement even when "Normal masks" was disabled.
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
    merged.masks.extend(copied.masks);
    *destination = merged;
}

fn masks_contain_content_aware_components(masks: &MaskStack) -> bool {
    // Generated bitmaps are caches, not the identity of an AI component. A
    // stale pasted mask can have `mask: None` and still require regeneration.
    masks.masks.iter().any(|mask| {
        mask.components
            .iter()
            .any(|component| match (component.kind, &component.geometry) {
                (MaskKind::Subject | MaskKind::Background, MaskGeometry::Ai { .. }) => true,
                (MaskKind::Landscape, MaskGeometry::Landscape { .. }) => true,
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
        || edits.inpainting != default.inpainting
        || edits.lens != default.lens
}

/// Merge a copied edit snapshot into an existing destination according to the
/// user's library copy settings. Content-aware masks are deliberately marked
/// stale after crossing image boundaries so Develop exposes the refresh action.
pub fn apply_copied_adjustments(
    destination: &mut EditState,
    source: &EditState,
    settings: AdjustmentCopySettings,
) {
    apply_copied_adjustments_with_mode(destination, source, settings, AdjustmentPasteMode::Merge);
}

/// Applies copied edits using an explicit conflict policy.
///
/// Merge preserves destination categories that were not enabled when the copy
/// was made. Replace first resets the destination to a clean edit state, then
/// installs the enabled copied categories.
pub fn apply_copied_adjustments_with_mode(
    destination: &mut EditState,
    source: &EditState,
    settings: AdjustmentCopySettings,
    mode: AdjustmentPasteMode,
) {
    if mode == AdjustmentPasteMode::Replace {
        *destination = default_edit_state();
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
        let mut masks = destination.masks.as_ref().clone();
        replace_selected_mask_categories(
            &mut masks,
            &source.masks,
            settings.masks,
            settings.ai_masks,
        );
        destination.masks = Arc::new(masks);
        destination.ai_masks_need_update = if settings.ai_masks {
            source.ai_masks_need_update
                || masks_contain_content_aware_components(&destination.masks)
        } else {
            previous_ai_masks_need_update
        };
    }
    if settings.inpainting {
        let inpainting_changed = destination.inpainting != source.inpainting;
        destination.inpainting = Arc::clone(&source.inpainting);
        if inpainting_changed && masks_contain_content_aware_components(&destination.masks) {
            destination.ai_masks_need_update = true;
        }
    }
    if settings.lens_correction {
        let lens_changed = destination.lens != source.lens;
        destination.lens = source.lens.clone();
        if lens_changed && masks_contain_content_aware_components(&destination.masks) {
            destination.ai_masks_need_update = true;
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
        MaskGeometry::Ai { mask, .. }
        | MaskGeometry::Landscape { mask, .. }
        | MaskGeometry::Object { mask, .. } => Some(mask),
        _ => None,
    }
}

fn generated_mask_mut(geometry: &mut MaskGeometry) -> Option<&mut Option<MaskImage>> {
    match geometry {
        MaskGeometry::Ai { mask, .. }
        | MaskGeometry::Landscape { mask, .. }
        | MaskGeometry::Object { mask, .. } => Some(mask),
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
pub struct LoadedSidecar {
    pub edits: EditState,
    /// True when an older supported schema was canonicalized in memory and
    /// should be rewritten on load.
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

/// Appends rather than replaces the RAW extension: `photo.CR3` becomes
/// `photo.CR3.auraw`. Building from `OsString` preserves non-UTF-8 paths.
pub fn sidecar_path_for_raw(raw_path: &Path) -> PathBuf {
    let mut path: OsString = raw_path.as_os_str().to_owned();
    path.push(SIDECAR_SUFFIX);
    PathBuf::from(path)
}

/// Places the developed preview in AuRaw's private per-user cache rather than
/// creating hidden files beside the user's RAW library.
#[cfg(not(target_os = "android"))]
pub fn developed_thumbnail_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::desktop_cache_path_for_raw(raw_path, DEVELOPED_THUMBNAIL_SUFFIX)
}

#[cfg(not(target_os = "android"))]
fn developed_thumbnail_fingerprint_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::desktop_cache_path_for_raw(
        raw_path,
        DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX,
    )
}

#[cfg(not(target_os = "android"))]
fn legacy_developed_thumbnail_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::legacy_sibling_cache_path_for_raw(raw_path, DEVELOPED_THUMBNAIL_SUFFIX)
}

#[cfg(not(target_os = "android"))]
fn legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path: &Path) -> PathBuf {
    crate::thumbnail_cache::legacy_sibling_cache_path_for_raw(
        raw_path,
        DEVELOPED_THUMBNAIL_FINGERPRINT_SUFFIX,
    )
}

/// Returns a stable fingerprint of the current edit sidecar. Thumbnail workers
/// compare this before and after GPU readback so an older render can never
/// overwrite the cache for a newer save.
#[cfg(not(target_os = "android"))]
pub fn desktop_sidecar_fingerprint(raw_path: &Path) -> Result<Option<u64>, String> {
    let path = sidecar_path_for_raw(raw_path);
    let bytes = match read_bounded(&path) {
        Ok(bytes) => bytes,
        Err(SidecarError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(None)
        }
        Err(error) => {
            return Err(format!(
                "could not fingerprint edit sidecar {}: {error}",
                path.display()
            ))
        }
    };

    // FNV-1a is deliberately simple and deterministic. This is an invalidation
    // token, not a cryptographic integrity check.
    let mut fingerprint = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        fingerprint ^= u64::from(byte);
        fingerprint = fingerprint.wrapping_mul(0x0000_0100_0000_01b3);
    }
    Ok(Some(fingerprint))
}

/// Loads a developed thumbnail only when it is newer than the RAW and its
/// stored sidecar fingerprint exactly matches the current edit file. Missing or
/// stale caches are regenerated from the RAW plus its sidecar by the library
/// thumbnail worker before any unedited embedded preview is considered.
#[cfg(not(target_os = "android"))]
pub fn load_developed_thumbnail_cache(
    raw_path: &Path,
    maximum_edge: u32,
) -> Result<Option<RawThumbnail>, String> {
    if maximum_edge == 0 {
        return Err("thumbnail edge must be non-zero".to_owned());
    }
    if !developed_thumbnail_cache_is_fresh(raw_path)? {
        return Ok(None);
    }
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    match crate::thumbnail_cache::load_jpeg(&cache_path, maximum_edge) {
        Ok(Some(thumbnail)) => Ok(Some(thumbnail)),
        Ok(None) => {
            let _ = fs::remove_file(developed_thumbnail_fingerprint_path_for_raw(raw_path));
            Ok(None)
        }
        Err(error) => {
            let _ = fs::remove_file(&cache_path);
            let _ = fs::remove_file(developed_thumbnail_fingerprint_path_for_raw(raw_path));
            Err(format!(
                "could not decode developed thumbnail {}: {error}",
                cache_path.display()
            ))
        }
    }
}

#[cfg(not(target_os = "android"))]
pub fn developed_thumbnail_cache_is_fresh(raw_path: &Path) -> Result<bool, String> {
    migrate_legacy_developed_thumbnail_cache(raw_path)?;
    let sidecar_path = sidecar_path_for_raw(raw_path);
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    let fingerprint_path = developed_thumbnail_fingerprint_path_for_raw(raw_path);
    let cache_metadata = match fs::metadata(&cache_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "could not inspect developed thumbnail {}: {error}",
                cache_path.display()
            ))
        }
    };
    let _sidecar_metadata = match fs::metadata(&sidecar_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(format!(
                "could not inspect edit sidecar {}: {error}",
                sidecar_path.display()
            ))
        }
    };
    let raw_metadata = fs::metadata(raw_path).map_err(|error| {
        format!(
            "could not inspect RAW while validating its thumbnail {}: {error}",
            raw_path.display()
        )
    })?;

    let Ok(cache_modified) = cache_metadata.modified() else {
        return Ok(false);
    };
    let Ok(raw_modified) = raw_metadata.modified() else {
        return Ok(false);
    };
    if cache_modified < raw_modified {
        remove_legacy_developed_thumbnail_cache(raw_path);
        return Ok(false);
    }

    // Hash the sidecar only after the cheap existence and timestamp checks.
    // Missing/stale caches therefore never pay to read a potentially large
    // sidecar containing raster masks.
    let cached_fingerprint = match fs::read_to_string(&fingerprint_path) {
        Ok(value) => match u64::from_str_radix(value.trim(), 16) {
            Ok(value) => value,
            Err(_) => {
                remove_legacy_developed_thumbnail_cache(raw_path);
                return Ok(false);
            }
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            remove_legacy_developed_thumbnail_cache(raw_path);
            return Ok(false);
        }
        Err(error) => {
            return Err(format!(
                "could not read developed thumbnail fingerprint {}: {error}",
                fingerprint_path.display()
            ))
        }
    };
    let fresh = desktop_sidecar_fingerprint(raw_path)?
        .map(|fingerprint| fingerprint ^ DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT)
        == Some(cached_fingerprint);
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(fresh)
}

/// Atomically stores a GPU-rendered thumbnail, but only if the sidecar still
/// has the fingerprint that was current when the render began.
#[cfg(not(target_os = "android"))]
pub fn save_developed_thumbnail_cache(
    raw_path: &Path,
    thumbnail: &RawThumbnail,
    expected_sidecar_fingerprint: u64,
) -> Result<PathBuf, String> {
    if desktop_sidecar_fingerprint(raw_path)? != Some(expected_sidecar_fingerprint) {
        return Err("edit sidecar changed while its thumbnail was rendering".to_owned());
    }
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    let fingerprint_path = developed_thumbnail_fingerprint_path_for_raw(raw_path);
    crate::thumbnail_cache::save_jpeg(&cache_path, thumbnail).map_err(|error| {
        format!(
            "could not cache developed thumbnail {}: {error}",
            cache_path.display()
        )
    })?;
    atomic_write(
        &fingerprint_path,
        format!(
            "{:016x}\n",
            expected_sidecar_fingerprint ^ DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT
        )
        .as_bytes(),
    )
    .map_err(|error| {
        format!(
            "could not cache developed thumbnail fingerprint {}: {error}",
            fingerprint_path.display()
        )
    })?;

    if desktop_sidecar_fingerprint(raw_path)? != Some(expected_sidecar_fingerprint) {
        let _ = fs::remove_file(&cache_path);
        let _ = fs::remove_file(&fingerprint_path);
        return Err("edit sidecar changed while its thumbnail was being cached".to_owned());
    }
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(cache_path)
}

#[cfg(not(target_os = "android"))]
fn migrate_legacy_developed_thumbnail_cache(raw_path: &Path) -> Result<(), String> {
    let cache_path = developed_thumbnail_path_for_raw(raw_path);
    let fingerprint_path = developed_thumbnail_fingerprint_path_for_raw(raw_path);
    if cache_path.is_file() && fingerprint_path.is_file() {
        return Ok(());
    }

    let legacy_cache = legacy_developed_thumbnail_path_for_raw(raw_path);
    let legacy_fingerprint = legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path);
    if !legacy_cache.is_file() || !legacy_fingerprint.is_file() {
        return Ok(());
    }

    let raw_metadata = match fs::metadata(raw_path) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(()),
    };
    let cache_metadata = match fs::metadata(&legacy_cache) {
        Ok(metadata) => metadata,
        Err(_) => {
            remove_legacy_developed_thumbnail_cache(raw_path);
            return Ok(());
        }
    };
    let cache_is_new_enough = cache_metadata
        .modified()
        .ok()
        .zip(raw_metadata.modified().ok())
        .is_some_and(|(cache_modified, raw_modified)| cache_modified >= raw_modified);
    let Some(current_fingerprint) = desktop_sidecar_fingerprint(raw_path)? else {
        remove_legacy_developed_thumbnail_cache(raw_path);
        return Ok(());
    };
    let cached_fingerprint = fs::read_to_string(&legacy_fingerprint)
        .ok()
        .and_then(|value| u64::from_str_radix(value.trim(), 16).ok());
    if !cache_is_new_enough || cached_fingerprint != Some(current_fingerprint) {
        remove_legacy_developed_thumbnail_cache(raw_path);
        return Ok(());
    }

    let thumbnail = match crate::thumbnail_cache::load_jpeg(&legacy_cache, 8192) {
        Ok(Some(thumbnail)) => thumbnail,
        Ok(None) | Err(_) => {
            remove_legacy_developed_thumbnail_cache(raw_path);
            return Ok(());
        }
    };
    if save_developed_thumbnail_cache(raw_path, &thumbnail, current_fingerprint).is_err() {
        return Ok(());
    }
    Ok(())
}

#[cfg(not(target_os = "android"))]
pub fn invalidate_developed_thumbnail_cache(raw_path: &Path) -> Result<(), String> {
    for path in [
        developed_thumbnail_path_for_raw(raw_path),
        developed_thumbnail_fingerprint_path_for_raw(raw_path),
        legacy_developed_thumbnail_path_for_raw(raw_path),
        legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path),
    ] {
        match fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("could not remove {}: {error}", path.display())),
        }
    }
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(())
}

#[cfg(not(target_os = "android"))]
fn remove_legacy_developed_thumbnail_cache(raw_path: &Path) {
    crate::thumbnail_cache::remove_legacy_cache_file(&legacy_developed_thumbnail_path_for_raw(
        raw_path,
    ));
    crate::thumbnail_cache::remove_legacy_cache_file(
        &legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path),
    );
}

#[cfg(not(target_os = "android"))]
pub fn remove_desktop_edits(raw_path: &Path) -> Result<bool, String> {
    let paths = [
        sidecar_path_for_raw(raw_path),
        developed_thumbnail_path_for_raw(raw_path),
        developed_thumbnail_fingerprint_path_for_raw(raw_path),
        legacy_developed_thumbnail_path_for_raw(raw_path),
        legacy_developed_thumbnail_fingerprint_path_for_raw(raw_path),
    ];
    let mut removed_any = false;
    for path in paths {
        match fs::remove_file(&path) {
            Ok(()) => removed_any = true,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!("could not remove {}: {error}", path.display()));
            }
        }
    }
    remove_legacy_developed_thumbnail_cache(raw_path);
    Ok(removed_any)
}

pub fn encode(mut edits: EditState) -> Result<Vec<u8>, SidecarError> {
    validate_edit_state(&edits)?;
    let (mask_assets, mask_asset_refs) = extract_mask_assets(&mut edits)?;
    let document = SidecarDocument {
        format: SIDECAR_FORMAT.to_owned(),
        schema_version: SIDECAR_SCHEMA_VERSION,
        edits,
        mask_assets,
        mask_asset_refs,
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
    if original_schema == SIDECAR_SCHEMA_VERSION {
        restore_mask_assets(
            &mut document.edits,
            &document.mask_assets,
            &document.mask_asset_refs,
        )?;
    } else {
        // TODO(pre-release cleanup): Remove this beta-only schema <= 4 inline-mask migration
        // once AuRaw is published. It exists only so beta testers keep their edits and masks;
        // `LoadedSidecar::migrated` immediately queues a schema-5 rewrite after opening the RAW.
        if !document.mask_assets.is_empty() || !document.mask_asset_refs.is_empty() {
            return invalid("legacy sidecar unexpectedly contains current mask assets");
        }
    }

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
    // "Reset all adjustments" is deliberately destructive: the sidecar is
    // the complete persisted edit state, including normal masks, generated AI
    // mask bitmaps/prompts, crop, inpainting, camera-profile selection, and
    // lens correction. Removing it guarantees the next open starts from the
    // untouched RAW instead of accidentally retaining hidden mask state.
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
    // A single relative component has `Some("")` as its parent. Opening that
    // empty path for the durability sync fails even though the rename already
    // succeeded, so normalize it to the current directory up front.
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

fn validate_edit_state(edits: &EditState) -> Result<(), SidecarError> {
    validate_exposure(&edits.exposure)?;
    if let Some(profile) = &edits.camera_profile {
        if profile.as_os_str().len() > MAX_EDIT_NAME_BYTES * 4 {
            return invalid("camera profile path is unreasonably long");
        }
        if profile.is_absolute()
            || profile.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            return invalid("camera profile path must stay inside the configured profile folder");
        }
    }
    let stack = &edits.masks;
    if stack.masks.len() > MAX_LOCAL_MASKS {
        return invalid("sidecar contains too many local masks");
    }
    if stack
        .selected_mask
        .is_some_and(|index| index >= stack.masks.len())
    {
        return invalid("selected mask index is out of range");
    }
    if edits.lens.maker.len() > MAX_EDIT_NAME_BYTES || edits.lens.model.len() > MAX_EDIT_NAME_BYTES
    {
        return invalid("lens name is unreasonably long");
    }

    for (mask_index, mask) in stack.masks.iter().enumerate() {
        finite("mask opacity", &[mask.opacity])?;
        if !(0.0..=1.0).contains(&mask.opacity) {
            return invalid("mask opacity is outside 0..1");
        }
        validate_local_adjustments(&mask.adjustments)?;
        if mask.name.len() > MAX_EDIT_NAME_BYTES {
            return invalid("mask name is unreasonably long");
        }
        if mask.components.is_empty() || mask.components.len() > MAX_MASK_COMPONENTS {
            return invalid("mask has an invalid component count");
        }
        if stack.selected_mask == Some(mask_index)
            && stack
                .selected_component
                .is_some_and(|index| index >= mask.components.len())
        {
            return invalid("selected mask component index is out of range");
        }
        for component in &mask.components {
            if component.name.len() > MAX_EDIT_NAME_BYTES {
                return invalid("mask component name is unreasonably long");
            }
            if !geometry_matches_kind(component.kind, &component.geometry) {
                return invalid("mask component kind and geometry do not agree");
            }
            match &component.geometry {
                MaskGeometry::Brush {
                    size,
                    feather,
                    opacity,
                    stroke_starts,
                    dabs,
                    ..
                } => {
                    finite("brush geometry", &[*size, *feather, *opacity])?;
                    bounded("brush size", *size, 0.0, 16.0)?;
                    bounded("brush feather", *feather, 0.0, 1.0)?;
                    bounded("brush opacity", *opacity, 0.0, 1.0)?;
                    if dabs.len() > MAX_BRUSH_DABS {
                        return invalid("brush mask contains too many dabs");
                    }
                    let mut previous_start = None;
                    for &start in stroke_starts {
                        if start >= dabs.len()
                            || previous_start.is_some_and(|previous| start <= previous)
                        {
                            return invalid("brush mask contains invalid stroke boundaries");
                        }
                        previous_start = Some(start);
                    }
                    for dab in dabs {
                        finite(
                            "brush dab",
                            &[
                                dab.center[0],
                                dab.center[1],
                                dab.opacity,
                                dab.size,
                                dab.feather,
                            ],
                        )?;
                        bounded("brush dab x", dab.center[0], -16.0, 16.0)?;
                        bounded("brush dab y", dab.center[1], -16.0, 16.0)?;
                        bounded("brush dab opacity", dab.opacity, -1.0, 1.0)?;
                        bounded("brush dab size", dab.size, 0.0, 16.0)?;
                        bounded("brush dab feather", dab.feather, 0.0, 1.0)?;
                    }
                }
                MaskGeometry::Radial {
                    center,
                    radius,
                    rotation,
                    feather,
                    ..
                } => {
                    finite(
                        "radial geometry",
                        &[
                            center[0], center[1], radius[0], radius[1], *rotation, *feather,
                        ],
                    )?;
                    for value in center {
                        bounded("radial center", *value, -16.0, 16.0)?;
                    }
                    for value in radius {
                        bounded("radial radius", *value, 0.0, 16.0)?;
                    }
                    bounded("radial rotation", *rotation, -1_000_000.0, 1_000_000.0)?;
                    bounded("radial feather", *feather, 0.0, 1.0)?;
                }
                MaskGeometry::Linear {
                    start,
                    end,
                    feather,
                    ..
                } => {
                    finite(
                        "linear geometry",
                        &[start[0], start[1], end[0], end[1], *feather],
                    )?;
                    for value in start.iter().chain(end.iter()) {
                        bounded("linear point", *value, -16.0, 16.0)?;
                    }
                    bounded("linear feather", *feather, 0.0, 16.0)?;
                }
                MaskGeometry::Ai {
                    mask,
                    grow,
                    feather,
                } => {
                    finite("AI mask settings", &[*grow, *feather])?;
                    bounded("AI mask grow", *grow, -1.0, 1.0)?;
                    bounded("AI mask feather", *feather, 0.0, 1.0)?;
                    if let Some(image) = mask {
                        validate_image(image.width, image.height, image.pixels.len(), 1)?;
                    }
                }
                MaskGeometry::Landscape {
                    mask,
                    grow,
                    feather,
                    ..
                } => {
                    finite("landscape mask settings", &[*grow, *feather])?;
                    bounded("landscape mask grow", *grow, -1.0, 1.0)?;
                    bounded("landscape mask feather", *feather, 0.0, 1.0)?;
                    if let Some(image) = mask {
                        validate_image(image.width, image.height, image.pixels.len(), 1)?;
                    }
                }
                MaskGeometry::Object {
                    mask,
                    grow,
                    feather,
                    brush_size,
                    edge_refine,
                    strokes,
                    ..
                } => {
                    finite(
                        "object mask settings",
                        &[*grow, *feather, *brush_size, *edge_refine],
                    )?;
                    bounded("object mask grow", *grow, -1.0, 1.0)?;
                    bounded("object mask feather", *feather, 0.0, 1.0)?;
                    bounded("object brush size", *brush_size, 0.0, 16.0)?;
                    bounded("object edge refine", *edge_refine, 0.0, 1.0)?;
                    if strokes.len() > MAX_OBJECT_STROKES {
                        return invalid("object mask contains too many strokes");
                    }
                    let mut point_count = 0usize;
                    for stroke in strokes {
                        point_count =
                            point_count
                                .checked_add(stroke.points.len())
                                .ok_or_else(|| {
                                    SidecarError::Invalid("object prompt count overflow".to_owned())
                                })?;
                        if point_count > MAX_OBJECT_STROKE_POINTS {
                            return invalid("object mask contains too many prompt points");
                        }
                        for point in &stroke.points {
                            finite("object prompt", point)?;
                            bounded("object prompt x", point[0], -16.0, 16.0)?;
                            bounded("object prompt y", point[1], -16.0, 16.0)?;
                        }
                    }
                    if let Some(image) = mask {
                        validate_image(image.width, image.height, image.pixels.len(), 1)?;
                    }
                }
                MaskGeometry::LuminanceRange {
                    source,
                    low,
                    high,
                    grow,
                    feather,
                } => {
                    finite("luminance range mask", &[*low, *high, *grow, *feather])?;
                    bounded("luminance low", *low, -16.0, 16.0)?;
                    bounded("luminance high", *high, -16.0, 16.0)?;
                    bounded("luminance grow", *grow, -1.0, 1.0)?;
                    bounded("luminance feather", *feather, 0.0, 16.0)?;
                    if let Some(image) = source {
                        validate_image(image.width, image.height, image.rgba.len(), 4)?;
                    }
                }
                MaskGeometry::ColorRange {
                    source,
                    sample,
                    tolerance,
                    grow,
                    feather,
                    ..
                } => {
                    finite(
                        "color range mask",
                        &[sample[0], sample[1], sample[2], *tolerance, *grow, *feather],
                    )?;
                    for value in sample {
                        bounded("color sample", *value, -16.0, 16.0)?;
                    }
                    bounded("color tolerance", *tolerance, 0.0, 16.0)?;
                    bounded("color grow", *grow, -1.0, 1.0)?;
                    bounded("color feather", *feather, 0.0, 16.0)?;
                    if let Some(image) = source {
                        validate_image(image.width, image.height, image.rgba.len(), 4)?;
                    }
                }
                _ => {}
            }
        }
    }
    if edits.inpainting.len() > MAX_INPAINT_STROKES {
        return invalid("sidecar contains too many inpainting strokes");
    }
    let mut inpaint_dabs = 0usize;
    for stroke in edits.inpainting.iter() {
        inpaint_dabs = inpaint_dabs
            .checked_add(stroke.dabs.len())
            .ok_or_else(|| SidecarError::Invalid("inpainting dab count overflow".to_owned()))?;
        if inpaint_dabs > MAX_INPAINT_DABS {
            return invalid("sidecar contains too many inpainting brush dabs");
        }
        for dab in &stroke.dabs {
            finite(
                "inpainting brush dab",
                &[
                    dab.center[0],
                    dab.center[1],
                    dab.opacity,
                    dab.size,
                    dab.feather,
                ],
            )?;
            bounded("inpainting dab x", dab.center[0], -16.0, 16.0)?;
            bounded("inpainting dab y", dab.center[1], -16.0, 16.0)?;
            bounded("inpainting dab opacity", dab.opacity, -1.0, 1.0)?;
            bounded("inpainting dab size", dab.size, 0.0, 16.0)?;
            bounded("inpainting dab feather", dab.feather, 0.0, 1.0)?;
        }

        let patch = &stroke.patch;
        if patch.source_width == 0
            || patch.source_height == 0
            || patch.width == 0
            || patch.height == 0
            || patch
                .x
                .checked_add(patch.width)
                .is_none_or(|right| right > patch.source_width)
            || patch
                .y
                .checked_add(patch.height)
                .is_none_or(|bottom| bottom > patch.source_height)
        {
            return invalid("inpainting patch bounds are invalid");
        }
        if !patch.is_valid() {
            return invalid("inpainting patch storage is invalid");
        }
        let [raster_width, raster_height] = patch.raster_dimensions();
        validate_image(raster_width, raster_height, patch.mask.len(), 1)?;
        let pixels = raster_width as usize * raster_height as usize;
        if !patch.rgba16f.is_empty() {
            if patch.rgba16f.len() != pixels.saturating_mul(4) {
                return invalid("inpainting RGBA16F patch dimensions are invalid");
            }
        } else {
            validate_image(raster_width, raster_height, patch.rgba.len(), 4)?;
        }
    }

    if stack.selected_mask.is_none() && stack.selected_component.is_some() {
        return invalid("a component is selected without a selected mask");
    }
    Ok(())
}

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

/// Rejects an inpainting result before it becomes visible state when adding it
/// would make the edit impossible to persist on the current platform. This is
/// intentionally a compression-independent upper bound: generated masks are
/// content-deduplicated, then measured from their lengths instead of being PNG-
/// compressed or Base64-encoded on the UI thread.
pub fn preflight_inpaint_addition(
    masks: &MaskStack,
    existing: &[InpaintStroke],
    candidate: &InpaintStroke,
) -> Result<(), SidecarError> {
    preflight_inpaint_addition_with_limit(masks, existing, candidate, MAX_SIDECAR_BYTES)
}

/// Rejects a mask copy/duplicate before it becomes live state if even the
/// conservative deduplicated asset bound could not fit beside current inpainting data.
pub fn preflight_mask_change(
    masks: &MaskStack,
    inpainting: &[InpaintStroke],
) -> Result<(), SidecarError> {
    let estimated = estimate_sidecar_bytes(masks, inpainting)?;
    enforce_size_limit(estimated, MAX_SIDECAR_BYTES)
}

fn preflight_inpaint_addition_with_limit(
    masks: &MaskStack,
    existing: &[InpaintStroke],
    candidate: &InpaintStroke,
    limit: u64,
) -> Result<(), SidecarError> {
    let estimated =
        estimate_sidecar_bytes(masks, existing.iter().chain(std::iter::once(candidate)))?;
    enforce_size_limit(estimated, limit)
}

fn enforce_size_limit(estimated: u64, limit: u64) -> Result<(), SidecarError> {
    if estimated > limit {
        Err(SidecarError::TooLarge(estimated))
    } else {
        Ok(())
    }
}

fn estimate_sidecar_bytes<'a>(
    masks: &MaskStack,
    inpainting: impl IntoIterator<Item = &'a InpaintStroke>,
) -> Result<u64, SidecarError> {
    // This covers the bounded camera-profile/lens strings, the complete global
    // adjustment structure, and document-level JSON punctuation. Dynamic mask
    // names, geometry, and inpainting data are counted separately below.
    const DOCUMENT_HEADROOM: u64 = 1024 * 1024;
    const MASK_HEADROOM: u64 = 16 * 1024;
    const COMPONENT_HEADROOM: u64 = 2 * 1024;
    const INPAINT_STROKE_HEADROOM: u64 = 512;
    const BRUSH_DAB_HEADROOM: u64 = 256;
    const OBJECT_STROKE_HEADROOM: u64 = 128;
    const OBJECT_POINT_HEADROOM: u64 = 96;
    // PNG adds scanline filters, chunks, and DEFLATE framing. The real schema-5
    // encoder normally makes masks much smaller, but preflight deliberately uses
    // a compression-independent upper bound so an accepted edit is always savable.
    const MASK_PNG_FIXED_HEADROOM: u64 = 64 * 1024;

    let mut estimated = DOCUMENT_HEADROOM;
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
                }
                | MaskGeometry::Landscape {
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

    for stroke in inpainting {
        checked_add(&mut estimated, INPAINT_STROKE_HEADROOM)?;
        checked_add_scaled(&mut estimated, stroke.dabs.len(), BRUSH_DAB_HEADROOM)?;
        if !stroke.patch.rgba16f.is_empty() {
            let byte_count = stroke
                .patch
                .rgba16f
                .len()
                .checked_mul(2)
                .ok_or(SidecarError::TooLarge(u64::MAX))?;
            checked_add(&mut estimated, base64_json_string_bytes(byte_count)?)?;
        }
        if !stroke.patch.rgba.is_empty() {
            checked_add(
                &mut estimated,
                base64_json_string_bytes(stroke.patch.rgba.len())?,
            )?;
        }
        checked_add(
            &mut estimated,
            base64_json_string_bytes(stroke.patch.mask.len())?,
        )?;
    }
    Ok(estimated)
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

fn geometry_matches_kind(kind: MaskKind, geometry: &MaskGeometry) -> bool {
    matches!(
        (kind, geometry),
        (MaskKind::Brush, MaskGeometry::Brush { .. })
            | (MaskKind::Radial, MaskGeometry::Radial { .. })
            | (MaskKind::Linear, MaskGeometry::Linear { .. })
            | (
                MaskKind::Subject | MaskKind::Background,
                MaskGeometry::Ai { .. }
            )
            | (MaskKind::Object, MaskGeometry::Object { .. })
            | (MaskKind::Landscape, MaskGeometry::Landscape { .. })
            | (
                MaskKind::LuminanceRange,
                MaskGeometry::LuminanceRange { .. }
            )
            | (MaskKind::ColorRange, MaskGeometry::ColorRange { .. })
            | (MaskKind::DepthRange, MaskGeometry::Placeholder)
    )
}

fn validate_exposure(exposure: &ExposureParams) -> Result<(), SidecarError> {
    finite(
        "global adjustment",
        &[
            exposure.black_point,
            exposure.exposure,
            exposure.contrast,
            exposure.temperature,
            exposure.tint,
            exposure.saturation,
            exposure.vibrance,
            exposure.chroma_denoise,
            exposure.luminance_denoise,
            exposure.denoise_detail,
            exposure.dual_threshold,
            exposure.frequency_chroma,
            exposure.ca_red,
            exposure.ca_blue,
            exposure.highlight_clip,
            exposure.highlight_reconstruction,
            exposure.highlights,
            exposure.shadows,
            exposure.whites,
            exposure.blacks,
            exposure.texture,
            exposure.clarity,
            exposure.dehaze,
            exposure.sharpen_amount,
            exposure.sharpen_radius,
            exposure.sharpen_detail,
            exposure.sharpen_masking,
            exposure.glow_amount,
            exposure.glow_radius,
            exposure.glow_threshold,
            exposure.vignette_amount,
            exposure.vignette_midpoint,
            exposure.vignette_roundness,
            exposure.vignette_feather,
            exposure.vignette_highlights,
            exposure.sigmoid.contrast,
            exposure.sigmoid.skew,
            exposure.sigmoid.display_white_target,
            exposure.sigmoid.display_black_target,
            exposure.sigmoid.hue_preservation,
        ],
    )?;
    finite("global HSL hue", &exposure.hsl_hue)?;
    finite("global HSL saturation", &exposure.hsl_saturation)?;
    finite("global HSL luminance", &exposure.hsl_luminance)?;
    validate_curves(
        &[
            &exposure.tone_curve,
            &exposure.tone_curve_red,
            &exposure.tone_curve_green,
            &exposure.tone_curve_blue,
        ],
        "global tone curve",
    )?;
    validate_grading(&exposure.color_grading, "global color grading")
}

fn validate_local_adjustments(
    adjustments: &crate::pipeline::LocalAdjustments,
) -> Result<(), SidecarError> {
    finite(
        "local adjustment",
        &[
            adjustments.exposure,
            adjustments.contrast,
            adjustments.highlights,
            adjustments.shadows,
            adjustments.whites,
            adjustments.blacks,
            adjustments.temperature,
            adjustments.tint,
            adjustments.saturation,
            adjustments.texture,
            adjustments.clarity,
            adjustments.dehaze,
        ],
    )?;
    finite("local HSL hue", &adjustments.hsl_hue)?;
    finite("local HSL saturation", &adjustments.hsl_saturation)?;
    finite("local HSL luminance", &adjustments.hsl_luminance)?;
    validate_curves(
        &[
            &adjustments.tone_curve,
            &adjustments.tone_curve_red,
            &adjustments.tone_curve_green,
            &adjustments.tone_curve_blue,
        ],
        "local tone curve",
    )?;
    validate_grading(&adjustments.color_grading, "local color grading")
}

fn validate_curves(
    curves: &[&crate::pipeline::PointCurve],
    label: &str,
) -> Result<(), SidecarError> {
    for curve in curves {
        if !(2..=crate::pipeline::MAX_POINT_CURVE_POINTS as u32).contains(&curve.len) {
            return invalid("tone curve point count is invalid");
        }
        for point in curve.points {
            finite(label, &point)?;
        }
    }
    Ok(())
}

fn validate_grading(
    grading: &crate::pipeline::ColorGrading,
    label: &str,
) -> Result<(), SidecarError> {
    finite(
        label,
        &[
            grading.shadows.hue,
            grading.shadows.saturation,
            grading.shadows.luminance,
            grading.midtones.hue,
            grading.midtones.saturation,
            grading.midtones.luminance,
            grading.highlights.hue,
            grading.highlights.saturation,
            grading.highlights.luminance,
            grading.global.hue,
            grading.global.saturation,
            grading.global.luminance,
            grading.blending,
            grading.balance,
        ],
    )
}

fn finite(label: &str, values: &[f32]) -> Result<(), SidecarError> {
    if values.iter().all(|value| value.is_finite()) {
        Ok(())
    } else {
        invalid(&format!("{label} contains a non-finite value"))
    }
}

fn bounded(label: &str, value: f32, minimum: f32, maximum: f32) -> Result<(), SidecarError> {
    if (minimum..=maximum).contains(&value) {
        Ok(())
    } else {
        invalid(&format!("{label} is outside the safe range"))
    }
}

fn validate_image(
    width: u32,
    height: u32,
    bytes: usize,
    channels: usize,
) -> Result<(), SidecarError> {
    if width == 0 || height == 0 || width > MAX_MASK_IMAGE_EDGE || height > MAX_MASK_IMAGE_EDGE {
        return invalid("mask image dimensions are invalid");
    }
    let expected = (width as usize)
        .checked_mul(height as usize)
        .and_then(|pixels| pixels.checked_mul(channels))
        .ok_or_else(|| SidecarError::Invalid("mask image dimensions overflow".to_owned()))?;
    if bytes != expected {
        return invalid("mask image byte count does not match its dimensions");
    }
    Ok(())
}

fn invalid<T>(message: &str) -> Result<T, SidecarError> {
    Err(SidecarError::Invalid(message.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::MaskKind;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn sample_edits() -> EditState {
        let mut exposure = ExposureParams::scene_referred_default();
        exposure.dehaze = 27.0;
        let mut masks = MaskStack::default();
        masks.add_mask(MaskKind::Radial);
        EditState {
            exposure,
            geometry: GeometryTransform::default(),
            camera_profile: None,
            masks: Arc::new(masks),
            inpainting: Arc::new(Vec::new()),
            lens: LensEditState {
                enabled: true,
                maker: "Test Optics".to_owned(),
                model: "35 mm f/2".to_owned(),
            },
            ai_masks_need_update: false,
        }
    }

    #[test]
    fn copied_adjustments_respect_category_settings_and_mark_ai_masks_stale() {
        let mut source = sample_edits();
        source.exposure.dehaze = 61.0;
        source.lens.enabled = false;
        let mut source_masks = MaskStack::default();
        source_masks.add_mask(MaskKind::Subject);
        if let MaskGeometry::Ai { mask, .. } = &mut source_masks.masks[0].components[0].geometry {
            *mask = Some(crate::pipeline::MaskImage::new(2, 2, vec![0, 64, 192, 255]).unwrap());
        }
        source.masks = Arc::new(source_masks);

        let mut destination = sample_edits();
        destination.exposure.dehaze = 7.0;
        let original_exposure = destination.exposure;
        let original_lens = destination.lens.clone();

        apply_copied_adjustments(
            &mut destination,
            &source,
            AdjustmentCopySettings {
                adjustments: false,
                geometry: false,
                camera_profile: false,
                masks: false,
                ai_masks: true,
                inpainting: false,
                lens_correction: false,
            },
        );

        assert_eq!(destination.exposure, original_exposure);
        assert_eq!(destination.lens, original_lens);
        // Merge mode preserves the destination's manual radial mask while
        // replacing only the selected AI category.
        assert_eq!(destination.masks.masks.len(), 2);
        assert_eq!(
            destination.masks.masks[1].components[0].kind,
            MaskKind::Subject
        );
        assert!(destination.ai_masks_need_update);
    }

    #[test]
    fn copied_uncached_ai_masks_are_still_marked_stale() {
        let mut source = default_edit_state();
        let mut source_masks = MaskStack::default();
        source_masks.add_mask(MaskKind::Subject);
        // A generated bitmap is a cache. Pasted AI masks may not include one,
        // but their semantic component still has to be regenerated for the
        // destination image.
        assert!(matches!(
            &source_masks.masks[0].components[0].geometry,
            MaskGeometry::Ai { mask: None, .. }
        ));
        source.masks = Arc::new(source_masks);

        let mut destination = default_edit_state();
        apply_copied_adjustments(
            &mut destination,
            &source,
            AdjustmentCopySettings {
                adjustments: false,
                geometry: false,
                camera_profile: false,
                masks: false,
                ai_masks: true,
                inpainting: false,
                lens_correction: false,
            },
        );

        assert!(destination.ai_masks_need_update);
    }

    #[test]
    fn manual_and_ai_masks_can_be_copied_independently() {
        let mut source = sample_edits();
        let mut source_masks = MaskStack::default();
        source_masks.add_mask(MaskKind::Brush);
        source_masks.add_mask(MaskKind::Subject);
        if let MaskGeometry::Ai { mask, .. } = &mut source_masks.masks[1].components[0].geometry {
            *mask = Some(crate::pipeline::MaskImage::new(2, 2, vec![0, 64, 192, 255]).unwrap());
        }
        source.masks = Arc::new(source_masks);

        let mut destination = default_edit_state();
        apply_copied_adjustments(
            &mut destination,
            &source,
            AdjustmentCopySettings {
                adjustments: false,
                geometry: false,
                camera_profile: false,
                masks: true,
                ai_masks: false,
                inpainting: false,
                lens_correction: false,
            },
        );

        assert_eq!(destination.masks.masks.len(), 1);
        assert_eq!(
            destination.masks.masks[0].components[0].kind,
            MaskKind::Brush
        );
        assert!(!destination.ai_masks_need_update);

        apply_copied_adjustments(
            &mut destination,
            &source,
            AdjustmentCopySettings {
                adjustments: false,
                geometry: false,
                camera_profile: false,
                masks: false,
                ai_masks: true,
                inpainting: false,
                lens_correction: false,
            },
        );

        assert_eq!(destination.masks.masks.len(), 2);
        assert!(destination
            .masks
            .masks
            .iter()
            .any(|mask| mask.components[0].kind == MaskKind::Brush));
        assert!(destination
            .masks
            .masks
            .iter()
            .any(|mask| mask.components[0].kind == MaskKind::Subject));
        assert!(destination.ai_masks_need_update);
    }

    #[test]
    fn mixed_mask_groups_do_not_copy_disabled_manual_components() {
        let mut source = default_edit_state();
        let mut masks = MaskStack::default();
        masks.add_mask(MaskKind::Brush);
        masks.add_component(MaskKind::Subject, crate::pipeline::MaskCombineMode::Add);
        source.masks = Arc::new(masks);

        let mut destination = default_edit_state();
        apply_copied_adjustments(
            &mut destination,
            &source,
            AdjustmentCopySettings {
                adjustments: false,
                geometry: false,
                camera_profile: false,
                masks: false,
                ai_masks: true,
                inpainting: false,
                lens_correction: false,
            },
        );

        assert_eq!(destination.masks.masks.len(), 1);
        assert_eq!(destination.masks.masks[0].components.len(), 1);
        assert_eq!(
            destination.masks.masks[0].components[0].kind,
            MaskKind::Subject
        );
        assert!(destination.ai_masks_need_update);

        apply_copied_adjustments(
            &mut destination,
            &source,
            AdjustmentCopySettings {
                adjustments: false,
                geometry: false,
                camera_profile: false,
                masks: true,
                ai_masks: false,
                inpainting: false,
                lens_correction: false,
            },
        );

        assert!(destination
            .masks
            .masks
            .iter()
            .any(|mask| mask.components.len() == 1 && mask.components[0].kind == MaskKind::Brush));
        assert!(destination
            .masks
            .masks
            .iter()
            .all(|mask| mask.components.len() == 1));
    }

    #[test]
    fn copied_adjustments_include_camera_profile_and_replace_clears_other_categories() {
        let mut source = sample_edits();
        source.camera_profile = Some(PathBuf::from("Adobe/Camera Standard.dcp"));
        source.exposure.dehaze = 48.0;

        let mut destination = sample_edits();

        apply_copied_adjustments_with_mode(
            &mut destination,
            &source,
            AdjustmentCopySettings {
                adjustments: true,
                geometry: false,
                camera_profile: true,
                masks: false,
                ai_masks: false,
                inpainting: false,
                lens_correction: false,
            },
            AdjustmentPasteMode::Replace,
        );

        assert_eq!(destination.exposure.dehaze, 48.0);
        assert_eq!(
            destination.camera_profile,
            Some(PathBuf::from("Adobe/Camera Standard.dcp"))
        );
        assert!(destination.masks.masks.is_empty());
        assert!(destination.inpainting.is_empty());
        assert_eq!(destination.lens, LensEditState::default());
    }

    #[test]
    fn stale_ai_mask_metadata_alone_is_not_an_edit_conflict() {
        let mut edits = default_edit_state();
        edits.ai_masks_need_update = true;
        assert!(!edit_state_has_adjustments(&edits));
    }

    #[test]
    fn legacy_copy_settings_use_safe_category_defaults() {
        let settings: AdjustmentCopySettings = serde_json::from_str("{}").unwrap();
        assert_eq!(settings, AdjustmentCopySettings::default());
    }

    fn temporary_directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "auraw-sidecar-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn sidecar_round_trip_preserves_edit_state() {
        let edits = sample_edits();
        let encoded = encode(edits.clone()).unwrap();
        let loaded = decode(&encoded).unwrap();
        assert_eq!(loaded.edits, edits);
        assert!(!loaded.migrated);
    }

    #[test]
    fn generated_masks_are_deduplicated_compressed_and_copy_on_write_after_loading() {
        let mut edits = sample_edits();
        let mut masks = MaskStack::default();
        masks.add_mask(MaskKind::Object);
        let pixels = (0..64 * 64)
            .map(|index| if index % 17 < 8 { 255 } else { 0 })
            .collect::<Vec<_>>();
        if let MaskGeometry::Object { mask, .. } = &mut masks.masks[0].components[0].geometry {
            *mask = MaskImage::new(64, 64, pixels);
        }
        masks.masks.push(masks.masks[0].clone());
        edits.masks = Arc::new(masks);

        let encoded = encode(edits.clone()).unwrap();
        let document: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(document["schema_version"], SIDECAR_SCHEMA_VERSION);
        assert_eq!(document["mask_assets"].as_array().unwrap().len(), 1);
        assert_eq!(document["mask_asset_refs"].as_array().unwrap().len(), 2);
        let compressed = document["mask_assets"][0]["png"].as_str().unwrap();
        assert!(compressed.len() < base64_json_string_bytes(64 * 64).unwrap() as usize);
        assert!(document
            .pointer("/edits/masks/masks/0/components/0/geometry/Object/mask")
            .unwrap()
            .is_null());

        let loaded = decode(&encoded).unwrap();
        assert_eq!(loaded.edits, edits);
        let mut restored = loaded.edits;
        let restored_masks = Arc::make_mut(&mut restored.masks);
        let [first_group, second_group, ..] = restored_masks.masks.as_mut_slice() else {
            panic!("expected duplicated object-mask groups");
        };
        let MaskGeometry::Object {
            mask: Some(first), ..
        } = &mut first_group.components[0].geometry
        else {
            panic!("first object mask was not restored");
        };
        let MaskGeometry::Object {
            mask: Some(second), ..
        } = &mut second_group.components[0].geometry
        else {
            panic!("second object mask was not restored");
        };
        assert!(Arc::ptr_eq(&first.pixels, &second.pixels));
        let unchanged = second.pixels[0];
        Arc::make_mut(&mut first.pixels)[0] = unchanged ^ 0xff;
        assert_eq!(second.pixels[0], unchanged);
        assert!(!Arc::ptr_eq(&first.pixels, &second.pixels));
    }

    #[test]
    fn beta_inline_mask_sidecar_migrates_to_asset_layout_without_losing_masks() {
        let mut edits = sample_edits();
        let masks = Arc::make_mut(&mut edits.masks);
        masks.add_mask(MaskKind::Subject);
        if let MaskGeometry::Ai { mask, .. } = &mut masks.masks[1].components[0].geometry {
            *mask = MaskImage::new(8, 8, vec![127; 8 * 8]);
        }
        let legacy = SidecarDocument {
            format: SIDECAR_FORMAT.to_owned(),
            schema_version: 4,
            edits: edits.clone(),
            mask_assets: Vec::new(),
            mask_asset_refs: Vec::new(),
        };
        let legacy_bytes = serde_json::to_vec(&legacy).unwrap();
        assert!(legacy_bytes
            .windows(b"\"pixels\"".len())
            .any(|part| part == b"\"pixels\""));

        let loaded = decode(&legacy_bytes).unwrap();
        assert!(loaded.migrated);
        assert_eq!(loaded.edits, edits);

        let rewritten = encode(loaded.edits).unwrap();
        let current: serde_json::Value = serde_json::from_slice(&rewritten).unwrap();
        assert_eq!(current["schema_version"], SIDECAR_SCHEMA_VERSION);
        assert_eq!(current["mask_assets"].as_array().unwrap().len(), 1);
        assert_eq!(current["mask_asset_refs"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn malformed_current_mask_assets_are_rejected() {
        let mut edits = sample_edits();
        let masks = Arc::make_mut(&mut edits.masks);
        masks.add_mask(MaskKind::Subject);
        if let MaskGeometry::Ai { mask, .. } = &mut masks.masks[1].components[0].geometry {
            *mask = MaskImage::new(8, 8, vec![255; 8 * 8]);
        }
        let encoded = encode(edits).unwrap();

        let mut invalid_reference: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        invalid_reference["mask_asset_refs"][0]["asset_index"] = 99.into();
        assert!(matches!(
            decode(&serde_json::to_vec(&invalid_reference).unwrap()),
            Err(SidecarError::Invalid(message)) if message.contains("asset index")
        ));

        let mut invalid_png: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        invalid_png["mask_assets"][0]["png"] = "AAAA".into();
        assert!(matches!(
            decode(&serde_json::to_vec(&invalid_png).unwrap()),
            Err(SidecarError::Invalid(message)) if message.contains("mask PNG")
        ));
    }

    #[test]
    fn reset_all_adjustments_removes_sidecar_masks_and_thumbnail_caches() {
        let directory = temporary_directory("reset-all");
        let raw = directory.join("masked.CR3");
        fs::write(&raw, b"raw").unwrap();

        let mut edits = sample_edits();
        let masks = Arc::make_mut(&mut edits.masks);
        masks.add_mask(MaskKind::Subject).unwrap();
        edits.ai_masks_need_update = true;
        save_desktop(&raw, edits).unwrap();

        let cache_paths = [
            developed_thumbnail_path_for_raw(&raw),
            developed_thumbnail_fingerprint_path_for_raw(&raw),
            legacy_developed_thumbnail_path_for_raw(&raw),
            legacy_developed_thumbnail_fingerprint_path_for_raw(&raw),
        ];
        for path in &cache_paths {
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"stale").unwrap();
        }

        assert!(reset_desktop_adjustments(&raw).unwrap());
        assert!(!sidecar_path_for_raw(&raw).exists());
        assert!(cache_paths.iter().all(|path| !path.exists()));
        assert!(load_desktop(&raw).unwrap().is_none());

        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn inpainting_round_trip_preserves_individual_strokes() {
        use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke};
        use half::f16;

        let mut edits = sample_edits();
        let rgba16f = vec![f16::from_f32(0.25).to_bits(); 4];
        let patch =
            InpaintPatch::new_linear_resampled([4, 4], [1, 1], [2, 2], [1, 1], rgba16f, vec![255])
                .unwrap();
        let stroke = InpaintStroke::from_result(vec![BrushDab::default()], patch).unwrap();
        edits.inpainting = Arc::new(vec![stroke]);

        let encoded = encode(edits.clone()).unwrap();
        let loaded = decode(&encoded).unwrap();
        assert_eq!(loaded.edits.inpainting, edits.inpainting);
    }

    #[test]
    fn prospective_inpaint_budget_counts_existing_persisted_payloads() {
        use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke, MaskImage};

        fn stroke(edge: u32, value: u16) -> InpaintStroke {
            let pixels = edge as usize * edge as usize;
            let patch = InpaintPatch::new_linear(
                edge + 2,
                edge + 2,
                1,
                1,
                edge,
                edge,
                vec![value; pixels * 4],
                vec![255; pixels],
            )
            .unwrap();
            InpaintStroke::from_result(vec![BrushDab::default(); 3], patch).unwrap()
        }

        let mut masks = MaskStack::default();
        masks.add_mask(MaskKind::Subject);
        masks.masks[0].name = "subject \"mask\"".to_owned();
        if let MaskGeometry::Ai { mask, .. } = &mut masks.masks[0].components[0].geometry {
            *mask = MaskImage::new(32, 32, vec![127; 32 * 32]);
        } else {
            panic!("subject mask should use AI geometry");
        }

        let existing = stroke(16, 1);
        let candidate = stroke(8, 2);
        let candidate_only =
            estimate_sidecar_bytes(&MaskStack::default(), std::iter::once(&candidate)).unwrap();
        let prospective = estimate_sidecar_bytes(&masks, [&existing, &candidate]).unwrap();
        assert!(prospective > candidate_only);

        assert!(preflight_inpaint_addition_with_limit(
            &MaskStack::default(),
            &[],
            &candidate,
            prospective - 1,
        )
        .is_ok());
        assert!(matches!(
            preflight_inpaint_addition_with_limit(
                &masks,
                std::slice::from_ref(&existing),
                &candidate,
                prospective - 1,
            ),
            Err(SidecarError::TooLarge(bytes)) if bytes == prospective
        ));

        let mut edits = sample_edits();
        edits.masks = Arc::new(masks);
        edits.inpainting = Arc::new(vec![existing, candidate]);
        let encoded = encode(edits).unwrap();
        assert!((encoded.len() as u64) <= prospective);
    }

    #[test]
    fn native_resolution_patches_round_trip_as_sequential_android_strokes() {
        use crate::pipeline::{BrushDab, InpaintPatch, InpaintStroke};

        let raster_pixels = 512usize * 512;
        let patch = InpaintPatch::new_linear_resampled(
            [6000, 4000],
            [500, 500],
            [1600, 1600],
            [512, 512],
            vec![0u16; raster_pixels * 4],
            vec![255; raster_pixels],
        )
        .unwrap();
        let stroke = InpaintStroke::from_result(vec![BrushDab::default()], patch).unwrap();
        let android_limit = 32 * 1024 * 1024;
        let mut strokes = Vec::new();
        for index in 0..8 {
            let mut candidate = stroke.clone();
            candidate.patch.x += index * 10;
            candidate.dabs[0].center[0] = index as f32 / 8.0;
            preflight_inpaint_addition_with_limit(
                &MaskStack::default(),
                &strokes,
                &candidate,
                android_limit,
            )
            .unwrap();
            strokes.push(candidate);
        }

        let mut edits = sample_edits();
        edits.inpainting = Arc::new(strokes.clone());
        let encoded = encode(edits).unwrap();
        assert!((encoded.len() as u64) <= android_limit);
        let decoded = decode(&encoded).unwrap();
        assert_eq!(decoded.edits.inpainting.as_ref(), strokes.as_slice());
    }

    #[test]
    fn schema_one_sidecar_without_inpainting_loads_as_empty() {
        let document = SidecarDocument {
            format: SIDECAR_FORMAT.to_owned(),
            schema_version: 1,
            edits: sample_edits(),
            mask_assets: Vec::new(),
            mask_asset_refs: Vec::new(),
        };
        let mut value = serde_json::to_value(document).unwrap();
        value["edits"].as_object_mut().unwrap().remove("inpainting");
        let encoded = serde_json::to_vec(&value).unwrap();
        let loaded = decode(&encoded).unwrap();
        assert!(loaded.edits.inpainting.is_empty());
        assert!(loaded.migrated);
    }

    #[test]
    fn schema_two_full_resolution_inpaint_patch_remains_compatible() {
        use crate::pipeline::{InpaintPatch, InpaintStroke};

        let mut edits = sample_edits();
        let patch =
            InpaintPatch::new_linear(4, 4, 1, 1, 2, 2, vec![0u16; 16], vec![255; 4]).unwrap();
        edits.inpainting = Arc::new(vec![InpaintStroke::from_result(Vec::new(), patch).unwrap()]);
        let encoded = serde_json::to_vec(&SidecarDocument {
            format: SIDECAR_FORMAT.to_owned(),
            schema_version: 2,
            edits,
            mask_assets: Vec::new(),
            mask_asset_refs: Vec::new(),
        })
        .unwrap();
        let loaded = decode(&encoded).unwrap();
        assert_eq!(loaded.edits.inpainting[0].patch.raster_dimensions(), [2, 2]);
        assert!(loaded.migrated);
    }

    #[test]
    fn corrupt_and_future_sidecars_are_rejected() {
        assert!(matches!(
            decode(br#"{"schema_version":1,"#),
            Err(SidecarError::Invalid(_))
        ));

        let edits = sample_edits();
        let future = SidecarDocument {
            format: SIDECAR_FORMAT.to_owned(),
            schema_version: SIDECAR_SCHEMA_VERSION + 1,
            edits,
            mask_assets: Vec::new(),
            mask_asset_refs: Vec::new(),
        };
        assert!(matches!(
            decode(&serde_json::to_vec(&future).unwrap()),
            Err(SidecarError::Unsupported(_))
        ));

        let mut non_finite = sample_edits();
        non_finite.exposure.exposure = f32::NAN;
        assert!(matches!(
            encode(non_finite),
            Err(SidecarError::Invalid(message)) if message.contains("non-finite")
        ));

        let mut unsafe_geometry = sample_edits();
        if let MaskGeometry::Radial { radius, .. } =
            &mut Arc::make_mut(&mut unsafe_geometry.masks).masks[0].components[0].geometry
        {
            radius[0] = 1.0e30;
        }
        assert!(matches!(
            encode(unsafe_geometry),
            Err(SidecarError::Invalid(message)) if message.contains("safe range")
        ));
    }

    #[test]
    fn desktop_save_is_atomic_and_uses_appended_suffix() {
        let directory = temporary_directory("atomic");
        let raw = directory.join("photo.CR3");
        fs::write(&raw, b"raw").unwrap();
        let edits = sample_edits();
        let path = save_desktop(&raw, edits.clone()).unwrap();
        assert_eq!(path.file_name().unwrap(), "photo.CR3.auraw");
        assert_eq!(load_desktop(&raw).unwrap().unwrap().edits, edits);
        assert_eq!(
            fs::read_dir(&directory)
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
                .count(),
            0
        );
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn reconstructible_range_source_is_not_persisted() {
        use crate::pipeline::{MaskCombineMode, MaskComponent, MaskRgbImage};

        let mut edits = sample_edits();
        let width = 2048;
        let height = 2048;
        let source = MaskRgbImage::new(
            width,
            height,
            vec![127; width as usize * height as usize * 4],
        )
        .unwrap();
        Arc::make_mut(&mut edits.masks).masks[0].components[0] = MaskComponent {
            name: "Luminance Range".to_owned(),
            kind: MaskKind::LuminanceRange,
            combine: MaskCombineMode::Add,
            enabled: true,
            invert: false,
            geometry: MaskGeometry::LuminanceRange {
                source: Some(source),
                low: 0.2,
                high: 0.8,
                grow: 0.0,
                feather: 0.15,
            },
        };
        let encoded = encode(edits).unwrap();
        assert!(encoded.len() < 64 * 1024);
        let loaded = decode(&encoded).unwrap();
        assert!(matches!(
            &loaded.edits.masks.masks[0].components[0].geometry,
            MaskGeometry::LuminanceRange { source: None, .. }
        ));
    }

    #[test]
    fn object_mask_round_trip_preserves_prompts_and_soft_mask() {
        use crate::pipeline::{MaskCombineMode, MaskComponent, MaskImage, ObjectStroke};

        let mut edits = sample_edits();
        let object = MaskComponent {
            name: "Object".to_owned(),
            kind: MaskKind::Object,
            combine: MaskCombineMode::Add,
            enabled: true,
            invert: false,
            geometry: MaskGeometry::Object {
                mask: Some(MaskImage::new(2, 2, vec![0, 64, 192, 255]).unwrap()),
                grow: 0.0,
                feather: 0.1,
                brush_size: 0.08,
                edge_refine: 0.7,
                strokes: vec![
                    ObjectStroke {
                        points: vec![[0.25, 0.25], [0.5, 0.5]],
                        positive: true,
                        brush_size: 0.0,
                    },
                    ObjectStroke {
                        points: vec![[0.75, 0.75]],
                        positive: false,
                        brush_size: 0.0,
                    },
                ],
            },
        };
        Arc::make_mut(&mut edits.masks).masks[0].components = vec![object];

        let encoded = encode(edits.clone()).unwrap();
        let loaded = decode(&encoded).unwrap();
        assert_eq!(loaded.edits, edits);
    }

    #[test]
    fn repeated_shared_range_sources_stay_small() {
        use crate::pipeline::{MaskCombineMode, MaskComponent, MaskRgbImage};

        let mut edits = sample_edits();
        let width = 2048;
        let height = 2048;
        let source = MaskRgbImage::new(
            width,
            height,
            vec![63; width as usize * height as usize * 4],
        )
        .unwrap();
        let component = MaskComponent {
            name: "Range".to_owned(),
            kind: MaskKind::LuminanceRange,
            combine: MaskCombineMode::Add,
            enabled: true,
            invert: false,
            geometry: MaskGeometry::LuminanceRange {
                source: Some(source),
                low: 0.2,
                high: 0.8,
                grow: 0.0,
                feather: 0.15,
            },
        };
        Arc::make_mut(&mut edits.masks).masks[0].components = vec![component; 3];
        assert!(encode(edits).unwrap().len() < 64 * 1024);
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn developed_thumbnail_cache_uses_private_application_directory() {
        let raw = Path::new("photos/photo.CR3");
        let cache = developed_thumbnail_path_for_raw(raw);
        assert!(cache.starts_with(crate::thumbnail_cache::desktop_thumbnail_cache_root()));
        assert!(cache
            .to_string_lossy()
            .ends_with(DEVELOPED_THUMBNAIL_SUFFIX));
        assert_ne!(cache.parent(), raw.parent());
    }

    #[cfg(not(target_os = "android"))]
    #[test]
    fn developed_thumbnail_cache_round_trips_and_tracks_sidecar_content() {
        let directory = temporary_directory("developed-thumbnail");
        let raw = directory.join("photo.CR3");
        fs::write(&raw, b"raw").unwrap();
        fs::write(sidecar_path_for_raw(&raw), b"edit-one").unwrap();
        let fingerprint = desktop_sidecar_fingerprint(&raw).unwrap().unwrap();
        let thumbnail = RawThumbnail {
            width: 16,
            height: 16,
            rgba: [10, 20, 30, 255].repeat(16 * 16),
        };

        let cache_path = save_developed_thumbnail_cache(&raw, &thumbnail, fingerprint).unwrap();
        assert!(cache_path.starts_with(crate::thumbnail_cache::desktop_thumbnail_cache_root()));
        assert_ne!(cache_path.parent(), raw.parent());
        let loaded = load_developed_thumbnail_cache(&raw, 512)
            .unwrap()
            .expect("developed thumbnail cache should load");
        assert_eq!(loaded.width, thumbnail.width);
        assert_eq!(loaded.height, thumbnail.height);
        for (actual, expected) in loaded
            .rgba
            .chunks_exact(4)
            .zip(thumbnail.rgba.chunks_exact(4))
        {
            for channel in 0..3 {
                assert!(actual[channel].abs_diff(expected[channel]) <= 3);
            }
            assert_eq!(actual[3], 255);
        }

        fs::write(sidecar_path_for_raw(&raw), b"edit-two").unwrap();
        assert!(!developed_thumbnail_cache_is_fresh(&raw).unwrap());
        let _ = fs::remove_file(developed_thumbnail_path_for_raw(&raw));
        let _ = fs::remove_file(developed_thumbnail_fingerprint_path_for_raw(&raw));
        fs::remove_dir_all(directory).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_raw_paths_keep_their_exact_bytes() {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};

        let raw = PathBuf::from(OsString::from_vec(b"photo-\xff.NEF".to_vec()));
        assert_eq!(
            sidecar_path_for_raw(&raw).as_os_str().as_bytes(),
            b"photo-\xff.NEF.auraw"
        );
    }

    #[test]
    fn relative_sidecar_parent_is_the_current_directory() {
        let path = Path::new("photo.NEF.auraw");
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        assert_eq!(parent, Path::new("."));
    }
}
