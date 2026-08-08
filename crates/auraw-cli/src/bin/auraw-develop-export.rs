use anyhow::{anyhow, bail, Context, Result};
use auraw_cli::pipeline::{
    crop_raw, export_mask_atlas_edge, load_raw_file, load_raw_file_with_dcp,
    spawn_tiled_png_export, DenoiseQuality, ExportEvent, ExportMetadata, ExportSettings,
    ExposureParams, GeometryTransform, MaskStack, TileSpec, GLOBAL_TINT_OFFSET_LIMIT,
    HUE_ROTATION_LIMIT_DEGREES,
};
use auraw_gpu::wgpu;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::{atomic::AtomicBool, Arc};

#[derive(Debug)]
struct Args {
    input: PathBuf,
    output: Option<PathBuf>,
    suite_output: Option<PathBuf>,
    suite_only: Vec<String>,
    skip_existing: bool,
    dcp: Option<PathBuf>,
    crop: Option<[u32; 4]>,
    report_detail_defaults: bool,
    adjustments: Vec<(String, f32)>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("auraw-develop-export: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let mut raw = match &args.dcp {
        Some(profile) => load_raw_file_with_dcp(&args.input, profile),
        None => load_raw_file(&args.input),
    }
    .with_context(|| format!("load RAW {}", args.input.display()))?;
    let adaptive_exposure = default_exposure_for_raw(&raw);
    if args.report_detail_defaults {
        eprintln!(
            "ISO {:.0}; estimated shot={:?}, read={:?}, confidence={:.3}; adaptive Detail: luminance={:.1}, color={:.1}, detail={:.1}, quality={}, sharpen={:.0}/{:.2}/{:.0}/{:.0}",
            raw.iso_speed(),
            raw.noise_profile.shot,
            raw.noise_profile.read,
            raw.noise_profile.confidence,
            adaptive_exposure.luminance_denoise,
            adaptive_exposure.chroma_denoise * 100.0,
            adaptive_exposure.denoise_detail,
            adaptive_exposure.denoise_quality.label(),
            adaptive_exposure.sharpen_amount,
            adaptive_exposure.sharpen_radius,
            adaptive_exposure.sharpen_detail,
            adaptive_exposure.sharpen_masking,
        );
    }
    if let Some([x, y, width, height]) = args.crop {
        raw = crop_raw(&raw, x, y, width, height);
    }

    let instance = wgpu::Instance::default();
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        compatible_surface: None,
        force_fallback_adapter: false,
    }))
    .or_else(|_| {
        pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::LowPower,
            compatible_surface: None,
            force_fallback_adapter: true,
        }))
    })
    .context("request a hardware or software wgpu adapter")?;
    let adapter_info = adapter.get_info();
    let adapter_limits = adapter.limits();
    let required_dimension = export_mask_atlas_edge(raw.width, raw.height);
    if required_dimension > adapter_limits.max_texture_dimension_2d {
        bail!(
            "export requires a {required_dimension}-pixel mask atlas, but adapter {:?} supports {}",
            adapter_info.name,
            adapter_limits.max_texture_dimension_2d
        );
    }
    let mut required_limits = if adapter_info.backend == wgpu::Backend::Gl {
        wgpu::Limits::downlevel_webgl2_defaults()
    } else {
        wgpu::Limits::default()
    };
    required_limits.max_texture_dimension_2d = required_dimension;
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("auraw headless Develop export"),
        required_limits,
        ..Default::default()
    }))
    .context("request a wgpu device")?;

    let raw = Arc::new(raw);
    let metadata = ExportMetadata::from_raw(
        &raw,
        args.input
            .file_name()
            .and_then(|name| name.to_str())
            .map(str::to_owned),
    );
    let settings = ExportSettings {
        keep_metadata: false,
        ..ExportSettings::default()
    };

    if let Some(directory) = &args.suite_output {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create suite directory {}", directory.display()))?;
        for (label, adjustment) in LIGHTROOM_COMPARISON_SUITE {
            if !args.suite_only.is_empty() && !args.suite_only.iter().any(|value| value == label) {
                continue;
            }
            let output = directory.join(format!("{label}.png"));
            if args.skip_existing && output.is_file() {
                println!("skipped existing {}", output.display());
                continue;
            }
            let mut exposure = default_exposure_for_raw(&raw);
            if let Some((name, value)) = adjustment {
                set_adjustment(&mut exposure, name, *value)?;
            }
            export_one(
                &device,
                &queue,
                Arc::clone(&raw),
                exposure,
                &output,
                &settings,
                &metadata,
                adapter_info.backend,
            )?;
        }
        return Ok(());
    }

    let mut exposure = default_exposure_for_raw(&raw);
    for (name, value) in &args.adjustments {
        set_adjustment(&mut exposure, name, *value)?;
    }
    export_one(
        &device,
        &queue,
        raw,
        exposure,
        args.output.as_deref().context("missing output path")?,
        &settings,
        &metadata,
        adapter_info.backend,
    )
}

fn default_exposure_for_raw(raw: &auraw_cli::pipeline::LoadedRaw) -> ExposureParams {
    let mut exposure = ExposureParams::default();
    raw.apply_adaptive_detail_defaults(&mut exposure);
    exposure
}

#[allow(clippy::too_many_arguments)]
fn export_one(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    raw: Arc<auraw_cli::pipeline::LoadedRaw>,
    exposure: ExposureParams,
    output: &Path,
    settings: &ExportSettings,
    metadata: &ExportMetadata,
    backend: wgpu::Backend,
) -> Result<()> {
    let source_dimensions = (metadata.source_width, metadata.source_height);
    let receiver = spawn_tiled_png_export(
        device.clone(),
        queue.clone(),
        raw,
        GeometryTransform::default(),
        exposure,
        MaskStack::default(),
        None,
        output.to_owned(),
        TileSpec::default(),
        settings.clone(),
        metadata.clone(),
        Arc::new(AtomicBool::new(false)),
    );

    let mut finished = None;
    while let Ok(event) = receiver.recv() {
        match event {
            ExportEvent::Progress {
                completed_tiles,
                total_tiles,
            } => eprintln!("rendered {completed_tiles}/{total_tiles} tiles"),
            ExportEvent::Finished(result) => {
                finished = Some(result.map_err(|error| anyhow!(error))?);
                break;
            }
        }
    }
    let output = finished.context("export worker exited without a completion event")?;
    // The completion event is sent at the end of the worker, but wait for its
    // sender to disconnect before releasing the final device handle. This
    // avoids racing Vulkan teardown in short-lived headless invocations.
    while receiver.recv().is_ok() {}
    println!(
        "wrote {} ({}x{}, sRGB PNG, {:?})",
        output.display(),
        source_dimensions.0,
        source_dimensions.1,
        backend,
    );
    Ok(())
}

const LIGHTROOM_COMPARISON_SUITE: &[(&str, Option<(&str, f32)>)] = &[
    ("baseline", None),
    ("exposure_plus1_25", Some(("exposure", 1.25))),
    ("exposure_minus1_25", Some(("exposure", -1.25))),
    ("exposure_plus5", Some(("exposure", 5.0))),
    ("exposure_minus5", Some(("exposure", -5.0))),
    ("contrast_plus100", Some(("contrast", 100.0))),
    ("contrast_minus100", Some(("contrast", -100.0))),
    ("highlights_plus100", Some(("highlights", 100.0))),
    ("highlights_minus100", Some(("highlights", -100.0))),
    ("shadows_plus100", Some(("shadows", 100.0))),
    ("shadows_minus100", Some(("shadows", -100.0))),
    ("whites_plus100", Some(("whites", 100.0))),
    ("whites_minus100", Some(("whites", -100.0))),
    ("blacks_plus100", Some(("blacks", 100.0))),
    ("blacks_minus100", Some(("blacks", -100.0))),
    ("texture_plus100", Some(("texture", 100.0))),
    ("texture_minus100", Some(("texture", -100.0))),
    ("clarity_plus100", Some(("clarity", 100.0))),
    ("clarity_minus100", Some(("clarity", -100.0))),
    ("dehaze_plus100", Some(("dehaze", 100.0))),
    ("dehaze_minus100", Some(("dehaze", -100.0))),
    ("vibrance_plus100", Some(("vibrance", 100.0))),
    ("vibrance_minus100", Some(("vibrance", -100.0))),
    ("saturation_plus100", Some(("saturation", 100.0))),
    ("saturation_minus100", Some(("saturation", -100.0))),
];

fn set_adjustment(exposure: &mut ExposureParams, name: &str, value: f32) -> Result<()> {
    if !value.is_finite() {
        bail!("adjustment {name:?} must be finite");
    }
    match name {
        "exposure" => exposure.exposure = value.clamp(-5.0, 5.0),
        "contrast" => exposure.contrast = value.clamp(-100.0, 100.0),
        "highlights" => exposure.highlights = value.clamp(-100.0, 100.0),
        "shadows" => exposure.shadows = value.clamp(-100.0, 100.0),
        "whites" => exposure.whites = value.clamp(-100.0, 100.0),
        "blacks" => exposure.blacks = value.clamp(-100.0, 100.0),
        "texture" => exposure.texture = value.clamp(-100.0, 100.0),
        "clarity" => exposure.clarity = value.clamp(-100.0, 100.0),
        "dehaze" => exposure.dehaze = value.clamp(-100.0, 100.0),
        "vignette" | "vignette_amount" => exposure.vignette_amount = value.clamp(-100.0, 100.0),
        "vibrance" => exposure.vibrance = value.clamp(-100.0, 100.0),
        "saturation" => exposure.saturation = value.clamp(-100.0, 100.0),
        "hue" => {
            exposure.hue = value.clamp(-HUE_ROTATION_LIMIT_DEGREES, HUE_ROTATION_LIMIT_DEGREES)
        }
        "temperature" => exposure.temperature = value.clamp(-500.0, 500.0),
        "tint" => exposure.tint = value.clamp(-GLOBAL_TINT_OFFSET_LIMIT, GLOBAL_TINT_OFFSET_LIMIT),
        "luminance_denoise" => exposure.luminance_denoise = value.clamp(0.0, 100.0),
        "color_denoise" => exposure.chroma_denoise = value.clamp(0.0, 100.0) / 100.0,
        "denoise_detail" => exposure.denoise_detail = value.clamp(0.0, 100.0),
        "denoise_quality" => {
            exposure.denoise_quality = if value < 0.5 {
                DenoiseQuality::Fast
            } else if value < 1.5 {
                DenoiseQuality::Balanced
            } else {
                DenoiseQuality::High
            }
        }
        "sharpen_amount" => exposure.sharpen_amount = value.clamp(0.0, 150.0),
        "sharpen_radius" => exposure.sharpen_radius = value.clamp(0.5, 3.0),
        "sharpen_detail" => exposure.sharpen_detail = value.clamp(0.0, 100.0),
        "sharpen_masking" => exposure.sharpen_masking = value.clamp(0.0, 100.0),
        other => bail!("unsupported adjustment {other:?}"),
    }
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut input = None;
    let mut output = None;
    let mut suite_output = None;
    let mut suite_only = Vec::new();
    let mut skip_existing = false;
    let mut dcp = None;
    let mut crop = None;
    let mut report_detail_defaults = false;
    let mut adjustments = Vec::new();
    let mut values = env::args().skip(1);
    while let Some(argument) = values.next() {
        match argument.as_str() {
            "--input" => input = Some(PathBuf::from(next_value(&mut values, "--input")?)),
            "--output" => output = Some(PathBuf::from(next_value(&mut values, "--output")?)),
            "--suite-output" => {
                suite_output = Some(PathBuf::from(next_value(&mut values, "--suite-output")?))
            }
            "--only" => suite_only.push(next_value(&mut values, "--only")?),
            "--skip-existing" => skip_existing = true,
            "--dcp" => dcp = Some(PathBuf::from(next_value(&mut values, "--dcp")?)),
            "--crop" => crop = Some(parse_crop(&next_value(&mut values, "--crop")?)?),
            "--report-detail-defaults" => report_detail_defaults = true,
            "--adjust" => {
                let assignment = next_value(&mut values, "--adjust")?;
                let (name, value) = assignment
                    .split_once('=')
                    .ok_or_else(|| anyhow!("--adjust expects NAME=VALUE, got {assignment:?}"))?;
                adjustments.push((
                    name.to_owned(),
                    value
                        .parse::<f32>()
                        .with_context(|| format!("parse adjustment value in {assignment:?}"))?,
                ));
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}; use --help"),
        }
    }
    if output.is_some() == suite_output.is_some() {
        bail!("provide exactly one of --output or --suite-output");
    }
    if suite_output.is_some() && !adjustments.is_empty() {
        bail!("--adjust cannot be combined with --suite-output");
    }
    if suite_output.is_none() && !suite_only.is_empty() {
        bail!("--only requires --suite-output");
    }
    for label in &suite_only {
        if !LIGHTROOM_COMPARISON_SUITE
            .iter()
            .any(|(candidate, _)| candidate == label)
        {
            bail!("unknown suite endpoint {label:?}");
        }
    }
    if let Some(output) = &output {
        if output.extension().and_then(|value| value.to_str()) != Some("png") {
            bail!("--output must use the .png extension");
        }
    }
    Ok(Args {
        input: input.ok_or_else(|| anyhow!("--input is required"))?,
        output,
        suite_output,
        suite_only,
        skip_existing,
        dcp,
        crop,
        report_detail_defaults,
        adjustments,
    })
}

fn parse_crop(value: &str) -> Result<[u32; 4]> {
    let values = value
        .split(',')
        .map(|part| {
            part.parse::<u32>()
                .with_context(|| format!("parse crop component in {value:?}"))
        })
        .collect::<Result<Vec<_>>>()?;
    let crop: [u32; 4] = values
        .try_into()
        .map_err(|_| anyhow!("--crop expects X,Y,WIDTH,HEIGHT"))?;
    if crop[2] == 0 || crop[3] == 0 {
        bail!("--crop width and height must be non-zero");
    }
    Ok(crop)
}

fn next_value(values: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    values
        .next()
        .ok_or_else(|| anyhow!("{option} requires a value"))
}

fn print_help() {
    println!(concat!(
        "Headless AuRaw Develop export\n\n",
        "Usage:\n",
        "  auraw-develop-export --input FILE --output FILE.png [--dcp PROFILE.dcp]\n",
        "    [--crop X,Y,WIDTH,HEIGHT] [--report-detail-defaults]\n",
        "    [--adjust NAME=VALUE]...\n\n",
        "  auraw-develop-export --input FILE --suite-output DIRECTORY [--dcp PROFILE.dcp]\n",
        "    [--only ENDPOINT]... [--skip-existing]\n\n",
        "Supported adjustment names: exposure, contrast, highlights, shadows, whites,\n",
        "blacks, texture, clarity, dehaze, vignette, hue, vibrance, saturation, temperature, tint,\n",
        "luminance_denoise, color_denoise, denoise_detail, denoise_quality (0/1/2),\n",
        "sharpen_amount, sharpen_radius, sharpen_detail, and sharpen_masking. The suite\n",
        "exports isolated endpoints matching the standard\n",
        "Lightroom comparison set. Lens correction is not applied by this tool."
    ));
}
