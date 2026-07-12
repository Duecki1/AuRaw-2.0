# Camera-profile engine

AuRaw now keeps camera characterization, creative DCP stages, and display/output colour management as distinct operations instead of baking them into one matrix.

## Processing order

1. LibRaw provides linear camera samples, CFA metadata, white balance, AnalogBalance, CameraCalibration fallback records, and BaselineExposure. The selected embedded or external DCP is parsed directly.
2. Dual-illuminant DNG matrices are blended in reciprocal correlated colour temperature. `ColorMatrix`, compatible `CameraCalibration`, and `ForwardMatrix` records share the same interpolation weight; directly parsed records take precedence over the LibRaw fallback.
3. The camera-to-working transform follows either `FM × D × inverse(AB × CC)` or the pseudoinverse/Bradford path when no forward matrix exists. The result is converted from the D50 profile connection space to linear Rec.2020 D65.
4. The interpolated DCP HueSat map is evaluated in linear ProPhoto RGB/HSV, followed by DNG BaselineExposure plus BaselineExposureOffset.
5. User scene-linear controls run.
6. The DCP LookTable and profile tone curve run before the adaptive scene-to-display transform.
7. A replaceable 33³ ICC output LUT converts display-referred linear Rec.2020 to encoded device RGB.

Adaptive tone analysis includes the fixed HueSat map, DNG default exposure, LookTable, and profile tone curve used by final rendering, while deliberately ignoring the user's live Exposure slider so histogram bounds remain stable.

## Public API

- `DcpProfile::from_path` reads embedded DNG profile tags, TIFF/BigTIFF profile IFDs, and standalone `.dcp` files.
- `load_raw_file_with_dcp` applies a standalone DCP's matrices and creative stages while retaining the raw file's AnalogBalance, CameraCalibration compatibility, white balance, and BaselineExposure.
- `CameraProfile::from_dcp` resolves the creative profile for an interpolation weight.
- `IccOutputTransform::from_icc` builds a CPU/GPU transform for ICC v2/v4 RGB matrix-shaper display or output profiles.
- `RawGpuPipeline::set_display_icc_profile` and `set_output_icc_profile` replace the active output transform without rebuilding pipelines.
- `IccOutputTransform::transform_rgb` applies the same transform during CPU-side export.

## ICC scope

The built-in CMM supports RGB matrix-shaper profiles with XYZ PCS, RGB colourant tags, sampled/gamma/parametric TRCs, media white, and the four rendering-intent choices. LUT-based and non-RGB profiles return an explicit error rather than being silently interpreted as sRGB. A future LittleCMS-backed adapter can extend this interface for CMYK and complex A2B/B2A profiles without changing the GPU LUT contract.

The DCP path targets standard SDR camera profiles. DNG 1.6 HDR `ProfileDynamicRange` and gain-table stages are outside the current pipeline and are not applied as though they were SDR profile data.

This product includes DNG technology under license by Adobe.

## Validation

Run:

```sh
python3 scripts/validate_camera_profiles.py
python3 scripts/validate_demosaic.py
cargo test
```

The first script checks the Rust/WGSL ABI, profile stage order, DCP tag/container support, ICC plumbing, matrix inverses, and CPU/GPU LUT packing. `cargo test` remains the authoritative compiler and shader-parser check on a machine with the Rust and LibRaw build dependencies installed.
