use anyhow::{anyhow, bail, Context, Result};
use auraw::pipeline::{
    load_raw_file, load_raw_file_with_dcp, mask_atlas_edge, CfaKind, ExposureParams, GpuParams,
    MaskStack, ProcessingQuality, RawGpuPipeline,
};
use auraw::regression::write_linear_rgb_npz;
use eframe::wgpu;
use ring::digest::{Context as Sha256Context, SHA256};
use std::env;
use std::path::PathBuf;

#[derive(Debug)]
struct Args {
    backend: String,
    input: PathBuf,
    output: PathBuf,
    dcp: Option<PathBuf>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("auraw-regression-render: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    if args.backend != "gpu" {
        bail!(
            "unsupported backend {:?}; this binary currently exposes the canonical GPU scene path",
            args.backend
        );
    }
    if args.output.extension().and_then(|value| value.to_str()) != Some("npz") {
        bail!("--output must use the .npz extension");
    }

    let raw = match &args.dcp {
        Some(profile) => load_raw_file_with_dcp(&args.input, profile),
        None => load_raw_file(&args.input),
    }
    .with_context(|| format!("load RAW {}", args.input.display()))?;

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
    // The headless pipeline also allocates the fixed local-mask atlas. Small
    // regression fixtures must not request a device limit below that resource.
    let required_dimension = raw.width.max(raw.height).max(mask_atlas_edge());
    if required_dimension > adapter_limits.max_texture_dimension_2d {
        bail!(
            "pipeline requires texture dimension {required_dimension} for RAW {}x{} and its mask atlas, but the adapter limit is {}",
            raw.width,
            raw.height,
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
        label: Some("auraw regression renderer"),
        required_limits,
        ..Default::default()
    }))
    .context("request a wgpu device")?;

    let exposure = ExposureParams::default();
    let params = GpuParams::new(&exposure, &MaskStack::default(), &raw);
    let pipeline = RawGpuPipeline::new_headless_with_quality(
        &device,
        &queue,
        &raw,
        &params,
        ProcessingQuality::High,
    )
    .context("create headless high-quality GPU pipeline")?;
    let rgb = pipeline
        .render_regression_scene_blocking(&device, &queue, &params)
        .context("render and read scene-linear GPU texture")?;

    let metadata = metadata_json(&args, &raw, &adapter_info)?;
    write_linear_rgb_npz(&args.output, raw.width, raw.height, &rgb, &metadata)
        .with_context(|| format!("write {}", args.output.display()))?;
    println!(
        "wrote {} ({}x{}, linear Rec.2020, {:?})",
        args.output.display(),
        raw.width,
        raw.height,
        adapter_info.backend
    );
    Ok(())
}

fn parse_args() -> Result<Args> {
    let mut backend = None;
    let mut input = None;
    let mut output = None;
    let mut dcp = None;
    let mut values = env::args().skip(1);
    while let Some(argument) = values.next() {
        match argument.as_str() {
            "--backend" => backend = Some(next_value(&mut values, "--backend")?),
            "--input" => input = Some(PathBuf::from(next_value(&mut values, "--input")?)),
            "--output" => output = Some(PathBuf::from(next_value(&mut values, "--output")?)),
            "--dcp" => dcp = Some(PathBuf::from(next_value(&mut values, "--dcp")?)),
            "--version" | "-V" => {
                println!(
                    "auraw-regression-render {} ({})",
                    env!("CARGO_PKG_VERSION"),
                    auraw::SOURCE_REVISION
                );
                std::process::exit(0);
            }
            "--help" | "-h" => {
                print_help();
                std::process::exit(0);
            }
            other => bail!("unknown argument {other:?}; use --help"),
        }
    }
    Ok(Args {
        backend: backend.ok_or_else(|| anyhow!("--backend is required"))?,
        input: input.ok_or_else(|| anyhow!("--input is required"))?,
        output: output.ok_or_else(|| anyhow!("--output is required"))?,
        dcp,
    })
}

fn next_value(values: &mut impl Iterator<Item = String>, option: &str) -> Result<String> {
    values
        .next()
        .ok_or_else(|| anyhow!("{option} requires a value"))
}

fn print_help() {
    println!(concat!(
        "AuRaw canonical image-regression renderer\n\n",
        "Usage:\n",
        "  auraw-regression-render --backend gpu --input FILE --output FILE.npz\n",
        "  [--dcp PROFILE.dcp]\n\n",
        "The output is full-resolution scene-linear D65 Rec.2020 RGB float32, before\n",
        "creative look/tone modules, display encoding, sharpening, or resizing."
    ));
}

fn metadata_json(
    args: &Args,
    raw: &auraw::pipeline::LoadedRaw,
    info: &wgpu::AdapterInfo,
) -> Result<String> {
    let cfa = match raw.cfa_kind {
        CfaKind::Bayer => "bayer",
        CfaKind::XTrans => "xtrans",
    };
    let input_name = args
        .input
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("unknown");
    let raw_sha256 = file_sha256_hex(&args.input)?;
    let renderer_sha256 = env::current_exe()
        .context("resolve regression renderer executable")
        .and_then(|path| file_sha256_hex(&path))?;
    let implementation = "auraw-wgpu-raw-stage-v1";
    let implementation_fingerprint = format!("{implementation}@{}", auraw::SOURCE_REVISION);
    Ok(format!(
        concat!(
            "{{",
            "\"adapter_backend\":\"{adapter_backend}\",",
            "\"adapter_device\":{adapter_device},",
            "\"adapter_device_type\":\"{adapter_device_type}\",",
            "\"adapter_driver\":\"{adapter_driver}\",",
            "\"adapter_driver_info\":\"{adapter_driver_info}\",",
            "\"adapter_name\":\"{adapter_name}\",",
            "\"adapter_vendor\":{adapter_vendor},",
            "\"backend\":\"gpu\",",
            "\"camera_make\":\"{camera_make}\",",
            "\"camera_model\":\"{camera_model}\",",
            "\"cfa\":\"{cfa}\",",
            "\"channels\":[\"R\",\"G\",\"B\"],",
            "\"color_space\":\"linear-rec2020-d65\",",
            "\"dtype\":\"float32\",",
            "\"implementation\":\"{implementation}\",",
            "\"implementation_fingerprint\":\"{implementation_fingerprint}\",",
            "\"input_file\":\"{input_file}\",",
            "\"layout\":\"HWC\",",
            "\"processing_quality\":\"high\",",
            "\"raw_sha256\":\"{raw_sha256}\",",
            "\"renderer\":\"auraw-regression-render\",",
            "\"renderer_sha256\":\"{renderer_sha256}\",",
            "\"schema\":1,",
            "\"source_revision\":\"{source_revision}\",",
            "\"transfer\":\"linear\"",
            "}}"
        ),
        adapter_backend = json_escape(&format!("{:?}", info.backend)),
        adapter_device = info.device,
        adapter_device_type = json_escape(&format!("{:?}", info.device_type)),
        adapter_driver = json_escape(&info.driver),
        adapter_driver_info = json_escape(&info.driver_info),
        adapter_name = json_escape(&info.name),
        adapter_vendor = info.vendor,
        camera_make = json_escape(&raw.camera_make),
        camera_model = json_escape(&raw.camera_model),
        cfa = cfa,
        implementation = implementation,
        implementation_fingerprint = json_escape(&implementation_fingerprint),
        input_file = json_escape(input_name),
        raw_sha256 = raw_sha256,
        renderer_sha256 = renderer_sha256,
        source_revision = json_escape(auraw::SOURCE_REVISION),
    ))
}

fn file_sha256_hex(path: &std::path::Path) -> Result<String> {
    use std::io::Read;

    let mut file = std::fs::File::open(path)
        .with_context(|| format!("open {} for SHA-256", path.display()))?;
    let mut digest = Sha256Context::new(&SHA256);
    let mut buffer = [0u8; 256 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .with_context(|| format!("hash {}", path.display()))?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn json_escape(value: &str) -> String {
    let mut output = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            value if value < ' ' => output.push_str(&format!("\\u{:04x}", value as u32)),
            value => output.push(value),
        }
    }
    output
}
