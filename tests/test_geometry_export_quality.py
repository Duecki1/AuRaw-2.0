from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
GEOMETRY = (ROOT / "src/pipeline/geometry.rs").read_text(encoding="utf-8")


def _body(name: str, next_name: str) -> str:
    start = EXPORT.index(f"fn {name}")
    end = EXPORT.index(f"fn {next_name}", start)
    return EXPORT[start:end]


def test_geometry_export_stages_native_float_linear_rows() -> None:
    png = _body("export_tiled_png_geometry", "export_tiled_jpeg_geometry")
    jpeg = _body("export_tiled_jpeg_geometry", "validate_linear_rgb_raster")
    for body in (png, jpeg):
        assert "stream_tiled_linear_rows(context, request" in body
        assert "bytemuck::cast_slice(row)" in body
        assert "validate_linear_rgb_raster" in body
        assert "GeometryResampler::new" in body
        assert "render_tiled_srgb" not in body
        assert "geometry_output_row" not in body


def test_geometry_resampling_happens_before_output_sharpen_and_encoding() -> None:
    png = _body("export_tiled_png_geometry", "export_tiled_jpeg_geometry")
    jpeg = _body("export_tiled_jpeg_geometry", "validate_linear_rgb_raster")
    for body in (png, jpeg):
        assert body.index("resampler.output_row(y)") < body.index("output_sharpen.push_row")
        assert "FinalSizeOutputSharpen::new" in body
        assert "output_sharpen.finish" in body
    sharpen = EXPORT[EXPORT.index("struct FinalSizeOutputSharpen") : EXPORT.index("struct LinearLightResizer")]
    assert sharpen.index("output_sharpen_linear_row") < sharpen.index("encode_output_row")


def test_geometry_sampler_uses_combined_inverse_map_and_ewa_mitchell() -> None:
    assert "pub(crate) struct GeometryInverseMap" in GEOMETRY
    assert "center_x + source_dx - 0.5" in GEOMETRY
    assert "center_y + source_dy - 0.5" in GEOMETRY
    sampler = EXPORT[EXPORT.index("struct GeometryResampler") : EXPORT.index("fn export_tiled_jpeg(")]
    assert "inverse_map.pixel_jacobian()" in sampler
    assert "let weight = mitchell_netravali_f32(radius_squared.sqrt())" in sampler
    assert "major_scale = lambda_major.sqrt().max(1.0)" in sampler
    assert "minor_scale = lambda_minor.sqrt().max(1.0)" in sampler
    assert "sample_rgb_bilinear" not in EXPORT


def test_lens_distortion_is_composed_into_the_float_geometry_resample() -> None:
    assert "pub struct LensGeometryMap" in GEOMETRY
    assert "lens_geometry: Option<&'a LensGeometryMap>" in GEOMETRY
    assert "GeometryInverseMap::new_with_lens" in EXPORT
    assert "request.raw.lens_geometry.as_deref()" in EXPORT
    assert "request.raw.lens_geometry.is_some()" in EXPORT
    assert "pixel_jacobian_at(output_x as f32, output_y as f32)" in EXPORT


def test_geometry_regressions_cover_identity_and_nonidentity() -> None:
    assert "fn geometry_resampler_identity_is_exact_in_linear_space" in EXPORT
    assert "fn geometry_resampler_quarter_turn_preserves_exact_pixels" in EXPORT
    assert "fn geometry_downsample_accumulates_linear_values_before_encoding" in EXPORT
