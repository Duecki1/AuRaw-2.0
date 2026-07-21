from __future__ import annotations

from pathlib import Path

from tests.source_helpers import read_source_tree

ROOT = Path(__file__).resolve().parents[1]
APP = read_source_tree(ROOT / "src/app.rs")
INPAINT = (ROOT / "src/inpainting.rs").read_text(encoding="utf-8")
MASKS = (ROOT / "src/pipeline/masks.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
INPAINT_SIDEBAR = (ROOT / "src/ui/sidebar/inpainting.rs").read_text(encoding="utf-8")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
READBACK = (ROOT / "src/pipeline/gpu/readback.rs").read_text(encoding="utf-8")


def test_inpainting_uses_full_resolution_local_raw_crop() -> None:
    capture = APP[APP.index("fn capture_inpaint_source("):APP.index("fn start_inpaint_worker")]
    assert ".loaded_raw" in capture
    assert "crop_raw(full_raw" in capture
    assert "new_headless_reusing_programs" in capture
    assert "ProcessingQuality::Preview" in capture
    assert ".preview_raw" not in capture
    assert "inpaint_capture_rect" in capture


def test_lama_stays_512_but_float_output_is_not_quantized() -> None:
    assert "const LAMA_EDGE: u32 = 512;" in INPAINT
    infer = INPAINT[INPAINT.index("fn infer_lama("):INPAINT.index("fn run_lama_session(")]
    assert "sample_lama_bilinear(&output" in infer
    assert "srgb_encoded_to_rec2020_linear" in infer
    assert "f16::from_f32" in infer
    assert "chw_to_rgba(&output)" not in infer


def test_inpaint_results_are_sparse_full_coordinate_rgba16f_patches() -> None:
    assert "pub patches: Arc<[InpaintPatch]>" in MASKS
    assert "pub rgba16f: Arc<[u16]>" in MASKS
    assert "pub fn new_linear(" in MASKS
    assert "source_width" in MASKS and "source_height" in MASKS
    assert "prepared.origin_x + crop.x" in INPAINT
    assert "prepared.origin_y + crop.y" in INPAINT


def test_gpu_inpaint_texture_is_linear_rgba16f_without_srgb_redecode() -> None:
    texture = GPU[GPU.index('label: Some("auraw pre-adjustment inpaint layer")'):]
    texture = texture[:1000]
    assert "wgpu::TextureFormat::Rgba16Float" in texture
    assert "bytes_per_row: Some(raw.width * 8)" in texture
    assert "let replacement_neutral = replacement.rgb;" in ADJUSTMENTS
    assert "let linear_srgb = mix(lo, hi, cutoff);" not in ADJUSTMENTS


def test_lama_boundary_keeps_wide_gamut_source_float_until_tensor_build() -> None:
    capture = APP[APP.index("fn capture_inpaint_source("):APP.index("fn start_inpaint_worker")]
    assert "rgb_rec2020" in capture
    assert "scene_rec2020_to_srgb8" not in APP
    assert "pub rgb_rec2020: Vec<f32>" in INPAINT
    assert "build_lama_image_tensor(&prepared" in INPAINT
    assert "rec2020_linear_to_model_srgb" in INPAINT


def test_resized_inpaint_capture_converts_camera_rgb_and_antialiases() -> None:
    resize_shader = GPU[
        GPU.index('const SHADER_INPAINT_DOWNSAMPLE: &str = r#"') : GPU.index(
            '"#;', GPU.index('const SHADER_INPAINT_DOWNSAMPLE: &str = r#"')
        )
    ]
    for row in range(3):
        assert f"dot(params.cam_to_working_{row}.xyz, camera_rgb)" in resize_shader
    assert "sample_camera_bilinear" in resize_shader
    assert "samples_x = clamp(u32(ceil(scale.x)), 1u, 8u)" in resize_shader
    assert "samples_y = clamp(u32(ceil(scale.y)), 1u, 8u)" in resize_shader

    resize_params = GPU[
        GPU.index("let resize_params = InpaintResizeParams {") : GPU.index(
            "};", GPU.index("let resize_params = InpaintResizeParams {")
        )
    ]
    for row in range(3):
        assert f"cam_to_working_{row}: params.cam_to_srgb_{row}" in resize_params


def test_inpainting_keeps_binary_model_mask_and_soft_composite_edge() -> None:
    infer = INPAINT[INPAINT.index("fn infer_lama("):INPAINT.index("fn localize_dabs(")]
    assert "rasterize_inpaint_dabs_binary" in infer
    assert "rasterize_brush_dabs" in infer
    assert "build_lama_mask_tensor(&inference_mask" in infer
    assert "replacement_mask[patch_index] = composite_mask[source_index];" in infer
    assert "inpaint_brush_feather" not in APP
    assert '"Feather"' not in INPAINT_SIDEBAR
    assert "feather: 0.0" in PREVIEW
    assert "alpha += f32::from(self.mask[index])" in MASKS
    assert "alpha.clamp(0.0, 1.0)" in MASKS


def test_sparse_patch_projection_filters_rgb_and_coverage() -> None:
    assert "pub fn sample_linear_rec2020_bilinear(" in MASKS
    sampler = MASKS[
        MASKS.index("pub fn sample_linear_rec2020_bilinear(") : MASKS.index(
            "#[derive(Clone, Debug, PartialEq, serde::Deserialize, serde::Serialize)]",
            MASKS.index("pub fn sample_linear_rec2020_bilinear("),
        )
    ]
    assert "self.has_valid_storage_layout()" in sampler
    assert "self.is_valid()" not in sampler
    upload = GPU[GPU.index("pub fn update_inpaint_layer("):GPU.index("pub const fn mask_atlas_edge")]
    assert "sample_linear_rec2020_bilinear(source_x, source_y)" in upload
    assert "composite_inpaint_rgba16f(" in upload
    assert "source_alpha + retained_destination" in GPU
    assert "return mix(working, replacement_working, clamp(replacement.a, 0.0, 1.0));" in ADJUSTMENTS


def test_large_inpaint_scene_readback_is_chunked_below_wgpu_buffer_limit() -> None:
    assert "MAX_RGBA32_READBACK_CHUNK_BYTES: u64 = 64 * 1024 * 1024" in READBACK
    assert "rgba32_readback_rows_per_chunk(width)?" in READBACK
    assert "while row_offset < height" in READBACK
    scene_conversion = GPU[GPU.index("fn render_scene_conversion_blocking("):GPU.index("fn encode_raw_stage(")]
    assert "read_rgba32_texture_rgb_blocking(" in scene_conversion
    assert "create_rgba32_readback_buffer(" not in scene_conversion
