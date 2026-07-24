use super::{CompactPixelMap, LoadedRaw};
use anyhow::{anyhow, Result};

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LensfunLens {
    pub maker: String,
    pub model: String,
}

impl LensfunLens {
    pub fn label(&self) -> String {
        match (self.maker.trim(), self.model.trim()) {
            ("", model) => model.to_owned(),
            (maker, "") => maker.to_owned(),
            (maker, model) => format!("{maker} {model}"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct LensfunCatalog {
    pub available: bool,
    pub camera_label: String,
    pub lenses: Vec<LensfunLens>,
    pub auto_match: Option<LensfunLens>,
    pub status: String,
}

pub fn lensfun_catalog(raw: &LoadedRaw) -> LensfunCatalog {
    imp::catalog(raw)
}

pub fn apply_lensfun_correction(raw: &LoadedRaw, selection: &LensfunLens) -> Result<LoadedRaw> {
    imp::apply(raw, selection)
}

#[cfg(not(lensfun_available))]
mod imp {
    use super::*;

    pub(super) fn catalog(_raw: &LoadedRaw) -> LensfunCatalog {
        LensfunCatalog {
            available: false,
            status: "Lensfun is not available in this build. Install the Lensfun development package and rebuild AuRaw.".to_owned(),
            ..LensfunCatalog::default()
        }
    }

    pub(super) fn apply(_raw: &LoadedRaw, _selection: &LensfunLens) -> Result<LoadedRaw> {
        Err(anyhow!(
            "this build was compiled without Lensfun; lens correction is unavailable"
        ))
    }
}

#[cfg(lensfun_available)]
mod imp {
    use super::*;
    use anyhow::Context;
    use rayon::prelude::*;
    use std::ffi::{c_char, c_int, c_void, CStr, CString};
    use std::path::{Path, PathBuf};
    use std::ptr;

    const LF_NO_ERROR: c_int = 0;
    const LF_SEARCH_LOOSE: c_int = 1;
    const LF_SEARCH_SORT_AND_UNIQUIFY: c_int = 2;
    const LF_PF_F32: c_int = 3;
    const LF_MODIFY_TCA: c_int = 0x0000_0001;
    const LF_MODIFY_VIGNETTING: c_int = 0x0000_0002;
    const LF_MODIFY_DISTORTION: c_int = 0x0000_0008;
    const LF_MODIFY_GEOMETRY: c_int = 0x0000_0010;
    const LF_MODIFY_SCALE: c_int = 0x0000_0020;
    const LF_CR_UNKNOWN: c_int = 2;
    const LF_CR_RED: c_int = 4;
    const LF_CR_GREEN: c_int = 5;
    const LF_CR_BLUE: c_int = 6;
    const LF_CR_RGBA: c_int =
        LF_CR_RED | (LF_CR_GREEN << 4) | (LF_CR_BLUE << 8) | (LF_CR_UNKNOWN << 12);

    #[repr(C)]
    struct lfDatabase {
        _private: [u8; 0],
    }

    #[repr(C)]
    struct lfModifier {
        _private: [u8; 0],
    }

    // Lensfun 0.3.2–0.3.4 expose these fields at the beginning of the
    // public C structs. AuRaw only reads this stable prefix; the database owns
    // each object and keeps it alive for the lifetime of `Database`.
    #[repr(C)]
    struct lfCamera {
        maker: *mut c_char,
        model: *mut c_char,
        variant: *mut c_char,
        mount: *mut c_char,
        crop_factor: f32,
        score: c_int,
    }

    #[repr(C)]
    struct lfLens {
        maker: *mut c_char,
        model: *mut c_char,
        min_focal: f32,
        max_focal: f32,
        min_aperture: f32,
        max_aperture: f32,
        mounts: *mut *mut c_char,
        center_x: f32,
        center_y: f32,
        crop_factor: f32,
        aspect_ratio: f32,
        lens_type: c_int,
    }

    unsafe extern "C" {
        fn lf_free(data: *mut c_void);
        fn lf_mlstr_get(value: *const c_char) -> *const c_char;
        fn lf_db_new() -> *mut lfDatabase;
        fn lf_db_destroy(db: *mut lfDatabase);
        fn lf_db_load(db: *mut lfDatabase) -> c_int;
        fn lf_db_load_file(db: *mut lfDatabase, filename: *const c_char) -> c_int;
        fn lf_db_find_cameras(
            db: *const lfDatabase,
            maker: *const c_char,
            model: *const c_char,
        ) -> *mut *const lfCamera;
        fn lf_db_find_cameras_ext(
            db: *const lfDatabase,
            maker: *const c_char,
            model: *const c_char,
            flags: c_int,
        ) -> *mut *const lfCamera;
        fn lf_db_find_lenses_hd(
            db: *const lfDatabase,
            camera: *const lfCamera,
            maker: *const c_char,
            lens: *const c_char,
            flags: c_int,
        ) -> *mut *const lfLens;
        fn lf_db_get_lenses(db: *const lfDatabase) -> *const *const lfLens;
        fn lf_modifier_new(
            lens: *const lfLens,
            crop: f32,
            width: c_int,
            height: c_int,
        ) -> *mut lfModifier;
        fn lf_modifier_destroy(modifier: *mut lfModifier);
        fn lf_modifier_initialize(
            modifier: *mut lfModifier,
            lens: *const lfLens,
            pixel_format: c_int,
            focal: f32,
            aperture: f32,
            distance: f32,
            scale: f32,
            target_geometry: c_int,
            flags: c_int,
            reverse: c_int,
        ) -> c_int;
        fn lf_modifier_get_auto_scale(modifier: *mut lfModifier, reverse: c_int) -> f32;
        fn lf_modifier_add_coord_callback_scale(
            modifier: *mut lfModifier,
            scale: f32,
            reverse: c_int,
        ) -> c_int;
        fn lf_modifier_apply_subpixel_geometry_distortion(
            modifier: *mut lfModifier,
            x: f32,
            y: f32,
            width: c_int,
            height: c_int,
            result: *mut f32,
        ) -> c_int;
        fn lf_modifier_apply_color_modification(
            modifier: *mut lfModifier,
            pixels: *mut c_void,
            x: f32,
            y: f32,
            width: c_int,
            height: c_int,
            component_role: c_int,
            row_stride: c_int,
        ) -> c_int;
    }

    struct Database(*mut lfDatabase);

    impl Database {
        fn create() -> Result<Self> {
            // SAFETY: Lensfun returns a uniquely owned database pointer or null.
            let pointer = unsafe { lf_db_new() };
            if pointer.is_null() {
                Err(anyhow!("Lensfun could not allocate a database"))
            } else {
                Ok(Self(pointer))
            }
        }

        fn load() -> Result<Self> {
            for candidate in database_candidates() {
                if !candidate.exists() {
                    continue;
                }
                let database = Self::create()?;
                if load_database_directory(&database, &candidate) {
                    log::info!(
                        "loaded bundled Lensfun database from {}",
                        candidate.display()
                    );
                    return Ok(database);
                }
            }

            let database = Self::create()?;
            // SAFETY: `database.0` is a live database owned by this guard.
            let result = unsafe { lf_db_load(database.0) };
            if result != LF_NO_ERROR {
                return Err(anyhow!("Lensfun database load failed with code {result}"));
            }
            Ok(database)
        }
    }

    fn load_database_directory(database: &Database, directory: &Path) -> bool {
        let Ok(entries) = std::fs::read_dir(directory) else {
            return false;
        };
        let mut files = entries
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| {
                path.extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("xml"))
            })
            .collect::<Vec<_>>();
        files.sort();
        if files.is_empty() {
            return false;
        }

        for file in files {
            let Ok(filename) = CString::new(file.to_string_lossy().as_bytes()) else {
                return false;
            };
            // SAFETY: the database and filename remain valid for the call.
            let result = unsafe { lf_db_load_file(database.0, filename.as_ptr()) };
            if result != LF_NO_ERROR {
                log::warn!(
                    "Lensfun could not load bundled database file {} (code {result})",
                    file.display()
                );
                return false;
            }
        }
        true
    }

    fn database_candidates() -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(configured) = std::env::var_os("AURAW_LENSFUN_DB") {
            roots.push(PathBuf::from(configured));
        }
        if let Ok(executable) = std::env::current_exe() {
            if let Some(directory) = executable.parent() {
                roots.push(directory.join("lensfun"));
                roots.push(directory.join("../share/auraw/lensfun"));
                roots.push(directory.join("../share/lensfun"));
            }
        }

        let mut candidates = Vec::new();
        for root in roots {
            push_database_candidates(&mut candidates, &root);
        }
        candidates
    }

    fn push_database_candidates(candidates: &mut Vec<PathBuf>, root: &Path) {
        for candidate in [
            root.join("version_2"),
            root.join("version_1"),
            root.to_owned(),
        ] {
            if !candidates.contains(&candidate) {
                candidates.push(candidate);
            }
        }
    }

    impl Drop for Database {
        fn drop(&mut self) {
            // SAFETY: the database was returned by lf_db_new and is destroyed once.
            unsafe { lf_db_destroy(self.0) };
        }
    }

    struct Modifier(*mut lfModifier);

    impl Drop for Modifier {
        fn drop(&mut self) {
            // SAFETY: the modifier was returned by lf_modifier_new and is destroyed once.
            unsafe { lf_modifier_destroy(self.0) };
        }
    }

    struct OwnedPointerList<T> {
        pointer: *mut *const T,
    }

    impl<T> OwnedPointerList<T> {
        fn first(&self) -> Option<*const T> {
            if self.pointer.is_null() {
                None
            } else {
                // SAFETY: Lensfun returns a null-terminated pointer list.
                let first = unsafe { *self.pointer };
                (!first.is_null()).then_some(first)
            }
        }

        fn values(&self) -> Vec<*const T> {
            pointer_list(self.pointer.cast_const())
        }
    }

    impl<T> Drop for OwnedPointerList<T> {
        fn drop(&mut self) {
            if !self.pointer.is_null() {
                // SAFETY: Lensfun documents that search result lists are released with lf_free.
                unsafe { lf_free(self.pointer.cast()) };
            }
        }
    }

    #[repr(C, align(16))]
    #[derive(Clone, Copy)]
    struct AlignedRgba([f32; 4]);

    pub(super) fn catalog(raw: &LoadedRaw) -> LensfunCatalog {
        match catalog_result(raw) {
            Ok(catalog) => catalog,
            Err(error) => LensfunCatalog {
                available: false,
                status: format!("Lensfun: {error:#}"),
                ..LensfunCatalog::default()
            },
        }
    }

    fn catalog_result(raw: &LoadedRaw) -> Result<LensfunCatalog> {
        let database = Database::load()?;
        let camera = find_camera(&database, &raw.camera_make, &raw.camera_model);
        let camera_label = camera
            .map(|camera| unsafe {
                format!(
                    "{} {}",
                    multilingual_string((*camera).maker),
                    multilingual_string((*camera).model)
                )
                .trim()
                .to_owned()
            })
            .unwrap_or_else(|| {
                let reported = format!("{} {}", raw.camera_make, raw.camera_model)
                    .trim()
                    .to_owned();
                if reported.is_empty() {
                    "Not reported".to_owned()
                } else {
                    format!("{reported} (no Lensfun camera match)")
                }
            });

        let mut lenses = camera
            .map(|camera| compatible_lenses(&database, camera))
            .unwrap_or_else(|| all_lenses(&database));
        sort_and_deduplicate_lenses(&mut lenses);

        let auto_match = find_auto_lens(&database, camera, raw);
        let status = if let Some(found) = &auto_match {
            format!("Auto-detected {} from RAW metadata", found.label())
        } else if raw.lens_model.trim().is_empty() {
            "The RAW file does not identify a lens. Select one manually.".to_owned()
        } else if camera.is_none() {
            format!(
                "No camera profile matched, and the lens ‘{}’ was not unambiguous. Select a profile manually.",
                raw.lens_model
            )
        } else {
            format!(
                "No Lensfun profile matched ‘{}’. Select one manually.",
                raw.lens_model
            )
        };

        Ok(LensfunCatalog {
            available: true,
            camera_label,
            lenses,
            auto_match,
            status,
        })
    }

    pub(super) fn apply(raw: &LoadedRaw, selection: &LensfunLens) -> Result<LoadedRaw> {
        let database = Database::load()?;
        let camera = find_camera(&database, &raw.camera_make, &raw.camera_model);
        let lens = find_lens(&database, camera, selection)
            .ok_or_else(|| anyhow!("Lensfun has no profile for {}", selection.label()))?;

        // SAFETY: camera/lens pointers are database-owned and valid for this scope.
        // If the camera is absent from the database, Lensfun's calibration crop
        // factor is the safest available fallback for manual correction.
        let crop = camera
            .map(|camera| unsafe { (*camera).crop_factor })
            .and_then(positive)
            .or_else(|| positive(unsafe { (*lens).crop_factor }))
            .unwrap_or(1.0);
        let focal = positive(raw.focal_length).unwrap_or_else(|| unsafe {
            let min = (*lens).min_focal;
            let max = (*lens).max_focal;
            if min.is_finite() && max.is_finite() && min > 0.0 && max >= min {
                0.5 * (min + max)
            } else {
                min.max(1.0)
            }
        });
        let width = c_int::try_from(raw.width).context("RAW width does not fit Lensfun")?;
        let height = c_int::try_from(raw.height).context("RAW height does not fit Lensfun")?;
        // SAFETY: all pointers and scalar parameters are valid. Lensfun 0.3.x
        // configures focal/aperture/distance through `lf_modifier_initialize`.
        let pointer = unsafe { lf_modifier_new(lens, crop.max(0.1), width, height) };
        if pointer.is_null() {
            return Err(anyhow!("Lensfun could not create a modifier"));
        }
        let modifier = Modifier(pointer);
        let aperture = positive(raw.aperture).unwrap_or(8.0);
        let distance = positive(raw.focus_distance).unwrap_or(1000.0);
        // SAFETY: modifier and lens are live. reverse=0 applies correction rather
        // than simulating defects, and the selected lens geometry is a valid enum.
        let mut flags = unsafe {
            lf_modifier_initialize(
                modifier.0,
                lens,
                LF_PF_F32,
                focal,
                aperture,
                distance,
                1.0,
                (*lens).lens_type,
                LF_MODIFY_TCA | LF_MODIFY_VIGNETTING | LF_MODIFY_DISTORTION,
                0,
            )
        };

        if flags & (LF_MODIFY_DISTORTION | LF_MODIFY_GEOMETRY | LF_MODIFY_TCA) != 0 {
            // SAFETY: Lensfun computes a scale for the configured live modifier.
            let scale = unsafe { lf_modifier_get_auto_scale(modifier.0, 0) };
            if scale.is_finite() && scale > 0.0 {
                // SAFETY: the callback is added to the same initialized modifier.
                let scaling_added =
                    unsafe { lf_modifier_add_coord_callback_scale(modifier.0, scale, 0) };
                if scaling_added != 0 {
                    flags |= LF_MODIFY_SCALE;
                }
            }
        }

        if flags
            & (LF_MODIFY_DISTORTION
                | LF_MODIFY_GEOMETRY
                | LF_MODIFY_TCA
                | LF_MODIFY_SCALE
                | LF_MODIFY_VIGNETTING)
            == 0
        {
            return Err(anyhow!(
                "the selected Lensfun profile contains no applicable correction data"
            ));
        }

        correct_mosaic(raw, &modifier, flags)
    }

    fn correct_mosaic(raw: &LoadedRaw, modifier: &Modifier, flags: c_int) -> Result<LoadedRaw> {
        let width = raw.width as usize;
        let height = raw.height as usize;
        let mut raw_pixels = vec![0u16; raw.raw_pixels.len()];
        // Geometric interpolation of a constant black level is still exactly
        // that same constant. Most modern Bayer RAWs (including the Sony ARWs
        // in the loading benchmarks) use one uniform value, so avoid allocating
        // and writing a dense f32 map with tens of millions of identical entries.
        let uniform_black = raw
            .black_levels_per_pixel
            .storage_slice()
            .first()
            .copied()
            .filter(|first| {
                raw.black_levels_per_pixel
                    .storage_slice()
                    .iter()
                    .all(|value| value == first)
            });
        let mut black_levels_per_pixel = uniform_black
            .is_none()
            .then(|| vec![0.0f32; raw.black_levels_per_pixel.len()]);
        let coordinate_enabled = flags
            & (LF_MODIFY_DISTORTION | LF_MODIFY_GEOMETRY | LF_MODIFY_TCA | LF_MODIFY_SCALE)
            != 0;
        let vignette_enabled = flags & LF_MODIFY_VIGNETTING != 0;
        // Lensfun's modifier is used serially to generate mapping rows, but the
        // expensive CFA resampling is independent per output pixel. Batch a
        // modest number of mapping rows before entering Rayon so we do not pay
        // the cost of starting a parallel job once for every sensor row.
        // 32 rows keeps the temporary coordinate buffer below ~6 MiB even for
        // a 7k-wide RAW while reducing thousands of Rayon dispatches to a few
        // hundred per image.
        const ROW_BATCH: usize = 32;
        let coordinate_row_len = width.saturating_mul(6);
        let mut coordinates = vec![0.0f32; coordinate_row_len.saturating_mul(ROW_BATCH)];
        let vignette_started = std::time::Instant::now();
        let vignette_gains = if vignette_enabled {
            build_vignette_gain_map(raw, modifier)?
        } else {
            Vec::new()
        };
        if vignette_enabled {
            crate::diagnostics::record(format!(
                "Lensfun vignette gain map prepared in {:.3}s",
                vignette_started.elapsed().as_secs_f64()
            ));
        }

        let warp_started = std::time::Instant::now();
        for batch_y in (0..height).step_by(ROW_BATCH) {
            let batch_rows = (height - batch_y).min(ROW_BATCH);
            let coordinate_len = batch_rows.saturating_mul(coordinate_row_len);
            let coordinate_batch = &mut coordinates[..coordinate_len];

            if coordinate_enabled {
                // Lensfun accepts a rectangular block and emits six floats per
                // output pixel (R/G/B x/y). Generate the whole batch in one FFI
                // call instead of invoking Lensfun once for every sensor row.
                // SAFETY: the output buffer has width*batch_rows*2*3 floats.
                let filled = unsafe {
                    lf_modifier_apply_subpixel_geometry_distortion(
                        modifier.0,
                        0.0,
                        batch_y as f32,
                        raw.width as c_int,
                        batch_rows as c_int,
                        coordinate_batch.as_mut_ptr(),
                    )
                };
                if filled == 0 {
                    for local_y in 0..batch_rows {
                        let y = batch_y + local_y;
                        let row_coordinates = &mut coordinate_batch
                            [local_y * coordinate_row_len..(local_y + 1) * coordinate_row_len];
                        fill_identity_coordinates(row_coordinates, y, width);
                    }
                }
            } else {
                for local_y in 0..batch_rows {
                    let y = batch_y + local_y;
                    let row_coordinates = &mut coordinate_batch
                        [local_y * coordinate_row_len..(local_y + 1) * coordinate_row_len];
                    fill_identity_coordinates(row_coordinates, y, width);
                }
            }

            let pixel_start = batch_y * width;
            let pixel_end = pixel_start + batch_rows * width;
            if let Some(output_black_map) = black_levels_per_pixel.as_mut() {
                raw_pixels[pixel_start..pixel_end]
                    .par_iter_mut()
                    .zip(output_black_map[pixel_start..pixel_end].par_iter_mut())
                    .enumerate()
                    .for_each(|(local_index, (output_sample, output_black))| {
                        let local_y = local_index / width;
                        let x = local_index % width;
                        let y = batch_y + local_y;
                        let output_index = pixel_start + local_index;
                        let cfa_index = raw.color_indices[output_index];
                        let channel = lensfun_rgb_channel(cfa_index);
                        let coordinate_index = local_y * coordinate_row_len + x * 6 + channel * 2;
                        let source_x = coordinate_batch[coordinate_index];
                        let source_y = coordinate_batch[coordinate_index + 1];
                        let (corrected, black) = sample_corrected_cfa_subpixel(
                            raw,
                            source_x,
                            source_y,
                            cfa_index,
                            x,
                            y,
                            vignette_enabled,
                            &vignette_gains,
                        );
                        *output_sample = corrected.round().clamp(0.0, f32::from(u16::MAX)) as u16;
                        *output_black = black;
                    });
            } else {
                raw_pixels[pixel_start..pixel_end]
                    .par_iter_mut()
                    .enumerate()
                    .for_each(|(local_index, output_sample)| {
                        let local_y = local_index / width;
                        let x = local_index % width;
                        let y = batch_y + local_y;
                        let output_index = pixel_start + local_index;
                        let cfa_index = raw.color_indices[output_index];
                        let channel = lensfun_rgb_channel(cfa_index);
                        let coordinate_index = local_y * coordinate_row_len + x * 6 + channel * 2;
                        let source_x = coordinate_batch[coordinate_index];
                        let source_y = coordinate_batch[coordinate_index + 1];
                        let (corrected, _black) = sample_corrected_cfa_subpixel(
                            raw,
                            source_x,
                            source_y,
                            cfa_index,
                            x,
                            y,
                            vignette_enabled,
                            &vignette_gains,
                        );
                        *output_sample = corrected.round().clamp(0.0, f32::from(u16::MAX)) as u16;
                    });
            }
        }
        crate::diagnostics::record(format!(
            "Lensfun coordinate warp/CFA resample finished in {:.3}s",
            warp_started.elapsed().as_secs_f64()
        ));

        Ok(LoadedRaw {
            width: raw.width,
            height: raw.height,
            camera_make: raw.camera_make.clone(),
            camera_model: raw.camera_model.clone(),
            lens_make: raw.lens_make.clone(),
            lens_model: raw.lens_model.clone(),
            focal_length: raw.focal_length,
            aperture: raw.aperture,
            focus_distance: raw.focus_distance,
            capture_metadata: raw.capture_metadata.clone(),
            cfa_kind: raw.cfa_kind,
            raw_pixels,
            color_indices: raw.color_indices.clone(),
            wb_coeffs: raw.wb_coeffs,
            cam_to_srgb: raw.cam_to_srgb,
            black_levels: raw.black_levels,
            black_levels_per_pixel: if let Some(black) = uniform_black {
                CompactPixelMap::repeating(raw.width, raw.height, 1, 1, vec![black])
            } else {
                CompactPixelMap::compact_from_dense(
                    raw.width,
                    raw.height,
                    black_levels_per_pixel.expect("non-uniform black map must be materialized"),
                    64,
                )
            },
            white_levels: raw.white_levels,
            camera_profile: raw.camera_profile.clone(),
            camera_profile_source: raw.camera_profile_source.clone(),
            available_camera_profiles: raw.available_camera_profiles.clone(),
            white_balance_model: raw.white_balance_model.clone(),
        })
    }

    fn build_vignette_gain_map(raw: &LoadedRaw, modifier: &Modifier) -> Result<Vec<f32>> {
        let width = raw.width as usize;
        let height = raw.height as usize;
        let row_stride = c_int::try_from(width.saturating_mul(std::mem::size_of::<AlignedRgba>()))
            .context("Lensfun vignette row stride overflow")?;
        const ROW_BATCH: usize = 32;
        let mut rgba_gains = vec![AlignedRgba([1.0; 4]); width.saturating_mul(ROW_BATCH)];
        let mut gains = vec![1.0f32; raw.raw_pixels.len()];

        for batch_y in (0..height).step_by(ROW_BATCH) {
            let batch_rows = (height - batch_y).min(ROW_BATCH);
            let batch_len = batch_rows * width;
            let rgba_batch = &mut rgba_gains[..batch_len];
            rgba_batch.fill(AlignedRgba([1.0; 4]));

            // Apply vignetting to the whole rectangular batch. The buffer
            // begins at unity, so if Lensfun reports no modification the data
            // already represents the exact no-op gain map.
            // SAFETY: the batch contains batch_rows rows of aligned RGBA f32
            // values with the supplied row stride.
            let _applied = unsafe {
                lf_modifier_apply_color_modification(
                    modifier.0,
                    rgba_batch.as_mut_ptr().cast(),
                    0.0,
                    batch_y as f32,
                    raw.width as c_int,
                    batch_rows as c_int,
                    LF_CR_RGBA,
                    row_stride,
                )
            };

            let pixel_start = batch_y * width;
            let pixel_end = pixel_start + batch_len;
            gains[pixel_start..pixel_end]
                .par_iter_mut()
                .zip(rgba_batch.par_iter())
                .enumerate()
                .for_each(|(local_index, (gain, rgba_gain))| {
                    let index = pixel_start + local_index;
                    let channel = lensfun_rgb_channel(raw.color_indices[index]);
                    *gain = rgba_gain.0[channel];
                });
        }

        Ok(gains)
    }

    fn lensfun_rgb_channel(cfa_index: u8) -> usize {
        match cfa_index {
            0 => 0,
            2 => 2,
            // LibRaw uses both 1 and 3 for the two Bayer green phases.
            // Lensfun has one green coordinate/gain channel, while sampling
            // below still preserves the exact CFA phase.
            _ => 1,
        }
    }

    fn fill_identity_coordinates(coordinates: &mut [f32], y: usize, width: usize) {
        for x in 0..width {
            let base = x * 6;
            for channel in 0..3 {
                coordinates[base + channel * 2] = x as f32;
                coordinates[base + channel * 2 + 1] = y as f32;
            }
        }
    }

    fn sample_corrected_cfa_subpixel(
        raw: &LoadedRaw,
        x: f32,
        y: f32,
        channel: u8,
        output_x: usize,
        output_y: usize,
        vignette_enabled: bool,
        vignette_gains: &[f32],
    ) -> (f32, f32) {
        if raw.cfa_kind == crate::pipeline::CfaKind::Bayer && x.is_finite() && y.is_finite() {
            if let Some(sample) = sample_bayer_phase_bilinear(
                raw,
                x,
                y,
                channel,
                output_x,
                output_y,
                vignette_enabled,
                vignette_gains,
            ) {
                return sample;
            }
        }

        // X-Trans has an irregular 6x6 color layout, so keep the conservative
        // same-color fallback there. Bayer is the common path and receives true
        // subpixel interpolation below, which removes the nearest-neighbor
        // stair-steps and color breaks that were visible after Lensfun warping.
        let source_index = nearest_matching_sample(raw, x, y, channel);
        corrected_sample_at(raw, source_index, vignette_enabled, vignette_gains)
    }

    fn sample_bayer_phase_bilinear(
        raw: &LoadedRaw,
        x: f32,
        y: f32,
        channel: u8,
        output_x: usize,
        output_y: usize,
        vignette_enabled: bool,
        vignette_gains: &[f32],
    ) -> Option<(f32, f32)> {
        let (x0, x1, tx) = bayer_axis_samples(x, raw.width, (output_x as u32) & 1)?;
        let (y0, y1, ty) = bayer_axis_samples(y, raw.height, (output_y as u32) & 1)?;
        let indices = [
            (y0 * raw.width + x0) as usize,
            (y0 * raw.width + x1) as usize,
            (y1 * raw.width + x0) as usize,
            (y1 * raw.width + x1) as usize,
        ];
        if indices
            .iter()
            .any(|&index| raw.color_indices.get(index).copied() != Some(channel))
        {
            return None;
        }

        let a = corrected_sample_at(raw, indices[0], vignette_enabled, vignette_gains);
        let b = corrected_sample_at(raw, indices[1], vignette_enabled, vignette_gains);
        let c = corrected_sample_at(raw, indices[2], vignette_enabled, vignette_gains);
        let d = corrected_sample_at(raw, indices[3], vignette_enabled, vignette_gains);
        let top = (lerp(a.0, b.0, tx), lerp(a.1, b.1, tx));
        let bottom = (lerp(c.0, d.0, tx), lerp(c.1, d.1, tx));
        Some((
            lerp(top.0, bottom.0, ty),
            lerp(top.1, bottom.1, ty),
        ))
    }

    fn bayer_axis_samples(coordinate: f32, extent: u32, phase: u32) -> Option<(u32, u32, f32)> {
        if extent == 0 {
            return None;
        }
        let maximum = extent - 1;
        let first = phase.min(maximum);
        if first > maximum {
            return None;
        }
        let last = first + ((maximum - first) / 2) * 2;
        if first == last {
            return Some((first, first, 0.0));
        }

        let lattice = ((coordinate - first as f32) * 0.5)
            .clamp(0.0, ((last - first) / 2) as f32);
        let lower_step = lattice.floor() as u32;
        let upper_step = (lower_step + 1).min((last - first) / 2);
        let lower = first + lower_step * 2;
        let upper = first + upper_step * 2;
        let mix = if lower == upper {
            0.0
        } else {
            ((coordinate - lower as f32) / (upper - lower) as f32).clamp(0.0, 1.0)
        };
        Some((lower, upper, mix))
    }

    fn corrected_sample_at(
        raw: &LoadedRaw,
        index: usize,
        vignette_enabled: bool,
        vignette_gains: &[f32],
    ) -> (f32, f32) {
        let black = raw.black_levels_per_pixel[index];
        let sample = f32::from(raw.raw_pixels[index]);
        let gain = if vignette_enabled {
            vignette_gains.get(index).copied().unwrap_or(1.0).max(0.0)
        } else {
            1.0
        };
        (black + (sample - black).max(0.0) * gain, black)
    }

    fn lerp(left: f32, right: f32, amount: f32) -> f32 {
        left + (right - left) * amount
    }

    fn nearest_matching_sample(raw: &LoadedRaw, x: f32, y: f32, channel: u8) -> usize {
        let max_x = raw.width.saturating_sub(1) as i32;
        let max_y = raw.height.saturating_sub(1) as i32;
        let center_x = x.round().clamp(0.0, max_x as f32) as i32;
        let center_y = y.round().clamp(0.0, max_y as f32) as i32;
        let center = center_y as usize * raw.width as usize + center_x as usize;
        if raw.color_indices[center] == channel {
            return center;
        }

        let radius_limit: i32 = match raw.cfa_kind {
            crate::pipeline::CfaKind::Bayer => 3,
            crate::pipeline::CfaKind::XTrans => 6,
        };
        let mut best = center;
        let mut best_distance = f32::INFINITY;
        for radius in 1..=radius_limit {
            for dy in -radius..=radius {
                for dx in -radius..=radius {
                    if dx.abs() != radius && dy.abs() != radius {
                        continue;
                    }
                    let sample_x = (center_x + dx).clamp(0, max_x);
                    let sample_y = (center_y + dy).clamp(0, max_y);
                    let index = sample_y as usize * raw.width as usize + sample_x as usize;
                    if raw.color_indices[index] != channel {
                        continue;
                    }
                    let distance = (sample_x as f32 - x).powi(2) + (sample_y as f32 - y).powi(2);
                    if distance < best_distance {
                        best = index;
                        best_distance = distance;
                    }
                }
            }
            if best_distance.is_finite() {
                return best;
            }
        }
        best
    }

    fn find_camera(database: &Database, make: &str, model: &str) -> Option<*const lfCamera> {
        if model.trim().is_empty() {
            return None;
        }
        let make = CString::new(make).ok()?;
        let model = CString::new(model).ok()?;
        // SAFETY: database and C strings are valid for the duration of the call.
        let exact = OwnedPointerList {
            pointer: unsafe { lf_db_find_cameras(database.0, make.as_ptr(), model.as_ptr()) },
        };
        if let Some(camera) = exact.first() {
            return Some(camera);
        }
        // SAFETY: same as above; loose search is used only as a fallback.
        let loose = OwnedPointerList {
            pointer: unsafe {
                lf_db_find_cameras_ext(
                    database.0,
                    make.as_ptr(),
                    model.as_ptr(),
                    LF_SEARCH_LOOSE | LF_SEARCH_SORT_AND_UNIQUIFY,
                )
            },
        };
        loose.first()
    }

    fn compatible_lenses(database: &Database, camera: *const lfCamera) -> Vec<LensfunLens> {
        // An empty model query asks Lensfun for the camera-compatible choices.
        let empty = CString::new("").expect("an empty string contains no NUL byte");
        // SAFETY: database, camera, and the empty C string remain live throughout the search.
        let list = OwnedPointerList {
            pointer: unsafe {
                lf_db_find_lenses_hd(
                    database.0,
                    camera,
                    ptr::null(),
                    empty.as_ptr(),
                    LF_SEARCH_SORT_AND_UNIQUIFY,
                )
            },
        };
        let values = list
            .values()
            .into_iter()
            .filter_map(lens_name)
            .collect::<Vec<_>>();
        if !values.is_empty() {
            return values;
        }

        // Older Lensfun builds may not treat an empty description as an
        // enumeration request. Verify each database lens against the camera
        // so the manual dropdown still excludes incompatible mounts.
        // SAFETY: this pointer list is database-owned until `database` drops.
        pointer_list(unsafe { lf_db_get_lenses(database.0) })
            .into_iter()
            .filter_map(lens_name)
            .filter(|lens| find_lens(database, Some(camera), lens).is_some())
            .collect()
    }

    fn all_lenses(database: &Database) -> Vec<LensfunLens> {
        // SAFETY: this pointer list is database-owned until `database` drops.
        pointer_list(unsafe { lf_db_get_lenses(database.0) })
            .into_iter()
            .filter_map(lens_name)
            .collect()
    }

    fn sort_and_deduplicate_lenses(lenses: &mut Vec<LensfunLens>) {
        lenses.sort_by(|left, right| {
            left.maker
                .to_lowercase()
                .cmp(&right.maker.to_lowercase())
                .then_with(|| left.model.to_lowercase().cmp(&right.model.to_lowercase()))
        });
        lenses.dedup_by(|left, right| {
            left.maker.eq_ignore_ascii_case(&right.maker)
                && left.model.eq_ignore_ascii_case(&right.model)
        });
    }

    fn find_auto_lens(
        database: &Database,
        camera: Option<*const lfCamera>,
        raw: &LoadedRaw,
    ) -> Option<LensfunLens> {
        if raw.lens_model.trim().is_empty() {
            return None;
        }

        let mut candidates = if let Some(camera) = camera {
            let maker = if raw.lens_make.trim().is_empty() {
                None
            } else {
                CString::new(raw.lens_make.trim()).ok()
            };
            let model = CString::new(raw.lens_model.trim()).ok()?;
            // SAFETY: all pointers remain valid during the search.
            let list = OwnedPointerList {
                pointer: unsafe {
                    lf_db_find_lenses_hd(
                        database.0,
                        camera,
                        maker.as_ref().map_or(ptr::null(), |value| value.as_ptr()),
                        model.as_ptr(),
                        LF_SEARCH_LOOSE | LF_SEARCH_SORT_AND_UNIQUIFY,
                    )
                },
            };
            list.values()
                .into_iter()
                .filter_map(lens_name)
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Some cameras write punctuation, marketing suffixes, or a maker prefix
        // differently from Lensfun. If Lensfun's loose query returns nothing,
        // score every camera-compatible profile instead of silently giving up.
        if candidates.is_empty() {
            candidates = camera
                .map(|camera| compatible_lenses(database, camera))
                .unwrap_or_else(|| all_lenses(database));
        }
        sort_and_deduplicate_lenses(&mut candidates);

        let exact = candidates
            .iter()
            .filter(|candidate| lens_metadata_is_exact(raw, candidate))
            .cloned()
            .collect::<Vec<_>>();
        if exact.len() == 1 {
            return exact.into_iter().next();
        }

        // A camera-constrained Lensfun search that yields one profile is itself
        // an unambiguous metadata match, provided the capture focal length does
        // not contradict that profile's calibrated range.
        if camera.is_some()
            && candidates.len() == 1
            && profile_supports_capture(database, camera, raw, &candidates[0])
        {
            return candidates.into_iter().next();
        }

        let mut ranked = candidates
            .into_iter()
            .filter_map(|candidate| {
                automatic_match_score(database, camera, raw, &candidate)
                    .map(|score| (score, candidate))
            })
            .collect::<Vec<_>>();
        ranked.sort_by(|left, right| {
            right
                .0
                .partial_cmp(&left.0)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let (best_score, best) = ranked.first()?;
        if *best_score < 0.90 {
            return None;
        }
        if let Some((runner_up, _)) = ranked.get(1) {
            if *best_score - *runner_up < 0.08 {
                return None;
            }
        }
        Some(best.clone())
    }

    fn lens_metadata_is_exact(raw: &LoadedRaw, candidate: &LensfunLens) -> bool {
        if !maker_is_compatible(&raw.lens_make, &raw.lens_model, &candidate.maker) {
            return false;
        }
        canonical_lens_model(
            &raw.lens_model,
            &[raw.lens_make.as_str(), candidate.maker.as_str()],
        ) == canonical_lens_model(
            &candidate.model,
            &[candidate.maker.as_str(), raw.lens_make.as_str()],
        )
    }

    fn automatic_match_score(
        database: &Database,
        camera: Option<*const lfCamera>,
        raw: &LoadedRaw,
        candidate: &LensfunLens,
    ) -> Option<f32> {
        if !maker_is_compatible(&raw.lens_make, &raw.lens_model, &candidate.maker)
            || !profile_supports_capture(database, camera, raw, candidate)
        {
            return None;
        }

        let raw_model = canonical_lens_model(
            &raw.lens_model,
            &[raw.lens_make.as_str(), candidate.maker.as_str()],
        );
        let candidate_model = canonical_lens_model(
            &candidate.model,
            &[candidate.maker.as_str(), raw.lens_make.as_str()],
        );
        if raw_model.is_empty() || candidate_model.is_empty() {
            return None;
        }
        if raw_model == candidate_model {
            return Some(1.0);
        }

        // Camera metadata often reports a compact mount-prefixed description,
        // while Lensfun stores the full retail name. For example, Sony writes
        // “E 28-75mm F2.8 A063” for Lensfun's “Tamron 28-75mm F2.8 Di III
        // VXD G2 (A063)”. A shared manufacturer model code is substantially
        // more identifying than the surrounding marketing words. Camera/mount
        // compatibility and focal-range validation have already succeeded
        // above, so one shared code is a safe high-confidence match.
        let raw_codes = lens_model_codes(&raw.lens_model);
        let candidate_codes = lens_model_codes(&candidate.model);
        if raw_codes
            .iter()
            .any(|code| candidate_codes.iter().any(|other| other == code))
        {
            return Some(0.995);
        }

        if raw_model.len().min(candidate_model.len()) >= 8
            && (raw_model.contains(&candidate_model) || candidate_model.contains(&raw_model))
        {
            return Some(0.96);
        }

        let raw_tokens = lens_tokens(&raw.lens_model, &raw.lens_make, &candidate.maker);
        let candidate_tokens = lens_tokens(&candidate.model, &candidate.maker, &raw.lens_make);
        if raw_tokens.is_empty() || candidate_tokens.is_empty() {
            return None;
        }
        let numeric_tokens = raw_tokens
            .iter()
            .filter(|token| token.chars().any(|character| character.is_ascii_digit()))
            .collect::<Vec<_>>();
        if !numeric_tokens.is_empty()
            && !numeric_tokens
                .iter()
                .all(|token| candidate_tokens.iter().any(|other| other == *token))
        {
            return None;
        }
        let shared = raw_tokens
            .iter()
            .filter(|token| candidate_tokens.iter().any(|other| other == *token))
            .count();
        let similarity = shared as f32 / raw_tokens.len().max(candidate_tokens.len()) as f32;
        (similarity >= 0.75).then_some(0.78 + similarity * 0.18)
    }

    fn profile_supports_capture(
        database: &Database,
        camera: Option<*const lfCamera>,
        raw: &LoadedRaw,
        candidate: &LensfunLens,
    ) -> bool {
        // Without a camera match, resolving each candidate back through the
        // complete database would be quadratic. The normalized metadata score
        // remains conservative in that case; focal-range validation is added
        // whenever Lensfun identified the camera and mount.
        if camera.is_none() {
            return true;
        }
        let Some(focal) = positive(raw.focal_length) else {
            return true;
        };
        let Some(lens) = find_lens(database, camera, candidate) else {
            return true;
        };
        // SAFETY: `lens` is database-owned and remains valid for this scope.
        let (min_focal, max_focal) = unsafe { ((*lens).min_focal, (*lens).max_focal) };
        if !min_focal.is_finite()
            || !max_focal.is_finite()
            || min_focal <= 0.0
            || max_focal < min_focal
        {
            return true;
        }
        let tolerance = (0.03 * max_focal).max(0.75);
        focal >= min_focal - tolerance && focal <= max_focal + tolerance
    }

    fn maker_is_compatible(raw_maker: &str, raw_model: &str, candidate_maker: &str) -> bool {
        let raw = canonical_text(raw_maker);
        if raw.is_empty() {
            return true;
        }
        let candidate = canonical_text(candidate_maker);
        if candidate.is_empty() {
            return true;
        }
        raw == candidate
            || raw.contains(&candidate)
            || candidate.contains(&raw)
            || canonical_text(raw_model).starts_with(&candidate)
    }

    fn canonical_lens_model(model: &str, makers: &[&str]) -> String {
        let mut canonical = canonical_text(model);
        for maker in makers {
            let maker = canonical_text(maker);
            if !maker.is_empty() && canonical.starts_with(&maker) && canonical.len() > maker.len() {
                canonical = canonical[maker.len()..].to_owned();
            }
        }
        canonical
    }

    fn canonical_text(value: &str) -> String {
        value
            .chars()
            .flat_map(char::to_lowercase)
            .filter(|character| character.is_alphanumeric())
            .collect()
    }

    fn lens_model_codes(model: &str) -> Vec<String> {
        let mut codes = tokenized(model)
            .into_iter()
            .filter(|token| token.len() >= 3)
            .filter(|token| {
                token
                    .chars()
                    .any(|character| character.is_ascii_alphabetic())
            })
            .filter(|token| token.chars().any(|character| character.is_ascii_digit()))
            // Optical specifications are not manufacturer identifiers.
            .filter(|token| !token.ends_with("mm"))
            .filter(|token| {
                !token.strip_prefix('f').is_some_and(|suffix| {
                    !suffix.is_empty() && suffix.chars().all(|character| character.is_ascii_digit())
                })
            })
            .collect::<Vec<_>>();
        codes.sort();
        codes.dedup();
        codes
    }

    fn lens_tokens(model: &str, primary_maker: &str, alternate_maker: &str) -> Vec<String> {
        let maker_tokens = tokenized(primary_maker)
            .into_iter()
            .chain(tokenized(alternate_maker))
            .collect::<Vec<_>>();
        let mut tokens = tokenized(model)
            .into_iter()
            .filter(|token| token != "lens")
            .filter(|token| !maker_tokens.iter().any(|maker| maker == token))
            .collect::<Vec<_>>();
        tokens.sort();
        tokens.dedup();
        tokens
    }

    fn tokenized(value: &str) -> Vec<String> {
        let mut result = Vec::new();
        let mut token = String::new();
        for character in value.chars().flat_map(char::to_lowercase) {
            if character.is_alphanumeric() {
                token.push(character);
            } else if !token.is_empty() {
                result.push(std::mem::take(&mut token));
            }
        }
        if !token.is_empty() {
            result.push(token);
        }
        result
    }

    fn find_lens(
        database: &Database,
        camera: Option<*const lfCamera>,
        selection: &LensfunLens,
    ) -> Option<*const lfLens> {
        if let Some(camera) = camera {
            let maker = CString::new(selection.maker.as_str()).ok()?;
            let model = CString::new(selection.model.as_str()).ok()?;
            // SAFETY: all pointers remain valid during the search.
            let list = OwnedPointerList {
                pointer: unsafe {
                    lf_db_find_lenses_hd(
                        database.0,
                        camera,
                        maker.as_ptr(),
                        model.as_ptr(),
                        LF_SEARCH_SORT_AND_UNIQUIFY,
                    )
                },
            };
            if let Some(lens) = list.first() {
                return Some(lens);
            }
        }

        find_lens_in_database(database, selection)
    }

    fn find_lens_in_database(
        database: &Database,
        selection: &LensfunLens,
    ) -> Option<*const lfLens> {
        // SAFETY: this pointer list is database-owned until `database` drops.
        pointer_list(unsafe { lf_db_get_lenses(database.0) })
            .into_iter()
            .find(|pointer| {
                lens_name(*pointer).is_some_and(|candidate| {
                    candidate.model.eq_ignore_ascii_case(&selection.model)
                        && (selection.maker.trim().is_empty()
                            || candidate.maker.eq_ignore_ascii_case(&selection.maker))
                })
            })
    }

    fn lens_name(pointer: *const lfLens) -> Option<LensfunLens> {
        if pointer.is_null() {
            return None;
        }
        // SAFETY: pointer is database-owned and valid for this read.
        let (maker, model) = unsafe {
            (
                multilingual_string((*pointer).maker),
                multilingual_string((*pointer).model),
            )
        };
        if maker.is_empty() && model.is_empty() {
            None
        } else {
            Some(LensfunLens { maker, model })
        }
    }

    fn pointer_list<T>(pointer: *const *const T) -> Vec<*const T> {
        if pointer.is_null() {
            return Vec::new();
        }
        let mut result = Vec::new();
        let mut index = 0usize;
        loop {
            // SAFETY: Lensfun returns null-terminated pointer arrays.
            let value = unsafe { *pointer.add(index) };
            if value.is_null() {
                break;
            }
            result.push(value);
            index += 1;
        }
        result
    }

    unsafe fn multilingual_string(pointer: *const c_char) -> String {
        if pointer.is_null() {
            return String::new();
        }
        let localized = lf_mlstr_get(pointer);
        if localized.is_null() {
            String::new()
        } else {
            CStr::from_ptr(localized)
                .to_string_lossy()
                .trim()
                .to_owned()
        }
    }

    fn positive(value: f32) -> Option<f32> {
        (value.is_finite() && value > 0.0).then_some(value)
    }
}
