use super::*;

impl LibraryState {
    pub(crate) fn touch_and_request_thumbnail(&mut self, index: usize, context: &egui::Context) {
        self.restore_resident_thumbnail_texture(index, context);

        let generation = self.generation.load(Ordering::Acquire);
        let request_sender = self.request_sender.clone();

        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };

        // A full GPU texture needs no work. A resident fallback remains visible
        // while we opportunistically queue the full thumbnail again, so revisiting
        // an evicted card never falls back to the loading placeholder.
        if entry.texture.is_some() && !entry.texture_is_resident || entry.thumbnail_queued {
            return;
        }
        if entry.thumbnail_error.is_some() {
            if entry
                .thumbnail_retry_after
                .is_some_and(|retry_after| Instant::now() < retry_after)
            {
                return;
            }
            entry.thumbnail_error = None;
            entry.thumbnail_retry_after = None;
        }
        let request = ThumbnailRequest {
            generation,
            asset_id: entry.asset.id.clone(),
            display_priority: true,
        };
        if request_sender
            .as_ref()
            .is_some_and(|sender| sender.try_send(request).is_ok())
        {
            entry.thumbnail_queued = true;
        }
    }

    pub(super) fn restore_resident_thumbnail_texture(&mut self, index: usize, context: &egui::Context) {
        let generation = self.generation.load(Ordering::Acquire);
        self.usage_clock = self.usage_clock.wrapping_add(1).max(1);
        let usage_clock = self.usage_clock;
        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };
        entry.last_used = usage_clock;

        if entry.texture.is_none() {
            if let Some(resident) = entry.resident_thumbnail.as_ref() {
                let image = egui::ColorImage::from_rgba_unmultiplied(
                    [resident.width as usize, resident.height as usize],
                    &resident.rgba,
                );
                entry.texture = Some(context.load_texture(
                    format!("library-resident-thumbnail-{generation}-{index}-{usage_clock}"),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
                entry.texture_is_resident = entry
                    .thumbnail_size
                    .is_some_and(|size| size != [resident.width, resident.height]);
            }
        }
    }

    #[cfg(not(target_os = "android"))]
    pub(crate) fn install_developed_thumbnail(
        &mut self,
        raw_path: &Path,
        thumbnail: RawThumbnail,
        context: &egui::Context,
        revision: u64,
    ) {
        let asset_id = LibraryAssetId::Desktop(raw_path.to_owned());
        let Some(index) = self.entry_indices.get(&asset_id).copied() else {
            return;
        };
        self.install_developed_thumbnail_at(index, thumbnail, context, revision);
    }

    #[cfg(target_os = "android")]
    pub(crate) fn install_android_developed_thumbnail(
        &mut self,
        raw_uri: &str,
        thumbnail: RawThumbnail,
        context: &egui::Context,
        revision: u64,
    ) {
        let asset_id = LibraryAssetId::Android(raw_uri.to_owned());
        let Some(index) = self.entry_indices.get(&asset_id).copied() else {
            return;
        };
        self.install_developed_thumbnail_at(index, thumbnail, context, revision);
    }

    pub(crate) fn invalidate_adjustment_thumbnail_for_asset(&mut self, asset: &LibraryAsset) {
        if let Some(index) = self.entry_indices.get(&asset.id).copied() {
            self.invalidate_adjustment_thumbnail_at(index);
        }
    }

    pub(super) fn invalidate_adjustment_thumbnail_at(&mut self, index: usize) {
        let Some(entry) = self.entries.get_mut(index) else {
            return;
        };
        if !entry.developed_thumbnail {
            return;
        }

        // A reset removes the developed cache, so the next valid result is an
        // unedited RAW thumbnail. Clear the developed marker immediately;
        // otherwise poll_events treats that RAW result as a stale downgrade and
        // can leave the card blank after its old texture is evicted.
        entry.texture = None;
        entry.resident_thumbnail = None;
        entry.texture_is_resident = false;
        entry.thumbnail_size = None;
        entry.thumbnail_error = None;
        entry.thumbnail_failures = 0;
        entry.thumbnail_retry_after = None;
        entry.thumbnail_queued = false;
        entry.developed_thumbnail = false;
    }

    pub(super) fn install_developed_thumbnail_at(
        &mut self,
        index: usize,
        thumbnail: RawThumbnail,
        context: &egui::Context,
        revision: u64,
    ) {
        let image = egui::ColorImage::from_rgba_unmultiplied(
            [thumbnail.width as usize, thumbnail.height as usize],
            &thumbnail.rgba,
        );
        self.entries[index].texture = Some(context.load_texture(
            format!("library-developed-thumbnail-{index}-{revision}"),
            image,
            egui::TextureOptions::LINEAR,
        ));
        let decoded_size = [thumbnail.width, thumbnail.height];
        let resident_thumbnail = make_resident_thumbnail(&thumbnail);
        self.entries[index].thumbnail_size = Some(decoded_size);
        self.entries[index].layout_size.get_or_insert(decoded_size);
        self.entries[index].resident_thumbnail = Some(resident_thumbnail);
        self.entries[index].texture_is_resident = false;
        self.entries[index].thumbnail_error = None;
        self.entries[index].thumbnail_failures = 0;
        self.entries[index].thumbnail_retry_after = None;
        self.entries[index].thumbnail_queued = false;
        self.entries[index].developed_thumbnail = true;
    }

    pub(crate) fn evict_old_textures(&mut self, protected_indices: &HashSet<usize>) {
        let limit = if cfg!(target_os = "android") {
            ANDROID_TEXTURE_CACHE_LIMIT
        } else {
            DESKTOP_TEXTURE_CACHE_LIMIT
        };
        self.evict_textures_to_limit_protecting(limit, protected_indices);
        let resident_limit = if cfg!(target_os = "android") {
            ANDROID_RESIDENT_THUMBNAIL_CACHE_LIMIT
        } else {
            DESKTOP_RESIDENT_THUMBNAIL_CACHE_LIMIT
        };
        self.evict_resident_thumbnails_to_limit_protecting(resident_limit, protected_indices);
    }

    #[cfg(target_os = "android")]
    pub(super) fn evict_textures_to_limit(&mut self, limit: usize) {
        self.evict_textures_to_limit_protecting(limit, &HashSet::new());
    }

    pub(super) fn evict_textures_to_limit_protecting(
        &mut self,
        limit: usize,
        protected_indices: &HashSet<usize>,
    ) {
        let texture_count = self
            .entries
            .iter()
            .filter(|entry| entry.texture.is_some())
            .count();
        if texture_count <= limit {
            return;
        }
        // Never evict thumbnails that are currently visible or inside the preload
        // margin. On a desktop resize/fullscreen transition the number of active
        // thumbnails can temporarily exceed the nominal cache limit. Evicting by
        // LRU alone would repeatedly remove the first (top) visible rows because
        // they are touched first every frame, making them oscillate between the
        // texture and the "Loading preview…" placeholder.
        let protected_texture_count = protected_indices
            .iter()
            .filter(|&&index| {
                self.entries
                    .get(index)
                    .is_some_and(|entry| entry.texture.is_some())
            })
            .count();
        let effective_limit = limit.max(protected_texture_count);
        if texture_count <= effective_limit {
            return;
        }

        let mut candidates = self
            .entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| entry.texture.is_some() && !protected_indices.contains(index))
            .map(|(index, entry)| (entry.last_used, index))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        for (_, index) in candidates.into_iter().take(texture_count - effective_limit) {
            self.entries[index].texture = None;
            self.entries[index].texture_is_resident = false;
            // Keep decoded dimensions and a bounded resident pixel fallback after GPU
            // eviction. Returning to this card can rebuild a texture synchronously.
        }
    }

    pub(super) fn evict_resident_thumbnails_to_limit_protecting(
        &mut self,
        limit: usize,
        protected_indices: &HashSet<usize>,
    ) {
        let resident_count = self
            .entries
            .iter()
            .filter(|entry| entry.resident_thumbnail.is_some())
            .count();
        if resident_count <= limit {
            return;
        }

        let protected_resident_count = protected_indices
            .iter()
            .filter(|&&index| {
                self.entries
                    .get(index)
                    .is_some_and(|entry| entry.resident_thumbnail.is_some())
            })
            .count();
        let effective_limit = limit.max(protected_resident_count);
        if resident_count <= effective_limit {
            return;
        }

        let mut candidates = self
            .entries
            .iter()
            .enumerate()
            .filter(|(index, entry)| {
                entry.resident_thumbnail.is_some() && !protected_indices.contains(index)
            })
            .map(|(index, entry)| (entry.last_used, index))
            .collect::<Vec<_>>();
        candidates.sort_unstable();
        for (_, index) in candidates
            .into_iter()
            .take(resident_count - effective_limit)
        {
            self.entries[index].resident_thumbnail = None;
        }
    }
}

pub(super) fn new_library_entry(asset: LibraryAsset) -> LibraryEntry {
    // Keep gallery geometry immutable for the lifetime of the catalog entry.
    // Header probing supplies the real display ratio for normal supported RAWs;
    // 3:2 is only a last-resort fallback when metadata cannot be inspected.
    let layout_size = Some(asset.metadata.dimensions_hint.unwrap_or([3, 2]));
    LibraryEntry {
        asset,
        texture: None,
        resident_thumbnail: None,
        texture_is_resident: false,
        thumbnail_size: None,
        layout_size,
        thumbnail_error: None,
        thumbnail_failures: 0,
        thumbnail_retry_after: None,
        thumbnail_queued: false,
        developed_thumbnail: false,
        last_used: 0,
    }
}

pub(super) fn same_library_asset_identity(left: &LibraryAsset, right: &LibraryAsset) -> bool {
    left.id == right.id
        && left.metadata.bytes == right.metadata.bytes
        && left.metadata.modified_seconds == right.metadata.modified_seconds
}

pub(super) fn compare_library_entries(
    left: &LibraryEntry,
    right: &LibraryEntry,
    sort_order: LibrarySortOrder,
) -> CmpOrdering {
    let name_order = compare_library_names(&left.asset, &right.asset);

    match sort_order {
        LibrarySortOrder::NewestFirst => right
            .asset
            .metadata
            .modified_seconds
            .cmp(&left.asset.metadata.modified_seconds)
            .then(name_order),
        LibrarySortOrder::OldestFirst => left
            .asset
            .metadata
            .modified_seconds
            .cmp(&right.asset.metadata.modified_seconds)
            .then(name_order),
        LibrarySortOrder::NameAscending => name_order,
        LibrarySortOrder::NameDescending => name_order.reverse(),
        LibrarySortOrder::LargestFirst => right
            .asset
            .metadata
            .bytes
            .cmp(&left.asset.metadata.bytes)
            .then(name_order),
        LibrarySortOrder::SmallestFirst => left
            .asset
            .metadata
            .bytes
            .cmp(&right.asset.metadata.bytes)
            .then(name_order),
    }
}

pub(super) fn compare_library_names(left: &LibraryAsset, right: &LibraryAsset) -> CmpOrdering {
    left.display_name
        .to_lowercase()
        .cmp(&right.display_name.to_lowercase())
        .then_with(|| left.display_path.cmp(&right.display_path))
        .then_with(|| left.id.cmp(&right.id))
}

pub(super) fn library_modified_key(asset: &LibraryAsset) -> u64 {
    asset.metadata.modified_seconds
}

pub(super) fn make_resident_thumbnail(thumbnail: &RawThumbnail) -> RawThumbnail {
    if thumbnail.width <= RESIDENT_THUMBNAIL_EDGE && thumbnail.height <= RESIDENT_THUMBNAIL_EDGE {
        return thumbnail.clone();
    }

    let Some(image) =
        image::RgbaImage::from_raw(thumbnail.width, thumbnail.height, thumbnail.rgba.clone())
    else {
        return thumbnail.clone();
    };
    let image = image::DynamicImage::ImageRgba8(image)
        .thumbnail(RESIDENT_THUMBNAIL_EDGE, RESIDENT_THUMBNAIL_EDGE)
        .to_rgba8();
    let (width, height) = image.dimensions();
    RawThumbnail {
        width,
        height,
        rgba: image.into_raw(),
    }
}

pub(super) fn loaded_library_thumbnail(thumbnail: RawThumbnail, developed: bool) -> LoadedLibraryThumbnail {
    let resident_thumbnail = make_resident_thumbnail(&thumbnail);
    LoadedLibraryThumbnail {
        thumbnail,
        resident_thumbnail,
        developed,
    }
}

pub(super) type ThumbnailLoader =
    Arc<dyn Fn(&LibraryAsset) -> Result<LoadedLibraryThumbnail, String> + Send + Sync + 'static>;

#[cfg(not(target_os = "android"))]
pub(super) struct DevelopedThumbnailGpu {
    device: eframe::wgpu::Device,
    queue: eframe::wgpu::Queue,
}

#[cfg(not(target_os = "android"))]
static DEVELOPED_THUMBNAIL_GPU: OnceLock<Result<Mutex<DevelopedThumbnailGpu>, String>> =
    OnceLock::new();

#[cfg(not(target_os = "android"))]
pub(super) fn developed_thumbnail_gpu() -> Result<&'static Mutex<DevelopedThumbnailGpu>, String> {
    let initialized = DEVELOPED_THUMBNAIL_GPU.get_or_init(|| {
        let instance = eframe::wgpu::Instance::default();
        let adapter = pollster::block_on(instance.request_adapter(
            &eframe::wgpu::RequestAdapterOptions {
                power_preference: eframe::wgpu::PowerPreference::LowPower,
                compatible_surface: None,
                force_fallback_adapter: false,
            },
        ))
        .or_else(|_| {
            pollster::block_on(instance.request_adapter(
                &eframe::wgpu::RequestAdapterOptions {
                    power_preference: eframe::wgpu::PowerPreference::LowPower,
                    compatible_surface: None,
                    force_fallback_adapter: true,
                },
            ))
        })
        .map_err(|error| format!("could not find a GPU for edited thumbnails: {error}"))?;
        let adapter_info = adapter.get_info();
        let adapter_limits = adapter.limits();
        let required_dimension = DEVELOPED_THUMBNAIL_PROXY_EDGE.max(mask_atlas_edge());
        if required_dimension > adapter_limits.max_texture_dimension_2d {
            return Err(format!(
                "edited thumbnails require a {required_dimension}-pixel GPU texture, but this adapter supports {}",
                adapter_limits.max_texture_dimension_2d
            ));
        }
        let mut required_limits = if adapter_info.backend == eframe::wgpu::Backend::Gl {
            eframe::wgpu::Limits::downlevel_webgl2_defaults()
        } else {
            eframe::wgpu::Limits::default()
        };
        required_limits.max_texture_dimension_2d = required_dimension;
        let (device, queue) = pollster::block_on(adapter.request_device(
            &eframe::wgpu::DeviceDescriptor {
                label: Some("auraw library edited-thumbnail device"),
                required_limits,
                ..Default::default()
            },
        ))
        .map_err(|error| format!("could not create the edited-thumbnail GPU device: {error}"))?;
        auraw_gpu::install_uncaptured_gpu_error_handler(&device);
        Ok(Mutex::new(DevelopedThumbnailGpu { device, queue }))
    });

    match initialized {
        Ok(gpu) => Ok(gpu),
        Err(error) => Err(error.clone()),
    }
}

#[cfg(not(target_os = "android"))]
pub(super) fn masks_need_canonical_source(masks: &MaskStack) -> bool {
    masks.masks.iter().any(|mask| {
        mask.components.iter().any(|component| {
            matches!(
                &component.geometry,
                MaskGeometry::LuminanceRange { source: None, .. }
                    | MaskGeometry::ColorRange { source: None, .. }
            )
        })
    })
}

#[cfg(not(target_os = "android"))]
pub(super) fn install_missing_range_sources(masks: &mut MaskStack, source: &MaskRgbImage) {
    for mask in &mut masks.masks {
        for component in &mut mask.components {
            match &mut component.geometry {
                MaskGeometry::LuminanceRange { source: target, .. }
                | MaskGeometry::ColorRange { source: target, .. }
                    if target.is_none() =>
                {
                    *target = Some(source.clone());
                }
                _ => {}
            }
        }
    }
}

#[cfg(not(target_os = "android"))]
pub(super) fn render_uncached_developed_thumbnail(
    path: &Path,
    maximum_edge: u32,
) -> Result<Option<RawThumbnail>, String> {
    let loaded_sidecar = match crate::sidecar::load_desktop(path) {
        Ok(Some(sidecar)) => sidecar,
        Ok(None) => return Ok(None),
        // Match RAW opening: malformed edit JSON is recoverable. Returning
        // None lets the library use its normal cached/embedded RAW thumbnail
        // instead of leaving the card without any preview.
        Err(crate::sidecar::SidecarError::Invalid(error)) => {
            log::warn!(
                "ignoring invalid sidecar while rendering library thumbnail for {}: {error}",
                path.display()
            );
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "could not load edits for {}: {error}",
                path.display()
            ))
        }
    };
    let sidecar_fingerprint = crate::sidecar::desktop_sidecar_fingerprint(path)?
        .ok_or_else(|| "edit sidecar disappeared before thumbnail rendering".to_owned())?;

    // Edited rebuilds and preview-less RAW fallbacks share the same user-set
    // concurrency budget because both unpack full sensors. The headless GPU
    // phase below remains serialized on its device while RAW preparation for
    // other edited cards can proceed concurrently.
    let _render_permit = crate::thumbnail_cache::acquire_rendered_thumbnail_worker();

    // A different worker may have completed the cache while this request was
    // waiting for a rendered-thumbnail permit.
    if let Some(thumbnail) = crate::sidecar::load_developed_thumbnail_cache(path, maximum_edge)? {
        let cached_edge = thumbnail.width.max(thumbnail.height);
        let minimum_edge = maximum_edge.saturating_mul(3) / 4;
        if maximum_edge <= THUMBNAIL_EDGE || cached_edge >= minimum_edge {
            return Ok(Some(thumbnail));
        }
    }

    let performance =
        crate::performance_settings::load(crate::performance_settings::desktop_path().as_deref());
    let mut camera_profile_folder = performance.camera_profile_folder;
    if performance.camera_profile_auto_detect
        && camera_profile_folder
            .as_ref()
            .is_none_or(|folder| !folder.is_dir())
    {
        camera_profile_folder = crate::performance_settings::detected_adobe_camera_profile_folder();
    }
    let requested_camera_profile =
        loaded_sidecar
            .edits
            .camera_profile
            .as_ref()
            .and_then(|relative| {
                camera_profile_folder
                    .as_ref()
                    .map(|root| root.join(relative))
            });
    let full_raw = load_raw_file_with_profile_selection(
        path,
        performance.camera_profile_mode,
        camera_profile_folder.as_deref(),
        requested_camera_profile.as_deref(),
    )
    .map_err(|error| format!("could not decode edited RAW {}: {error:#}", path.display()))?;
    let render_proxy_edge = DEVELOPED_THUMBNAIL_PROXY_EDGE.max(maximum_edge);
    let mut preview_raw = if full_raw.width.max(full_raw.height) > render_proxy_edge {
        build_proxy(
            &full_raw,
            ProxySpec {
                max_edge: render_proxy_edge,
            },
        )
    } else {
        full_raw
    };

    let edits = loaded_sidecar.edits;
    let geometry = edits.geometry;
    if edits.lens.enabled {
        let catalog = lensfun_catalog(&preview_raw);
        let selected = catalog
            .lenses
            .iter()
            .find(|lens| lens.maker == edits.lens.maker && lens.model == edits.lens.model)
            .cloned()
            .or_else(|| {
                (!edits.lens.maker.is_empty() || !edits.lens.model.is_empty()).then(|| {
                    LensfunLens {
                        maker: edits.lens.maker.clone(),
                        model: edits.lens.model.clone(),
                    }
                })
            })
            .or(catalog.auto_match);
        if let Some(selected) = selected {
            match apply_lensfun_correction(&preview_raw, &selected) {
                Ok(corrected) => preview_raw = corrected,
                Err(error) => log::warn!(
                    "could not apply saved lens correction to library thumbnail {}: {error:#}",
                    path.display()
                ),
            }
        }
    }

    let mut masks = Arc::unwrap_or_clone(edits.masks);
    let inpaint_strokes = Arc::unwrap_or_clone(edits.inpainting);
    let composed_inpaint = compose_inpaint_strokes(&inpaint_strokes);
    let initial_params =
        GpuParams::new(&edits.exposure, &masks, &preview_raw).with_vignette_geometry(geometry);
    let gpu = developed_thumbnail_gpu()?;
    let gpu = gpu
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let pipeline = RawGpuPipeline::new_headless_with_quality(
        &gpu.device,
        &gpu.queue,
        &preview_raw,
        &initial_params,
        ProcessingQuality::Preview,
    )
    .map_err(|error| format!("could not prepare edited thumbnail rendering: {error:#}"))?;
    pipeline
        .update_inpaint_layer(
            &gpu.queue,
            composed_inpaint.as_ref(),
            0,
            0,
            preview_raw.width,
            preview_raw.height,
        )
        .map_err(|error| format!("could not apply thumbnail inpainting: {error:#}"))?;

    if masks_need_canonical_source(&masks) {
        let neutral_exposure = crate::pipeline::ExposureParams::scene_referred_default();
        let neutral_masks = MaskStack::default();
        let neutral_params = GpuParams::new(&neutral_exposure, &neutral_masks, &preview_raw);
        pipeline.recompute(&gpu.queue, &gpu.device, &neutral_params);
        let rgba = pipeline
            .read_output_region_blocking(
                &gpu.device,
                &gpu.queue,
                0,
                0,
                preview_raw.width,
                preview_raw.height,
            )
            .map_err(|error| format!("could not build range-mask thumbnail source: {error:#}"))?;
        let source = MaskRgbImage::new(preview_raw.width, preview_raw.height, rgba)
            .ok_or_else(|| "range-mask thumbnail source has invalid dimensions".to_owned())?;
        install_missing_range_sources(&mut masks, &source);
    }

    for layer in 0..masks.masks.len().min(MAX_LOCAL_MASKS) {
        let edge = pipeline.mask_atlas_edge();
        let values =
            masks.rasterize_layer_f16(layer, edge, edge, preview_raw.width, preview_raw.height);
        pipeline
            .update_mask_layer(&gpu.queue, layer, &values)
            .map_err(|error| format!("could not apply thumbnail local mask: {error:#}"))?;
    }
    pipeline
        .update_light_rays_mask_layers(&gpu.queue, &masks, preview_raw.width, preview_raw.height)
        .map_err(|error| format!("could not apply thumbnail Light Rays mask: {error:#}"))?;
    let params =
        GpuParams::new(&edits.exposure, &masks, &preview_raw).with_vignette_geometry(geometry);
    pipeline.recompute(&gpu.queue, &gpu.device, &params);
    let thumbnail = pipeline
        .output_snapshot()
        .read_thumbnail_blocking(&gpu.device, &gpu.queue, maximum_edge)
        .map_err(|error| format!("could not read edited thumbnail pixels: {error:#}"))?;
    let thumbnail = crate::pipeline::transform_thumbnail_geometry_with_lens(
        &thumbnail,
        geometry,
        preview_raw.lens_geometry.as_deref(),
    );
    crate::sidecar::save_developed_thumbnail_cache(path, &thumbnail, sidecar_fingerprint)?;
    Ok(Some(thumbnail))
}

#[cfg(not(target_os = "android"))]
pub(crate) fn load_desktop_reference_preview(
    path: &Path,
    maximum_edge: u32,
) -> Result<RawThumbnail, String> {
    if maximum_edge == 0 {
        return Err("reference preview edge must be non-zero".to_owned());
    }

    // Preserve developed edits when a sidecar exists. The reference request is
    // allowed to ask for a larger render than the 512 px catalog card;
    // `render_uncached_developed_thumbnail` regenerates an undersized cache.
    match render_uncached_developed_thumbnail(path, maximum_edge) {
        Ok(Some(thumbnail)) => return Ok(thumbnail),
        Ok(None) => {}
        Err(error) => {
            log::warn!(
                "could not render developed reference preview for {}: {error}",
                path.display()
            );
        }
    }

    // Unedited references use the same RAW preview loader as the catalog but
    // at a much larger edge. Most cameras embed a full-resolution JPEG; when
    // they do not, the loader retains its LibRaw processed fallback.
    load_raw_thumbnail(path, maximum_edge)
        .map_err(|error| format!("could not render reference preview: {error:#}"))
}

/// Loads only an already-rendered desktop thumbnail. This deliberately avoids
/// the embedded-preview and sensor-decode fallbacks used to populate Library
/// cards, so it is safe to run alongside the full RAW open worker.
#[cfg(not(target_os = "android"))]
pub(crate) fn load_desktop_cached_thumbnail(
    path: &Path,
    maximum_edge: u32,
) -> Result<Option<RawThumbnail>, String> {
    match crate::sidecar::load_developed_thumbnail_cache(path, maximum_edge) {
        Ok(Some(thumbnail)) => return Ok(Some(thumbnail)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use developed loading thumbnail for {}: {error}",
            path.display()
        ),
    }
    crate::thumbnail_cache::load_desktop_raw_thumbnail(path, maximum_edge)
}

#[cfg(not(target_os = "android"))]
pub(super) fn load_desktop_library_thumbnail(
    asset: &LibraryAsset,
) -> Result<LoadedLibraryThumbnail, String> {
    let Some(path) = asset.desktop_path() else {
        return Err("invalid desktop thumbnail request".to_owned());
    };
    match crate::sidecar::load_developed_thumbnail_cache(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, true)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use developed thumbnail cache for {}: {error}",
            path.display()
        ),
    }
    match render_uncached_developed_thumbnail(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, true)),
        Ok(None) => {}
        Err(error) => {
            return Err(format!(
                "could not render the edited RAW thumbnail for {}: {error}",
                path.display()
            ))
        }
    }
    match crate::thumbnail_cache::load_desktop_raw_thumbnail(path, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, false)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use RAW thumbnail cache for {}: {error}",
            path.display()
        ),
    }

    // Prefer the camera-generated JPEG/bitmap, but a missing or unsupported
    // embedded preview must never make an otherwise valid RAW card permanent.
    // `load_raw_thumbnail` falls back to LibRaw's half-size sensor render.
    let thumbnail = load_raw_thumbnail(path, THUMBNAIL_EDGE)
        .map_err(|error| format!("could not render a RAW preview: {error:#}"))?;
    if let Err(error) = crate::thumbnail_cache::save_desktop_raw_thumbnail(path, &thumbnail) {
        log::warn!(
            "could not persist RAW thumbnail cache for {}: {error}",
            path.display()
        );
    }
    Ok(loaded_library_thumbnail(thumbnail, false))
}

#[cfg(target_os = "android")]
pub(super) fn load_android_library_thumbnail(
    app: &auraw_ffi::AndroidApp,
    asset: &LibraryAsset,
) -> Result<LoadedLibraryThumbnail, String> {
    let Some(uri) = asset.android_uri() else {
        return Err("invalid Android thumbnail request".to_owned());
    };
    let display_name = asset.display_name.as_str();
    let bytes = asset.metadata.bytes;
    let modified_seconds = asset.metadata.modified_seconds;
    match crate::android::load_developed_thumbnail_cache(app, uri, display_name, THUMBNAIL_EDGE) {
        Ok(Some(thumbnail)) => return Ok(loaded_library_thumbnail(thumbnail, true)),
        Ok(None) => {}
        Err(error) => log::warn!(
            "could not use Android developed-thumbnail cache for {display_name}: {error}"
        ),
    }
    let mut thumbnail = crate::android::load_library_thumbnail(
        app,
        uri,
        display_name,
        bytes,
        modified_seconds,
        THUMBNAIL_EDGE,
    )?;
    // Android cannot headlessly rebuild all adjustments while browsing the
    // library, but geometry is cheap and important for composition. Apply the
    // saved crop/orientation even when a developed cache has not been captured
    // yet; opening/saving the image later replaces this with the fully developed
    // geometry-aware thumbnail.
    if let Ok(Some(sidecar)) = crate::sidecar::load_android(app, uri, display_name) {
        thumbnail =
            crate::pipeline::transform_thumbnail_geometry(&thumbnail, sidecar.edits.geometry);
    }
    Ok(loaded_library_thumbnail(thumbnail, false))
}

pub(super) fn run_thumbnail_workers(worker: ThumbnailWorker, worker_count: usize, load: ThumbnailLoader) {
    let ThumbnailWorker {
        assets,
        warning_count,
        truncated,
        generation,
        cancellation,
        decoding_paused,
        decode_gate,
        event_sender,
        request_receiver,
        repaint,
    } = worker;
    if cancellation.load(Ordering::Acquire) != generation {
        return;
    }
    let work_queue = Arc::new(Mutex::new(ThumbnailWorkQueue::new(generation, &assets)));
    if event_sender
        .send(ScanEvent::Catalog {
            generation,
            assets: assets.clone(),
            warning_count,
            truncated,
        })
        .is_err()
    {
        return;
    }
    repaint.request_repaint();

    let assets = Arc::new(assets);
    let request_receiver = Arc::new(Mutex::new(request_receiver));
    let worker_count = worker_count.clamp(1, maximum_thumbnail_worker_count());
    let mut handles = Vec::with_capacity(worker_count);
    for worker_index in 0..worker_count {
        let cancellation = Arc::clone(&cancellation);
        let decoding_paused = Arc::clone(&decoding_paused);
        let decode_gate = Arc::clone(&decode_gate);
        let event_sender = event_sender.clone();
        let request_receiver = Arc::clone(&request_receiver);
        let work_queue = Arc::clone(&work_queue);
        let repaint = repaint.clone();
        let load = Arc::clone(&load);
        let assets = Arc::clone(&assets);
        let spawn = std::thread::Builder::new()
            .name(format!("auraw-thumbnail-{worker_index}"))
            .spawn(move || {
                run_one_thumbnail_worker(ThumbnailWorkerContext {
                    assets,
                    generation,
                    cancellation,
                    decoding_paused,
                    decode_gate,
                    event_sender,
                    request_receiver,
                    work_queue,
                    repaint,
                    load,
                })
            });
        match spawn {
            Ok(handle) => handles.push(handle),
            Err(error) => log::warn!("could not start thumbnail worker {worker_index}: {error}"),
        }
    }

    if handles.is_empty() {
        send_scan_failure(
            &event_sender,
            generation,
            "Could not start any thumbnail workers.".to_owned(),
            &repaint,
        );
        return;
    }
    for handle in handles {
        if handle.join().is_err() {
            log::warn!("a thumbnail worker panicked");
        }
    }
}

struct ThumbnailWorkerContext {
    assets: Arc<Vec<LibraryAsset>>,
    generation: u64,
    cancellation: Arc<AtomicU64>,
    decoding_paused: Arc<AtomicBool>,
    decode_gate: Arc<RwLock<()>>,
    event_sender: mpsc::SyncSender<ScanEvent>,
    request_receiver: Arc<Mutex<mpsc::Receiver<ThumbnailRequest>>>,
    work_queue: Arc<Mutex<ThumbnailWorkQueue>>,
    repaint: egui::Context,
    load: ThumbnailLoader,
}

fn run_one_thumbnail_worker(context: ThumbnailWorkerContext) {
    let ThumbnailWorkerContext {
        assets,
        generation,
        cancellation,
        decoding_paused,
        decode_gate,
        event_sender,
        request_receiver,
        work_queue,
        repaint,
        load,
    } = context;
    while cancellation.load(Ordering::Acquire) == generation {
        let received = request_receiver
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .try_recv();
        let (request, initial_background) = match received {
            Ok(request) => (request, false),
            Err(mpsc::TryRecvError::Empty) => {
                // Develop pauses catalog-wide background decoding, but visible
                // filmstrip/reference requests still arrive through the explicit
                // request channel. Do not let workers get stuck holding ordinary
                // background entries while those display-priority requests wait.
                if decoding_paused.load(Ordering::Acquire) {
                    std::thread::sleep(THUMBNAIL_PAUSE_POLL_INTERVAL);
                    continue;
                }
                let background = work_queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .background
                    .pop_front();
                let Some(request) = background else {
                    std::thread::sleep(THUMBNAIL_QUEUE_POLL_INTERVAL);
                    continue;
                };
                (request, true)
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                if decoding_paused.load(Ordering::Acquire) {
                    break;
                }
                let background = work_queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .background
                    .pop_front();
                let Some(request) = background else {
                    break;
                };
                (request, true)
            }
        };
        if request.generation != generation {
            continue;
        }
        if !work_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .claim(&request, initial_background)
        {
            continue;
        }
        let result = loop {
            // Ordinary catalog requests remain paused in Develop. Explicit
            // display-priority requests (filmstrip/reference) may proceed, but
            // still take the shared decode gate so an active full RAW open keeps
            // exclusive priority and the application's peak memory stays bounded.
            while decoding_paused.load(Ordering::Acquire) && !request.display_priority {
                if cancellation.load(Ordering::Acquire) != generation {
                    return;
                }
                std::thread::sleep(THUMBNAIL_PAUSE_POLL_INTERVAL);
            }

            let decode_guard = decode_gate
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if cancellation.load(Ordering::Acquire) != generation {
                return;
            }
            if decoding_paused.load(Ordering::Acquire) && !request.display_priority {
                drop(decode_guard);
                continue;
            }
            let Some(asset) = assets.iter().find(|asset| asset.id == request.asset_id) else {
                break Err("thumbnail asset disappeared from the catalog".to_owned());
            };
            break load(asset);
        };
        let display_priority = work_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .finish(&request.asset_id);
        if event_sender
            .send(ScanEvent::Thumbnail {
                generation,
                asset_id: request.asset_id,
                display_priority,
                result,
            })
            .is_err()
        {
            break;
        }
        repaint.request_repaint();
    }
}

