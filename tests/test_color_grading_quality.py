from __future__ import annotations

from tests.source_helpers import read_source_tree
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
MASKS = (ROOT / "src/pipeline/masks.rs").read_text(encoding="utf-8")
GPU = read_source_tree(ROOT / "src/pipeline/gpu.rs")
COMMON = (ROOT / "src/shaders/common.wgsl").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
WHEEL = (ROOT / "src/ui/components/color_grading.rs").read_text(encoding="utf-8")


def test_four_way_grading_exists_globally_and_on_masks() -> None:
    for field in ("shadows", "midtones", "highlights", "global"):
        assert f"pub {field}: ColorGradeWheel" in BASIC
    assert "pub blending: f32" in BASIC
    assert "pub balance: f32" in BASIC
    assert "pub color_grading: ColorGrading" in BASIC
    assert "pub color_grading: super::ColorGrading" in MASKS
    assert 'CollapsingHeader::new("Color Grading")' in SIDEBAR
    assert "Self::show_local_mask_color_grading" in SIDEBAR


def test_wheels_are_real_two_dimensional_controls_with_precise_entry() -> None:
    assert "ANGULAR_SEGMENTS: usize = 96" in WHEEL
    assert "RADIAL_SEGMENTS: usize = 12" in WHEEL
    assert "Shape::mesh(build_wheel_mesh" in WHEEL
    assert "Sense::click_and_drag()" in WHEEL
    assert "atan2(offset.x)" in WHEEL
    assert "DragValue::new(&mut wheel.hue)" in WHEEL
    assert "DragValue::new(&mut wheel.saturation)" in WHEEL
    assert '"Luminance"' in WHEEL


def test_uniform_abi_carries_global_and_local_grading() -> None:
    for field in (
        "grade_shadows",
        "grade_midtones",
        "grade_highlights",
        "grade_global",
        "grade_options",
    ):
        assert f"{field}: [f32; 4]" in GPU
        assert f"{field}: vec4<f32>" in COMMON
    for field in (
        "mask_grade_shadows",
        "mask_grade_midtones",
        "mask_grade_highlights",
        "mask_grade_global",
        "mask_grade_options",
    ):
        assert f"{field}: [[f32; 4]; MAX_LOCAL_MASKS]" in GPU
        assert f"{field}: array<vec4<f32>, 32>" in COMMON
    assert "size_of::<super::GpuParams>(), 25136" in GPU


def test_grading_is_scene_referred_perceptual_and_gamut_safe() -> None:
    assert "fn apply_color_grading_wheels" in ADJUSTMENTS
    assert "linear_srgb_to_oklab(REC2020_TO_SRGB * rgb)" in ADJUSTMENTS
    assert "perceptual_rec2020_from_oklab_nonnegative" in ADJUSTMENTS
    assert "target_ab / target_chroma" in ADJUSTMENTS
    assert "adjusted = adjusted * exp2(mixer_luminance_ev" in ADJUSTMENTS
    assert "fn apply_explicit_view_node" in ADJUSTMENTS
    assert "return apply_sigmoid_view_transform(view_input);" in ADJUSTMENTS
    assert "apply_optional_profile_look(scene_rgb)" in ADJUSTMENTS
    assert "profile_tone_display_shoulder" in ADJUSTMENTS
    assert "var display_linear = clamp(graded" not in ADJUSTMENTS
    assert "clipping individual RGB channels" in ADJUSTMENTS


def test_tonal_ranges_overlap_smoothly_and_protect_fragile_pixels() -> None:
    assert "fn color_grade_tonal_weights" in ADJUSTMENTS
    assert "log2(max(luminance, 1e-7) / SCENE_MIDDLE_GREY)" in ADJUSTMENTS
    assert "mix(0.60, 2.80" in ADJUSTMENTS
    assert "smoothstep(" in ADJUSTMENTS
    assert "smoothstep(0.025, 0.115" in ADJUSTMENTS
    assert "saturation_guard" in ADJUSTMENTS
    assert "hdr_guard" in ADJUSTMENTS


def test_neutral_grading_is_an_exact_bypass_and_masks_are_layered() -> None:
    assert "if color_grade_strength(shadows, midtones, highlights, global) < 1e-7" in ADJUSTMENTS
    assert "return input_rgb;" in ADJUSTMENTS
    assert "u32::from(has_hsl) | (u32::from(has_grading) << 1)" in GPU
    assert "fn apply_local_color_grading" in ADJUSTMENTS
    assert "(state.w & 2u) == 0u" in ADJUSTMENTS
    assert "rgb = mix(rgb, adjusted, weight)" in ADJUSTMENTS


def test_final_render_pipeline_binds_mask_resources_for_local_grading() -> None:
    render_layout_start = GPU.index("let bgl_adjust_render = device.create_bind_group_layout")
    render_layout_end = GPU.index("let bg_highlights", render_layout_start)
    render_layout = GPU[render_layout_start:render_layout_end]
    assert "texture_array_entry(27" in render_layout
    assert "sampler_entry(28)" in render_layout

    render_group_start = GPU.index("let bg_adjust_render = device.create_bind_group")
    render_group_end = GPU.index("// Storage texture declarations", render_group_start)
    render_group = GPU[render_group_start:render_group_end]
    assert "binding: 27" in render_group
    assert "TextureView(&mask_view)" in render_group
    assert "binding: 28" in render_group
    assert "Sampler(&mask_sampler)" in render_group
