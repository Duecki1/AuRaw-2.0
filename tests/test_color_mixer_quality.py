from tests.source_helpers import read_source_tree
from pathlib import Path
import re


ROOT = Path(__file__).resolve().parents[1]
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
GPU = read_source_tree(ROOT / "src/pipeline/gpu.rs")


def test_color_mixer_does_not_process_in_mathematical_hsl() -> None:
    assert "fn rgb_to_hsl" not in ADJUSTMENTS
    assert "fn hsl_to_rgb" not in ADJUSTMENTS
    assert "HSL lightness" in ADJUSTMENTS
    assert "linear_srgb_to_oklab" in ADJUSTMENTS


def test_neutral_mixer_is_an_exact_no_op() -> None:
    neutral_branch = re.search(
        r"fn apply_color_mixer\([^}]+color_mixer_strength\(\);(.+?)let sample",
        ADJUSTMENTS,
        flags=re.DOTALL,
    )
    assert neutral_branch is not None
    assert "return rgb;" in neutral_branch.group(1)
    assert "max_abs_vec4" in ADJUSTMENTS


def test_selector_is_spatially_stable_but_center_detail_is_not_blurred() -> None:
    assert "fn stabilized_mixer_sample" in ADJUSTMENTS
    assert "range_weight" in ADJUSTMENTS
    assert "hue_agreement" in ADJUSTMENTS
    assert "textureLoad(final_adjustment_tex" in ADJUSTMENTS
    assert "Only hue selection is" in ADJUSTMENTS
    assert "actual RGB detail always comes from the center pixel" in ADJUSTMENTS


def test_luminance_is_ratio_preserving_and_gamut_mapping_is_constant_hue() -> None:
    assert "adjusted = adjusted * exp2(mixer_luminance_ev" in ADJUSTMENTS
    assert "fn nonnegative_rec2020_from_oklab" in ADJUSTMENTS
    assert "binary search" in ADJUSTMENTS
    assert "clamp(hsl.z" not in ADJUSTMENTS


def test_gpu_schedules_full_precision_base_effects_then_mixer_render() -> None:
    prepare = GPU.index('"prepare_adjustment_base"')
    sharpen_tone = GPU.index('"apply_capture_sharpen_and_tone"', prepare)
    effects = GPU.index('"apply_lightroom_effects"', sharpen_tone)
    creative = GPU.index('"apply_creative_effects"', effects)
    render = GPU.index('"apply_lightroom_adjustments"', creative)
    assert prepare < sharpen_tone < effects < creative < render
    assert "work_shader_source(SHADER_ADJUSTMENTS, work_format)" in GPU
    # Two existing demosaic scratch textures are reused after the RAW stage:
    # tex1 holds the pre-tone base, then local Effects; tex2 holds the sharpened
    # post-tone base, then the final creative composite consumed by the mixer.
    assert "binding: 22" in GPU and "TextureView(&tex1_view)" in GPU
    assert "binding: 23" in GPU and "TextureView(&tex2_view)" in GPU
    assert "binding: 24" in GPU and "TextureView(&tex1_view)" in GPU
    assert "binding: 25" in GPU and "TextureView(&tex2_view)" in GPU


def test_named_channels_are_calibrated_in_oklab_not_hsl_angles() -> None:
    pairs = [
        (float(anchor), float(width))
        for anchor, width in re.findall(
            r"smooth_hue_bell\(hue, ([0-9.]+), ([0-9.]+)\)", ADJUSTMENTS
        )
    ]
    assert len(pairs) == 8
    anchors = [pair[0] for pair in pairs]
    widths = [pair[1] for pair in pairs]
    # Pure sRGB red is around 29 degrees in OKLab, not zero degrees.
    assert 0.075 < anchors[0] < 0.09
    # Pure sRGB yellow is around 110 degrees in OKLab, not HSL's 60 degrees.
    assert 0.29 < anchors[2] < 0.32

    def circular_distance(a: float, b: float) -> float:
        distance = abs(a - b)
        return min(distance, 1.0 - distance)

    def bell(hue: float, anchor: float, width: float) -> float:
        t = max(0.0, min(1.0, 1.0 - circular_distance(hue, anchor) / width))
        feather = t * t * (3.0 - 2.0 * t)
        return feather * feather

    for index, hue in enumerate(anchors):
        weights = [bell(hue, anchor, width) for anchor, width in pairs]
        assert weights[index] == max(weights)


def test_hue_sliders_have_extended_range_without_changing_old_edits() -> None:
    basic = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
    sidebar = read_source_tree(ROOT / "src/ui/sidebar.rs")
    assert "HSL_HUE_LIMIT: f32 = 200.0" in basic
    assert sidebar.count("-HSL_HUE_LIMIT..=HSL_HUE_LIMIT") == 2
    assert "clamp(value / 100.0, -2.0, 2.0)" in ADJUSTMENTS
    assert "Preserve the historical +/-100 response exactly" in ADJUSTMENTS

