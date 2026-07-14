use super::LoadedRaw;
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
                    log::info!("loaded bundled Lensfun database from {}", candidate.display());
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
        for candidate in [root.join("version_2"), root.join("version_1"), root.to_owned()] {
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

        let auto_match = find_auto_lens(&database, camera, raw).or_else(|| {
            (raw.lens_model.trim().is_empty() && lenses.len() == 1).then(|| lenses[0].clone())
        });
        let status = if let Some(found) = &auto_match {
            format!("Matched {}", found.label())
        } else if raw.lens_model.trim().is_empty() {
            "The RAW file does not identify a lens. Select one manually.".to_owned()
        } else if camera.is_none() {
            format!(
                "No camera profile matched, and the lens ‘{}’ was not unambiguous. Select a profile manually.",
                raw.lens_model
            )
        } else {
            format!("No Lensfun profile matched ‘{}’. Select one manually.", raw.lens_model)
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
                let scaling_added = unsafe {
                    lf_modifier_add_coord_callback_scale(modifier.0, scale, 0)
                };
                if scaling_added != 0 {
                    flags |= LF_MODIFY_SCALE;
                }
            }
        }

        if flags & (LF_MODIFY_DISTORTION | LF_MODIFY_GEOMETRY | LF_MODIFY_TCA | LF_MODIFY_SCALE | LF_MODIFY_VIGNETTING) == 0 {
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
        let mut black_levels_per_pixel = vec![0.0f32; raw.black_levels_per_pixel.len()];
        let coordinate_enabled = flags
            & (LF_MODIFY_DISTORTION | LF_MODIFY_GEOMETRY | LF_MODIFY_TCA | LF_MODIFY_SCALE)
            != 0;
        let vignette_enabled = flags & LF_MODIFY_VIGNETTING != 0;
        let mut coordinates = vec![0.0f32; width.saturating_mul(6)];
        let vignette_gains = if vignette_enabled {
            build_vignette_gain_map(raw, modifier)?
        } else {
            Vec::new()
        };

        for y in 0..height {
            if coordinate_enabled {
                // SAFETY: the output buffer has width*1*2*3 floats as required.
                let filled = unsafe {
                    lf_modifier_apply_subpixel_geometry_distortion(
                        modifier.0,
                        0.0,
                        y as f32,
                        raw.width as c_int,
                        1,
                        coordinates.as_mut_ptr(),
                    )
                };
                if filled == 0 {
                    fill_identity_coordinates(&mut coordinates, y, width);
                }
            } else {
                fill_identity_coordinates(&mut coordinates, y, width);
            }

            for x in 0..width {
                let output_index = y * width + x;
                let cfa_index = raw.color_indices[output_index];
                let channel = lensfun_rgb_channel(cfa_index);
                let coordinate_index = x * 6 + channel * 2;
                let source_x = coordinates[coordinate_index];
                let source_y = coordinates[coordinate_index + 1];
                let source_index = nearest_matching_sample(raw, source_x, source_y, cfa_index);
                let black = raw.black_levels_per_pixel[source_index];
                let sample = f32::from(raw.raw_pixels[source_index]);
                let gain = if vignette_enabled {
                    vignette_gains[source_index].max(0.0)
                } else {
                    1.0
                };
                let corrected = black + (sample - black).max(0.0) * gain;
                raw_pixels[output_index] = corrected.round().clamp(0.0, f32::from(u16::MAX)) as u16;
                black_levels_per_pixel[output_index] = black;
            }
        }

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
            cfa_kind: raw.cfa_kind,
            raw_pixels,
            color_indices: raw.color_indices.clone(),
            wb_coeffs: raw.wb_coeffs,
            cam_to_srgb: raw.cam_to_srgb,
            black_levels: raw.black_levels,
            black_levels_per_pixel,
            white_levels: raw.white_levels,
            camera_profile: raw.camera_profile.clone(),
            white_balance_model: raw.white_balance_model.clone(),
        })
    }

    fn build_vignette_gain_map(raw: &LoadedRaw, modifier: &Modifier) -> Result<Vec<f32>> {
        let width = raw.width as usize;
        let height = raw.height as usize;
        let row_stride = c_int::try_from(
            width.saturating_mul(std::mem::size_of::<AlignedRgba>()),
        )
        .context("Lensfun vignette row stride overflow")?;
        let mut rgba_gains = vec![AlignedRgba([1.0; 4]); width];
        let mut gains = vec![1.0f32; raw.raw_pixels.len()];

        for y in 0..height {
            rgba_gains.fill(AlignedRgba([1.0; 4]));
            // SAFETY: AlignedRgba is 16-byte aligned and contains four f32
            // components. Lensfun modifies only this live row buffer.
            let applied = unsafe {
                lf_modifier_apply_color_modification(
                    modifier.0,
                    rgba_gains.as_mut_ptr().cast(),
                    0.0,
                    y as f32,
                    raw.width as c_int,
                    1,
                    LF_CR_RGBA,
                    row_stride,
                )
            };
            if applied == 0 {
                continue;
            }
            for x in 0..width {
                let index = y * width + x;
                let channel = lensfun_rgb_channel(raw.color_indices[index]);
                gains[index] = rgba_gains[x].0[channel];
            }
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
        let candidates = if let Some(camera) = camera {
            let maker = CString::new(raw.lens_make.as_str()).ok();
            let model = CString::new(raw.lens_model.as_str()).ok()?;
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
            all_lenses(database)
        };

        let normalized_model = raw.lens_model.trim();
        let normalized_maker = raw.lens_make.trim();
        let exact = candidates
            .iter()
            .filter(|candidate| {
                candidate.model.trim().eq_ignore_ascii_case(normalized_model)
                    && (normalized_maker.is_empty()
                        || candidate
                            .maker
                            .trim()
                            .eq_ignore_ascii_case(normalized_maker))
            })
            .cloned()
            .collect::<Vec<_>>();
        if exact.len() == 1 {
            exact.into_iter().next()
        } else if camera.is_some() && candidates.len() == 1 {
            candidates.into_iter().next()
        } else {
            None
        }
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
            CStr::from_ptr(localized).to_string_lossy().trim().to_owned()
        }
    }

    fn positive(value: f32) -> Option<f32> {
        (value.is_finite() && value > 0.0).then_some(value)
    }

}
