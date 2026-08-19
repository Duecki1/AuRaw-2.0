use crate::file_ops::{replace_file, sync_parent_directory};
use crate::pipeline::{
    ExposureParams, GeometryTransform, MaskGeometry, MaskImage, MaskKind, MaskStack,
    SubjectRefinement, MAX_LOCAL_MASKS, MAX_MASK_COMPONENTS,
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

pub const SIDECAR_SCHEMA_VERSION: u32 = 11;
/// Bump when developed-thumbnail rendering semantics change without changing the sidecar bytes.
pub const DEVELOPED_THUMBNAIL_CACHE_VERSION_SALT: u64 = 0x4155_5241_5700_0006;
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
    /// Explicit per-image DCP selection relative to the configured camera-profile root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub camera_profile: Option<PathBuf>,
    pub masks: Arc<MaskStack>,
    /// Shared signed brush correction for every AI Subject / Not Subject mask.
    /// Missing on schema <= 6, which cleanly defaults to no refinement.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_refinement: Option<SubjectRefinement>,
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
        subject_refinement: None,
        lens: LensEditState::default(),
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
        || edits.subject_refinement != default.subject_refinement
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
    // Schema 5 introduced extracted PNG mask assets. Schema 6 kept that layout.
    // Schema 7 adds the optional shared Subject refinement field;
    // schema 8 adds the defaulted mask-effect selector; schema 9 adds the
    // Fullscreen mask geometry; schema 10 adds non-destructive settings for
    // implemented mask effects; schema 11 adds procedural Fog and Smoke mask
    // effects. Serde defaults keep every earlier sidecar backward-compatible.
    if original_schema >= 5 {
        restore_mask_assets(
            &mut document.edits,
            &document.mask_assets,
            &document.mask_asset_refs,
        )?;
    } else {
        // TODO(pre-release cleanup): Remove this beta-only schema <= 4 inline-mask migration
        // once AuRaw is published. It exists only so beta testers keep their edits and masks;
        // `LoadedSidecar::migrated` immediately queues a current-schema rewrite after opening the RAW.
        if !document.mask_assets.is_empty() || !document.mask_asset_refs.is_empty() {
            return invalid("legacy sidecar unexpectedly contains current mask assets");
        }
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
    // "Reset all adjustments" is deliberately destructive: the sidecar is
    // the complete persisted edit state, including normal masks, generated AI
    // mask bitmaps/prompts, crop, camera-profile selection, and
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

/// Rejects a mask copy/duplicate before it becomes live state only when the
/// prospective persisted edit cannot fit after exact compressed re-measurement.
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
    // This covers the bounded camera-profile/lens strings, the complete global
    // adjustment structure, and document-level JSON punctuation. Dynamic mask
    // names and geometry are counted separately below.
    const DOCUMENT_HEADROOM: u64 = 1024 * 1024;
    const MASK_HEADROOM: u64 = 16 * 1024;
    const COMPONENT_HEADROOM: u64 = 2 * 1024;
    const BRUSH_DAB_HEADROOM: u64 = 256;
    const OBJECT_STROKE_HEADROOM: u64 = 128;
    const OBJECT_POINT_HEADROOM: u64 = 96;
    // PNG adds scanline filters, chunks, and DEFLATE framing. The real schema-6
    // encoder normally makes masks much smaller, but the fast preflight uses
    // a compression-independent upper bound so an accepted edit is always savable.
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
                }
                | MaskGeometry::Landscape {
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
