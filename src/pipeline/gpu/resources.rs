use super::*;
use std::cell::RefCell;
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_UPLOAD_SCRATCH_BYTES: usize = 8 * 1024 * 1024;

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

const GPU_ALLOCATION_ALIGNMENT_BYTES: u64 = 256;
const GPU_SAFETY_MARGIN_NUMERATOR: u64 = 1;
const GPU_SAFETY_MARGIN_DENOMINATOR: u64 = 5;
static RESERVED_GPU_BYTES: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum GpuResourceResidency {
    Persistent,
    Transient,
    HostPeak,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GpuResourceAccountingEntry {
    pub name: &'static str,
    pub residency: GpuResourceResidency,
    pub bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct GpuResourcePlan {
    pub entries: Vec<GpuResourceAccountingEntry>,
    pub persistent_gpu_bytes: u64,
    pub transient_gpu_peak_bytes: u64,
    pub host_peak_bytes: u64,
    pub safety_margin_bytes: u64,
    pub admitted_gpu_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct GpuResourcePlanInput {
    pub width: u32,
    pub height: u32,
    pub quality: ProcessingQuality,
    pub tone_scale: u32,
    pub mask_atlas_edge: u32,
    pub mask_layers: u32,
    pub profile_buffer_bytes: u64,
    pub params_buffer_bytes: u64,
}

#[derive(Debug)]
pub(super) struct GpuBudgetReservation {
    bytes: u64,
}

impl GpuBudgetReservation {
    pub(super) fn acquire(plan: &GpuResourcePlan, limit: u64) -> Result<Self> {
        validate_gpu_resource_plan(plan, limit)?;
        reserve_gpu_bytes(&RESERVED_GPU_BYTES, limit, plan.admitted_gpu_bytes).map_err(|used| {
            anyhow!(
                "GPU pipelines already reserve {:.1} MiB; this pipeline needs another {:.1} MiB including its safety margin, exceeding the {:.1} MiB process budget; close optional detail/navigation previews, reduce proxy size or mask capacity, or use tiled export",
                used as f64 / (1024.0 * 1024.0),
                plan.admitted_gpu_bytes as f64 / (1024.0 * 1024.0),
                limit as f64 / (1024.0 * 1024.0),
            )
        })?;
        Ok(Self {
            bytes: plan.admitted_gpu_bytes,
        })
    }
}

impl Drop for GpuBudgetReservation {
    fn drop(&mut self) {
        RESERVED_GPU_BYTES.fetch_sub(self.bytes, Ordering::AcqRel);
    }
}

fn reserve_gpu_bytes(used: &AtomicU64, limit: u64, bytes: u64) -> std::result::Result<(), u64> {
    let mut current = used.load(Ordering::Acquire);
    loop {
        let Some(next) = current.checked_add(bytes) else {
            return Err(current);
        };
        if next > limit {
            return Err(current);
        }
        match used.compare_exchange_weak(current, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return Ok(()),
            Err(observed) => current = observed,
        }
    }
}

fn checked_align_up(value: u64, alignment: u64, context: &'static str) -> Result<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return Err(anyhow!("invalid {context} alignment {alignment}"));
    }
    let mask = alignment - 1;
    value
        .checked_add(mask)
        .map(|expanded| expanded & !mask)
        .ok_or_else(|| anyhow!("{context} alignment overflows"))
}

fn texture_format_bytes_per_texel(format: wgpu::TextureFormat) -> Result<u64> {
    match format {
        wgpu::TextureFormat::R8Uint => Ok(1),
        wgpu::TextureFormat::R16Uint | wgpu::TextureFormat::R16Float => Ok(2),
        wgpu::TextureFormat::R32Float | wgpu::TextureFormat::Rgba8Unorm => Ok(4),
        wgpu::TextureFormat::Rgba16Float => Ok(8),
        wgpu::TextureFormat::Rgba32Float => Ok(16),
        _ => Err(anyhow!("unaccounted GPU texture format {format:?}")),
    }
}

fn texture_allocation_bytes(
    width: u32,
    height: u32,
    depth_or_layers: u32,
    mip_levels: u32,
    format: wgpu::TextureFormat,
) -> Result<u64> {
    if width == 0 || height == 0 || depth_or_layers == 0 || mip_levels == 0 {
        return Err(anyhow!("GPU texture dimensions, layers, and mip levels must be non-zero"));
    }
    let bytes_per_texel = texture_format_bytes_per_texel(format)?;
    let mut total = 0u64;
    let mut mip_width = width;
    let mut mip_height = height;
    for _ in 0..mip_levels {
        let mip_bytes = u64::from(mip_width)
            .checked_mul(u64::from(mip_height))
            .and_then(|value| value.checked_mul(u64::from(depth_or_layers)))
            .and_then(|value| value.checked_mul(bytes_per_texel))
            .ok_or_else(|| anyhow!("GPU texture byte calculation overflows"))?;
        total = total
            .checked_add(checked_align_up(
                mip_bytes,
                GPU_ALLOCATION_ALIGNMENT_BYTES,
                "GPU texture",
            )?)
            .ok_or_else(|| anyhow!("GPU texture mip total overflows"))?;
        mip_width = (mip_width / 2).max(1);
        mip_height = (mip_height / 2).max(1);
    }
    Ok(total)
}

fn aligned_buffer_bytes(bytes: u64) -> Result<u64> {
    checked_align_up(bytes.max(1), GPU_ALLOCATION_ALIGNMENT_BYTES, "GPU buffer")
}

fn aligned_copy_buffer_bytes(width: u32, height: u32, bytes_per_texel: u64) -> Result<u64> {
    let row_bytes = u64::from(width)
        .checked_mul(bytes_per_texel)
        .ok_or_else(|| anyhow!("GPU copy row byte calculation overflows"))?;
    let padded_row = checked_align_up(row_bytes, 256, "GPU copy row")?;
    padded_row
        .checked_mul(u64::from(height))
        .ok_or_else(|| anyhow!("GPU copy buffer byte calculation overflows"))
}

fn push_entry(
    entries: &mut Vec<GpuResourceAccountingEntry>,
    name: &'static str,
    residency: GpuResourceResidency,
    bytes: u64,
) {
    entries.push(GpuResourceAccountingEntry {
        name,
        residency,
        bytes,
    });
}

pub(super) fn build_gpu_resource_plan(input: GpuResourcePlanInput) -> Result<GpuResourcePlan> {
    if input.tone_scale == 0 {
        return Err(anyhow!("GPU tone-analysis scale must be non-zero"));
    }
    let mut entries = Vec::new();
    let work_format = processing_work_format(input.quality);
    let full = |format| texture_allocation_bytes(input.width, input.height, 1, 1, format);

    push_entry(
        &mut entries,
        "raw CFA texture",
        GpuResourceResidency::Persistent,
        full(wgpu::TextureFormat::R16Uint)?,
    );
    push_entry(
        &mut entries,
        "CFA color-index texture",
        GpuResourceResidency::Persistent,
        full(wgpu::TextureFormat::R8Uint)?,
    );
    push_entry(
        &mut entries,
        "black-level texture",
        GpuResourceResidency::Persistent,
        full(wgpu::TextureFormat::R32Float)?,
    );
    push_entry(
        &mut entries,
        "reconstructed raw texture",
        GpuResourceResidency::Persistent,
        full(wgpu::TextureFormat::R32Float)?,
    );
    for name in [
        "highlight work A",
        "highlight work B",
        "scene texture",
        "display-linear texture",
        "demosaic scratch 1",
        "demosaic scratch 2",
    ] {
        push_entry(&mut entries, name, GpuResourceResidency::Persistent, full(work_format)?);
    }
    push_entry(
        &mut entries,
        "encoded output texture",
        GpuResourceResidency::Persistent,
        full(wgpu::TextureFormat::Rgba8Unorm)?,
    );

    let tone_width = input.width.div_ceil(input.tone_scale);
    let tone_height = input.height.div_ceil(input.tone_scale);
    let tone_bytes = texture_allocation_bytes(
        tone_width,
        tone_height,
        1,
        1,
        tone_guide_format(),
    )?;
    push_entry(&mut entries, "tone guide A", GpuResourceResidency::Persistent, tone_bytes);
    push_entry(&mut entries, "tone guide B", GpuResourceResidency::Persistent, tone_bytes);

    let mask_bytes = texture_allocation_bytes(
        input.mask_atlas_edge,
        input.mask_atlas_edge,
        input.mask_layers,
        1,
        wgpu::TextureFormat::R16Float,
    )?;
    push_entry(
        &mut entries,
        "local-mask atlas",
        GpuResourceResidency::Persistent,
        mask_bytes,
    );
    push_entry(
        &mut entries,
        "inpaint texture",
        GpuResourceResidency::Persistent,
        full(wgpu::TextureFormat::Rgba16Float)?,
    );
    push_entry(
        &mut entries,
        "camera/output profile buffer",
        GpuResourceResidency::Persistent,
        aligned_buffer_bytes(input.profile_buffer_bytes)?,
    );
    push_entry(
        &mut entries,
        "pipeline parameter buffer",
        GpuResourceResidency::Persistent,
        aligned_buffer_bytes(input.params_buffer_bytes)?,
    );
    let histogram_bytes = u64::try_from(std::mem::size_of::<u32>())
        .ok()
        .and_then(|word_bytes| word_bytes.checked_mul(256))
        .ok_or_else(|| anyhow!("tone histogram buffer byte calculation overflows"))?;
    push_entry(
        &mut entries,
        "tone histogram buffer",
        GpuResourceResidency::Persistent,
        aligned_buffer_bytes(histogram_bytes)?,
    );
    push_entry(
        &mut entries,
        "tone statistics buffer",
        GpuResourceResidency::Persistent,
        aligned_buffer_bytes(TONE_STATS_SIZE_BYTES)?,
    );

    // On-demand scene conversion/inpainting creates one full-resolution RGBA32F
    // work texture. Its readback may coexist until mapping completes, so both are
    // included in the transient peak. RGBA32F readback is chunked to 64 MiB.
    let full_scene_conversion = full(wgpu::TextureFormat::Rgba32Float)?;
    let model_scene_conversion = texture_allocation_bytes(
        crate::inpainting::LAMA_EDGE,
        crate::inpainting::LAMA_EDGE,
        1,
        1,
        wgpu::TextureFormat::Rgba32Float,
    )?;
    let transient_work = full_scene_conversion.max(model_scene_conversion);
    push_entry(
        &mut entries,
        "scene/inpaint conversion texture",
        GpuResourceResidency::Transient,
        transient_work,
    );
    push_entry(
        &mut entries,
        "on-demand conversion parameters",
        GpuResourceResidency::Transient,
        aligned_buffer_bytes(
            u64::try_from(std::mem::size_of::<InpaintResizeParams>())
                .map_err(|_| anyhow!("inpaint resize parameter size does not fit in u64"))?,
        )?,
    );
    let rgba32_full_copy = aligned_copy_buffer_bytes(input.width, input.height, 16)?;
    let rgba32_model_copy = aligned_copy_buffer_bytes(
        crate::inpainting::LAMA_EDGE,
        crate::inpainting::LAMA_EDGE,
        16,
    )?;
    let rgba32_readback = rgba32_full_copy
        .max(rgba32_model_copy)
        .min(MAX_RGBA32_READBACK_CHUNK_BYTES);
    let rgba8_readback = aligned_copy_buffer_bytes(input.width, input.height, 4)?;
    let readback_peak = rgba32_readback.max(rgba8_readback);
    push_entry(
        &mut entries,
        "readback buffer peak",
        GpuResourceResidency::Transient,
        readback_peak,
    );

    // The constructor currently uploads one zeroed RGBA16F inpaint image. Raw
    // compact-map expansion also retains two bounded 8 MiB thread-local scratch
    // vectors. Host peak is reported separately from the GPU admission total.
    let inpaint_upload = u64::from(input.width)
        .checked_mul(u64::from(input.height))
        .and_then(|value| value.checked_mul(8))
        .ok_or_else(|| anyhow!("inpaint upload byte calculation overflows"))?;
    push_entry(
        &mut entries,
        "zero inpaint upload",
        GpuResourceResidency::HostPeak,
        inpaint_upload,
    );
    let upload_scratch_bytes = u64::try_from(MAX_UPLOAD_SCRATCH_BYTES)
        .map_err(|_| anyhow!("upload scratch size does not fit in u64"))?;
    push_entry(
        &mut entries,
        "black upload scratch",
        GpuResourceResidency::HostPeak,
        upload_scratch_bytes,
    );
    push_entry(
        &mut entries,
        "color upload scratch",
        GpuResourceResidency::HostPeak,
        upload_scratch_bytes,
    );

    let sum = |residency| -> Result<u64> {
        entries
            .iter()
            .filter(|entry| entry.residency == residency)
            .try_fold(0u64, |total, entry| {
                total
                    .checked_add(entry.bytes)
                    .ok_or_else(|| anyhow!("GPU resource-plan total overflows"))
            })
    };
    let persistent_gpu_bytes = sum(GpuResourceResidency::Persistent)?;
    let transient_gpu_peak_bytes = sum(GpuResourceResidency::Transient)?;
    let host_peak_bytes = sum(GpuResourceResidency::HostPeak)?;
    let gpu_before_margin = persistent_gpu_bytes
        .checked_add(transient_gpu_peak_bytes)
        .ok_or_else(|| anyhow!("GPU resource-plan peak overflows"))?;
    let safety_margin_bytes = gpu_before_margin
        .checked_mul(GPU_SAFETY_MARGIN_NUMERATOR)
        .and_then(|value| value.checked_div(GPU_SAFETY_MARGIN_DENOMINATOR))
        .ok_or_else(|| anyhow!("GPU safety-margin calculation overflows"))?;
    let admitted_gpu_bytes = gpu_before_margin
        .checked_add(safety_margin_bytes)
        .ok_or_else(|| anyhow!("GPU admitted working set overflows"))?;

    Ok(GpuResourcePlan {
        entries,
        persistent_gpu_bytes,
        transient_gpu_peak_bytes,
        host_peak_bytes,
        safety_margin_bytes,
        admitted_gpu_bytes,
    })
}

pub(super) fn validate_gpu_resource_plan(plan: &GpuResourcePlan, limit: u64) -> Result<()> {
    if plan.admitted_gpu_bytes > limit {
        let mask = plan
            .entries
            .iter()
            .find(|entry| entry.name == "local-mask atlas")
            .map_or(0, |entry| entry.bytes);
        let inpaint = plan
            .entries
            .iter()
            .find(|entry| entry.name == "inpaint texture")
            .map_or(0, |entry| entry.bytes);
        return Err(anyhow!(
            "GPU resource plan requires {:.1} MiB including a {:.1} MiB safety margin (mask atlas {:.1} MiB, inpaint {:.1} MiB), above the {:.1} MiB budget; reduce proxy size or mask-atlas capacity, or use tiled export",
            plan.admitted_gpu_bytes as f64 / (1024.0 * 1024.0),
            plan.safety_margin_bytes as f64 / (1024.0 * 1024.0),
            mask as f64 / (1024.0 * 1024.0),
            inpaint as f64 / (1024.0 * 1024.0),
            limit as f64 / (1024.0 * 1024.0),
        ));
    }
    Ok(())
}

pub(super) fn gpu_working_set_limit_bytes() -> u64 {
    if cfg!(target_os = "android") {
        ANDROID_GPU_WORKING_SET_LIMIT_BYTES
    } else {
        DESKTOP_GPU_WORKING_SET_LIMIT_BYTES
    }
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

    const MAX_EXPANSION_BYTES: usize = MAX_UPLOAD_SCRATCH_BYTES;
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

    const MAX_EXPANSION_BYTES: usize = MAX_UPLOAD_SCRATCH_BYTES;
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

#[cfg(test)]
mod resource_plan_tests {
    use super::*;

    fn input() -> GpuResourcePlanInput {
        GpuResourcePlanInput {
            width: 640,
            height: 480,
            quality: ProcessingQuality::Preview,
            tone_scale: 4,
            mask_atlas_edge: 256,
            mask_layers: 4,
            profile_buffer_bytes: 4096,
            params_buffer_bytes: GPU_PARAMS_ABI_SIZE_BYTES as u64,
        }
    }

    fn entry_bytes(plan: &GpuResourcePlan, name: &str) -> u64 {
        plan.entries
            .iter()
            .find(|entry| entry.name == name)
            .map(|entry| entry.bytes)
            .unwrap_or_else(|| panic!("missing resource accounting entry {name}"))
    }

    #[test]
    fn texture_accounting_is_format_layer_and_mip_aware() {
        assert_eq!(
            texture_allocation_bytes(16, 8, 3, 1, wgpu::TextureFormat::R16Float).unwrap(),
            16 * 8 * 3 * 2
        );
        assert_eq!(
            texture_allocation_bytes(16, 8, 1, 2, wgpu::TextureFormat::Rgba16Float).unwrap(),
            (16 * 8 * 8) + (8 * 4 * 8)
        );
    }

    #[test]
    fn plan_includes_mask_atlas_and_full_resolution_inpaint() {
        let plan = build_gpu_resource_plan(input()).unwrap();
        assert_eq!(entry_bytes(&plan, "local-mask atlas"), 256 * 256 * 4 * 2);
        assert_eq!(
            entry_bytes(&plan, "inpaint texture"),
            640 * 480 * 8
        );
    }


    #[test]
    fn plan_includes_fixed_size_inpaint_model_resources_for_small_pipelines() {
        let mut small = input();
        small.width = 64;
        small.height = 64;
        let plan = build_gpu_resource_plan(small).unwrap();
        assert_eq!(
            entry_bytes(&plan, "scene/inpaint conversion texture"),
            u64::from(crate::inpainting::LAMA_EDGE)
                * u64::from(crate::inpainting::LAMA_EDGE)
                * 16
        );
        assert_eq!(
            entry_bytes(&plan, "readback buffer peak"),
            u64::from(crate::inpainting::LAMA_EDGE) * 16 * u64::from(crate::inpainting::LAMA_EDGE)
        );
    }

    #[test]
    fn resource_plan_rejects_arithmetic_overflow() {
        let mut oversized = input();
        oversized.width = u32::MAX;
        oversized.height = u32::MAX;
        oversized.mask_atlas_edge = u32::MAX;
        oversized.mask_layers = u32::MAX;
        assert!(build_gpu_resource_plan(oversized).is_err());
    }

    #[test]
    fn budget_boundary_is_deterministic() {
        let plan = build_gpu_resource_plan(input()).unwrap();
        assert!(validate_gpu_resource_plan(&plan, plan.admitted_gpu_bytes).is_ok());
        assert!(validate_gpu_resource_plan(&plan, plan.admitted_gpu_bytes - 1).is_err());
    }

    #[test]
    fn aggregate_pipeline_reservations_fail_before_crossing_budget() {
        let used = AtomicU64::new(0);
        assert!(reserve_gpu_bytes(&used, 1_000, 600).is_ok());
        assert_eq!(used.load(Ordering::Acquire), 600);
        assert_eq!(reserve_gpu_bytes(&used, 1_000, 401), Err(600));
        assert_eq!(used.load(Ordering::Acquire), 600);
        assert!(reserve_gpu_bytes(&used, 1_000, 400).is_ok());
        assert_eq!(used.load(Ordering::Acquire), 1_000);
    }

    #[test]
    fn aggregate_pipeline_reservation_overflow_is_rejected() {
        let used = AtomicU64::new(u64::MAX - 2);
        assert_eq!(reserve_gpu_bytes(&used, u64::MAX, 4), Err(u64::MAX - 2));
        assert_eq!(used.load(Ordering::Acquire), u64::MAX - 2);
    }

    #[test]
    fn every_constructor_allocation_class_has_a_named_entry() {
        let plan = build_gpu_resource_plan(input()).unwrap();
        for expected in [
            "raw CFA texture",
            "CFA color-index texture",
            "black-level texture",
            "reconstructed raw texture",
            "highlight work A",
            "highlight work B",
            "scene texture",
            "display-linear texture",
            "demosaic scratch 1",
            "demosaic scratch 2",
            "encoded output texture",
            "tone guide A",
            "tone guide B",
            "local-mask atlas",
            "inpaint texture",
            "camera/output profile buffer",
            "pipeline parameter buffer",
            "tone histogram buffer",
            "tone statistics buffer",
            "scene/inpaint conversion texture",
            "on-demand conversion parameters",
            "readback buffer peak",
        ] {
            assert!(
                plan.entries.iter().any(|entry| entry.name == expected),
                "missing accounting entry {expected}"
            );
        }
        assert!(plan.safety_margin_bytes > 0);
        assert_eq!(
            plan.admitted_gpu_bytes,
            plan.persistent_gpu_bytes
                + plan.transient_gpu_peak_bytes
                + plan.safety_margin_bytes
        );
    }
}
