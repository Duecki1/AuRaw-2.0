use super::*;
use std::cell::RefCell;


std::thread_local! {
    // Queue::write_texture copies the supplied bytes before returning, so these
    // bounded per-thread staging vectors can be safely reused for every tile.
    // This removes repeated multi-megabyte allocations from tiled export.
    static BLACK_UPLOAD_SCRATCH: RefCell<Vec<f32>> = RefCell::new(Vec::new());
    static COLOR_UPLOAD_SCRATCH: RefCell<Vec<u8>> = RefCell::new(Vec::new());
}

pub(super) fn tone_analysis_scale() -> u32 {
    if cfg!(target_os = "android") {
        8
    } else {
        4
    }
}

pub(super) fn tone_guide_format() -> wgpu::TextureFormat {
    // The guide is reduced-resolution, so R32Float costs little even on
    // Android and avoids optional R16Float storage-texture support.
    wgpu::TextureFormat::R32Float
}

pub(super) fn default_processing_quality() -> ProcessingQuality {
    ProcessingQuality::Preview
}

pub(super) fn estimated_gpu_working_set_bytes(
    width: u32,
    height: u32,
    quality: ProcessingQuality,
) -> Option<u64> {
    let pixels = u64::from(width).checked_mul(u64::from(height))?;
    let work_bytes = match quality {
        ProcessingQuality::Preview => 8u64,
        ProcessingQuality::High => 16u64,
    };
    // Six full-resolution RGBA working surfaces plus raw/color/black, the
    // reconstructed CFA plane, output RGBA8, and a conservative 20% allowance
    // for row alignment, reduced guides, driver metadata, and staging buffers.
    let per_pixel = work_bytes.checked_mul(6)?.checked_add(2 + 1 + 4 + 4 + 4)?;
    pixels
        .checked_mul(per_pixel)?
        .checked_mul(6)?
        .checked_div(5)
}

pub(super) fn validate_gpu_working_set(
    width: u32,
    height: u32,
    quality: ProcessingQuality,
) -> Result<()> {
    let estimated = estimated_gpu_working_set_bytes(width, height, quality)
        .ok_or_else(|| anyhow!("GPU working-set estimate overflows"))?;
    let limit = if cfg!(target_os = "android") {
        ANDROID_GPU_WORKING_SET_LIMIT_BYTES
    } else {
        DESKTOP_GPU_WORKING_SET_LIMIT_BYTES
    };
    if estimated > limit {
        return Err(anyhow!(
            "{}x{} {:?} processing requires an estimated {:.1} MiB GPU working set, above the {:.1} MiB safety budget; use a preview proxy or tiled export",
            width,
            height,
            quality,
            estimated as f64 / (1024.0 * 1024.0),
            limit as f64 / (1024.0 * 1024.0),
        ));
    }
    Ok(())
}

pub(super) fn processing_work_format(quality: ProcessingQuality) -> wgpu::TextureFormat {
    match quality {
        ProcessingQuality::Preview => wgpu::TextureFormat::Rgba16Float,
        ProcessingQuality::High => wgpu::TextureFormat::Rgba32Float,
    }
}

pub(super) fn work_shader_source(source: &str, format: wgpu::TextureFormat) -> Cow<'_, str> {
    let replacement = match format {
        wgpu::TextureFormat::Rgba16Float => return Cow::Borrowed(source),
        wgpu::TextureFormat::Rgba32Float => "rgba32float",
        _ => unreachable!("unsupported AuRaw work texture format: {format:?}"),
    };
    debug_assert!(
        source.contains(WORK_FORMAT_MARKER),
        "format-specialized shader is missing the AuRaw work-format marker"
    );
    Cow::Owned(source.replace(WORK_FORMAT_MARKER, replacement))
}

pub(super) fn create_demosaic_texture(
    device: &wgpu::Device,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING
            | wgpu::TextureUsages::STORAGE_BINDING
            | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[format],
    })
}

pub(super) fn create_tone_guide_texture(
    device: &wgpu::Device,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[format],
    })
}

pub(super) fn create_float_work_texture(
    device: &wgpu::Device,
    size: wgpu::Extent3d,
    format: wgpu::TextureFormat,
    label: &'static str,
) -> wgpu::Texture {
    device.create_texture(&wgpu::TextureDescriptor {
        label: Some(label),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
        view_formats: &[format],
    })
}

pub(super) fn buffer_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

pub(super) fn storage_buffer_entry(binding: u32, read_only: bool) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only },
            has_dynamic_offset: false,
            min_binding_size: None,
        },
        count: None,
    }
}

pub(super) fn storage_texture_entry(
    binding: u32,
    format: wgpu::TextureFormat,
    access: wgpu::StorageTextureAccess,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::StorageTexture {
            access,
            format,
            view_dimension: wgpu::TextureViewDimension::D2,
        },
        count: None,
    }
}

pub(super) fn validate_raw(raw: &LoadedRaw) -> Result<()> {
    let pixels = crate::pipeline::raw_loader::validate_raw_dimensions(raw.width, raw.height)?;
    if raw.raw_pixels.len() != pixels {
        return Err(anyhow!(
            "raw pixel count mismatch: got {}, expected {}",
            raw.raw_pixels.len(),
            pixels
        ));
    }
    if raw.color_indices.len() != pixels {
        return Err(anyhow!(
            "CFA index count mismatch: got {}, expected {}",
            raw.color_indices.len(),
            pixels
        ));
    }
    if raw.black_levels_per_pixel.len() != pixels {
        return Err(anyhow!(
            "black-level map count mismatch: got {}, expected {}",
            raw.black_levels_per_pixel.len(),
            pixels
        ));
    }
    if raw
        .color_indices
        .storage_slice()
        .iter()
        .any(|channel| *channel > 3)
    {
        return Err(anyhow!("CFA index map contains a channel above 3"));
    }
    if raw
        .wb_coeffs
        .iter()
        .any(|value| !value.is_finite() || *value <= 0.0)
    {
        return Err(anyhow!(
            "white-balance coefficients must be finite and positive"
        ));
    }
    if raw
        .cam_to_srgb
        .iter()
        .flatten()
        .any(|value| !value.is_finite())
    {
        return Err(anyhow!(
            "camera-to-working matrix contains a non-finite value"
        ));
    }
    if raw
        .cam_to_srgb
        .iter()
        .flatten()
        .all(|value| value.abs() <= 1e-12)
    {
        return Err(anyhow!("camera-to-working matrix is empty"));
    }
    if raw.black_levels.iter().any(|value| !value.is_finite())
        || raw.white_levels.iter().any(|value| !value.is_finite())
    {
        return Err(anyhow!(
            "black/white calibration contains a non-finite value"
        ));
    }

    // Compact calibration maps repeat exactly. Validate one joint period rather
    // than walking tens of millions of logical pixels on every pipeline build.
    // Dense/non-periodic fallbacks still validate the full image.
    let period_width = joint_period(
        raw.color_indices.storage_width(),
        raw.black_levels_per_pixel.storage_width(),
        raw.width,
    );
    let period_height = joint_period(
        raw.color_indices.storage_height(),
        raw.black_levels_per_pixel.storage_height(),
        raw.height,
    );
    for y in 0..period_height {
        for x in 0..period_width {
            let index = (y * raw.width + x) as usize;
            let black = raw.black_levels_per_pixel[index];
            let channel = raw.color_indices[index];
            let white = raw.white_levels[channel as usize];
            if !black.is_finite() || white <= black {
                return Err(anyhow!(
                    "invalid black/white range at pixel {index}: black={black}, white={white}"
                ));
            }
        }
    }
    Ok(())
}


fn joint_period(left: u32, right: u32, logical: u32) -> u32 {
    fn gcd(mut a: u64, mut b: u64) -> u64 {
        while b != 0 {
            let remainder = a % b;
            a = b;
            b = remainder;
        }
        a.max(1)
    }

    let left = u64::from(left.max(1));
    let right = u64::from(right.max(1));
    let lcm = left
        .checked_div(gcd(left, right))
        .and_then(|value| value.checked_mul(right))
        .unwrap_or(u64::from(logical));
    lcm.min(u64::from(logical)).max(1) as u32
}

pub(super) fn texture_array_entry(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2Array,
            multisampled: false,
        },
        count: None,
    }
}

pub(super) fn sampler_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
        count: None,
    }
}

pub(super) fn texture_entry(
    binding: u32,
    sample_type: wgpu::TextureSampleType,
) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::COMPUTE,
        ty: wgpu::BindingType::Texture {
            sample_type,
            view_dimension: wgpu::TextureViewDimension::D2,
            multisampled: false,
        },
        count: None,
    }
}

pub(super) fn create_raw_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("auraw raw mosaic"),
        size: texture_size(raw.width, raw.height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R16Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::R16Uint],
    });
    queue.write_texture(
        copy_texture(&texture),
        bytemuck::cast_slice(&raw.raw_pixels),
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(raw.width * 2),
            rows_per_image: Some(raw.height),
        },
        texture_size(raw.width, raw.height),
    );
    texture
}

pub(super) fn create_black_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("auraw per-pixel black levels"),
        size: texture_size(raw.width, raw.height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R32Float,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::R32Float],
    });
    upload_black_texture(queue, &texture, raw);
    texture
}

pub(super) fn upload_black_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    raw: &LoadedRaw,
) {
    if raw.black_levels_per_pixel.storage_width() == raw.width
        && raw.black_levels_per_pixel.storage_height() == raw.height
    {
        queue.write_texture(
            copy_texture(texture),
            bytemuck::cast_slice(raw.black_levels_per_pixel.storage_slice()),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raw.width * 4),
                rows_per_image: Some(raw.height),
            },
            texture_size(raw.width, raw.height),
        );
        return;
    }

    const MAX_EXPANSION_BYTES: usize = 8 * 1024 * 1024;
    let width = raw.width as usize;
    let row_bytes = width.saturating_mul(std::mem::size_of::<f32>()).max(1);
    let rows_per_chunk = (MAX_EXPANSION_BYTES / row_bytes).max(1).min(raw.height as usize);
    BLACK_UPLOAD_SCRATCH.with(|scratch| {
        let mut values = scratch.borrow_mut();
        values.clear();
        let required_capacity = width.saturating_mul(rows_per_chunk);
        if values.capacity() < required_capacity {
            let additional = required_capacity - values.capacity();
            values.reserve(additional);
        }
        let mut row_start = 0u32;
        while row_start < raw.height {
            let rows = (rows_per_chunk as u32).min(raw.height - row_start);
            values.clear();
            for y in row_start..row_start + rows {
                raw.black_levels_per_pixel.append_row_to(y, &mut *values);
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: row_start, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                bytemuck::cast_slice(values.as_slice()),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(raw.width * 4),
                    rows_per_image: Some(rows),
                },
                texture_size(raw.width, rows),
            );
            row_start += rows;
        }
    });
}

pub(super) fn create_color_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: &LoadedRaw,
) -> wgpu::Texture {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("auraw CFA color indices"),
        size: texture_size(raw.width, raw.height),
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::R8Uint,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[wgpu::TextureFormat::R8Uint],
    });
    upload_color_texture(queue, &texture, raw);
    texture
}

pub(super) fn upload_color_texture(
    queue: &wgpu::Queue,
    texture: &wgpu::Texture,
    raw: &LoadedRaw,
) {
    if raw.color_indices.storage_width() == raw.width
        && raw.color_indices.storage_height() == raw.height
    {
        queue.write_texture(
            copy_texture(texture),
            raw.color_indices.storage_slice(),
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(raw.width),
                rows_per_image: Some(raw.height),
            },
            texture_size(raw.width, raw.height),
        );
        return;
    }

    const MAX_EXPANSION_BYTES: usize = 8 * 1024 * 1024;
    let width = raw.width as usize;
    let rows_per_chunk = (MAX_EXPANSION_BYTES / width.max(1)).max(1).min(raw.height as usize);
    COLOR_UPLOAD_SCRATCH.with(|scratch| {
        let mut values = scratch.borrow_mut();
        values.clear();
        let required_capacity = width.saturating_mul(rows_per_chunk);
        if values.capacity() < required_capacity {
            let additional = required_capacity - values.capacity();
            values.reserve(additional);
        }
        let mut row_start = 0u32;
        while row_start < raw.height {
            let rows = (rows_per_chunk as u32).min(raw.height - row_start);
            values.clear();
            for y in row_start..row_start + rows {
                raw.color_indices.append_row_to(y, &mut *values);
            }
            queue.write_texture(
                wgpu::TexelCopyTextureInfo {
                    texture,
                    mip_level: 0,
                    origin: wgpu::Origin3d { x: 0, y: row_start, z: 0 },
                    aspect: wgpu::TextureAspect::All,
                },
                values.as_slice(),
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(raw.width),
                    rows_per_image: Some(rows),
                },
                texture_size(raw.width, rows),
            );
            row_start += rows;
        }
    });
}

pub(super) fn copy_texture(texture: &wgpu::Texture) -> wgpu::TexelCopyTextureInfo<'_> {
    wgpu::TexelCopyTextureInfo {
        texture,
        mip_level: 0,
        origin: wgpu::Origin3d::ZERO,
        aspect: wgpu::TextureAspect::All,
    }
}

pub(super) fn texture_size(width: u32, height: u32) -> wgpu::Extent3d {
    wgpu::Extent3d {
        width,
        height,
        depth_or_array_layers: 1,
    }
}
