use anyhow::{bail, Context, Result};
use calibraw_core::pipeline::{load_raw_file, load_raw_file_with_dcp};
use std::env;
use std::path::PathBuf;

#[derive(Debug)]
struct Args {
    input: PathBuf,
    dcp: Option<PathBuf>,
    temperature: Option<f32>,
    tint: Option<f32>,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("calibraw-wb-diagnostics: {error:#}");
        std::process::exit(2);
    }
}

fn run() -> Result<()> {
    let args = parse_args()?;
    let raw = match &args.dcp {
        Some(profile) => load_raw_file_with_dcp(&args.input, profile),
        None => load_raw_file(&args.input),
    }
    .with_context(|| format!("load source image {}", args.input.display()))?;

    let (as_shot_temperature, as_shot_tint) = raw
        .as_shot_white_balance()
        .context("RAW does not expose a camera-space white-balance model")?;
    let selected_temperature = args.temperature.unwrap_or(as_shot_temperature);
    let selected_tint = args.tint.unwrap_or(as_shot_tint);
    let (temperature_offset, tint_offset) = raw
        .white_balance_offsets_from_temperature_tint(selected_temperature, selected_tint)
        .context("selected white balance is outside the supported camera model")?;
    let (selected_wb, camera_to_working, profile_weight) =
        raw.adjusted_white_balance_and_camera_transform(temperature_offset, tint_offset);

    println!("Camera: {} {}", raw.camera_make, raw.camera_model);
    println!("As shot: {as_shot_temperature:.1} K, tint {as_shot_tint:.6}");
    println!("Selected: {selected_temperature:.1} K, tint {selected_tint:.6}");
    println!("Offsets: temperature {temperature_offset:.6}, tint {tint_offset:.6}");
    println!("As-shot multipliers: {}", format_vector(raw.wb_coeffs));
    println!("Selected multipliers: {}", format_vector(selected_wb));
    println!("DNG profile interpolation weight: {profile_weight:.6}");
    println!("Camera -> working matrix:");
    for row in camera_to_working {
        println!("  {}", format_vector(row));
    }
    Ok(())
}

fn format_vector<const N: usize>(values: [f32; N]) -> String {
    values
        .iter()
        .map(|value| format!("{value:.8}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_args() -> Result<Args> {
    let mut values = env::args().skip(1);
    let Some(first) = values.next() else {
        print_usage();
        bail!("missing input RAW path");
    };
    if first == "-h" || first == "--help" {
        print_usage();
        std::process::exit(0);
    }

    let mut args = Args {
        input: PathBuf::from(first),
        dcp: None,
        temperature: None,
        tint: None,
    };
    while let Some(flag) = values.next() {
        match flag.as_str() {
            "--dcp" => {
                args.dcp = Some(PathBuf::from(
                    values.next().context("--dcp requires a profile path")?,
                ));
            }
            "--temperature" => {
                args.temperature = Some(
                    values
                        .next()
                        .context("--temperature requires Kelvin")?
                        .parse::<f32>()
                        .context("parse --temperature")?,
                );
            }
            "--tint" => {
                args.tint = Some(
                    values
                        .next()
                        .context("--tint requires an absolute Tint coordinate")?
                        .parse::<f32>()
                        .context("parse --tint")?,
                );
            }
            _ => bail!("unknown argument {flag:?}"),
        }
    }
    Ok(args)
}

fn print_usage() {
    eprintln!(
        "Usage: calibraw-wb-diagnostics RAW [--dcp PROFILE] [--temperature K] [--tint T]\n\
         Prints As Shot and selected camera multipliers, temperature/tint offsets,\n\
         DNG profile weight, and the camera->working matrix."
    );
}
