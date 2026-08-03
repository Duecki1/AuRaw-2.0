from pathlib import Path

from tests.source_helpers import read_source_tree

BASIC = read_source_tree(Path("src/pipeline/basicadj.rs"))
GPU = read_source_tree(Path("src/pipeline/gpu.rs"))
PROFILE = read_source_tree(Path("src/pipeline/color_profile.rs"))
RAW_LOADER = read_source_tree(Path("src/pipeline/raw_loader/libraw_loader.rs"))
SIGMOID = read_source_tree(Path("src/pipeline/sigmoid.rs"))
ADJUSTMENTS = read_source_tree(Path("src/shaders/adjustments.wgsl"))
COMMON = read_source_tree(Path("src/shaders/common.wgsl"))


def test_global_exposure_has_no_unconditional_backend_lift() -> None:
    assert "exposure: exposure.exposure," in GPU
    assert "exposure.exposure + super::GLOBAL_EXPOSURE_BACKEND_OFFSET_EV" not in GPU
    assert "LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV" in BASIC
    assert "8 | 9 =>" in BASIC
    assert "10..=CURRENT_PROCESS_VERSION" in BASIC
    assert "self.exposure += LEGACY_GLOBAL_EXPOSURE_BACKEND_OFFSET_EV" in BASIC


def test_metadata_default_exposure_is_resolved_once_with_calibrated_fallback() -> None:
    assert "MISSING_BASELINE_EXPOSURE_FALLBACK_EV: f32 = 1.25" in RAW_LOADER
    assert "fn valid_baseline_exposure" in RAW_LOADER
    assert "value.is_finite() && value > -999.0" in RAW_LOADER
    assert "fn resolve_default_exposure_ev" in RAW_LOADER
    assert "baseline_exposure.unwrap_or(MISSING_BASELINE_EXPOSURE_FALLBACK_EV)" in RAW_LOADER
    assert "baseline + profile_offset_ev" in RAW_LOADER
    assert "pub profile_exposure_offset_ev: f32" in PROFILE
    assert "pub default_exposure_ev: f32" in PROFILE
    assert "profile.default_exposure_ev.to_bits()" in PROFILE


def test_default_view_transform_uses_scene_headroom_shoulder_and_gamut_aware_chroma() -> None:
    assert "fn default_view_chroma_limit" not in ADJUSTMENTS
    assert "let toe_weight = 1.0 - smoothstep(0.018, 0.22, luma)" in ADJUSTMENTS
    assert "fn profile_tone_scene_shoulder_knee" in ADJUSTMENTS
    assert "tone_stats.percentiles_0.w" in ADJUSTMENTS
    assert "tone_stats.percentiles_1.x" in ADJUSTMENTS
    assert "let broad_highlight_pressure" in ADJUSTMENTS
    assert "let isolated_specular" in ADJUSTMENTS
    assert "return mix(0.91, 0.62, scene_pressure)" in ADJUSTMENTS
    assert "let shoulder_knee = profile_tone_scene_shoulder_knee()" in ADJUSTMENTS
    assert "let shoulder_knee = 0.70" not in ADJUSTMENTS
    assert "let shoulder_knee = 0.82" not in ADJUSTMENTS
    assert "let ratio_preserved = positive * (mapped_luma / luma)" in ADJUSTMENTS
    assert "perceptual_gamut_compress_unit_rec2020(ratio_preserved)" in ADJUSTMENTS
    assert "safe_luma(positive)" in ADJUSTMENTS
    assert "peak <= knee" not in ADJUSTMENTS


def test_basic_contrast_is_the_darktable_sigmoid_middle_grey_slope() -> None:
    tonemap = read_source_tree(Path("src/shaders/tonemap.wgsl"))
    assert "fn apply_basic_contrast" not in tonemap
    assert "fn apply_mask_contrast_value" in tonemap
    assert "let pixel_average = max((rgb.r + rgb.g + rgb.b) / 3.0, 0.0)" in tonemap
    assert "-pixel_average / (minimum - pixel_average)" in tonemap
    assert "let contrast = finite_or(params.contrast, defaults.contrast).clamp(0.1, 10.0)" in SIGMOID
    assert "let ref_slope = contrast * CONTRAST_SLOPE_CALIBRATION" in SIGMOID
    assert "sigmoid_params.contrast = sigmoid_contrast_from_percent(exposure.contrast);" in GPU
    assert "let raw_selection_flags = u32::from(exposure.ai_denoise_enabled) << 1;" in GPU
    assert "pub const SIGMOID_CONTRAST_PROCESS_VERSION: u32 = 30;" in BASIC
    assert "pub const PERCENT_SIGMOID_CONTRAST_PROCESS_VERSION: u32 = 31;" in BASIC
    assert "pub const PHOTOGRAPHIC_SIGMOID_CONTRAST_PROCESS_VERSION: u32 = 32;" in BASIC
    assert "fn sigmoid_contrast_from_percent" in BASIC
    assert "DARKTABLE_SIGMOID_CONTRAST_SOFT_MIN: f32 = 0.7" in BASIC
    assert "DARKTABLE_SIGMOID_CONTRAST_SOFT_MAX: f32 = 3.0" in BASIC
    assert "self.contrast = percent_from_sigmoid_contrast" in BASIC


def test_default_sigmoid_matches_darktable_per_channel_color_processing() -> None:
    assert "#[default]\n    PerChannel" in SIGMOID
    assert "darktable default: apply the sigmoid per channel" in SIGMOID
    assert "Camera/DNG default rendering exposure lives independently" in COMMON


def test_final_render_binds_tone_stats_for_scene_headroom_shoulder() -> None:
    # profile_tone_scene_shoulder_knee() is called by the final render entry
    # point and reads @binding(16) tone_stats from tonemap.wgsl. The final
    # pipeline layout and bind group must therefore both expose the buffer.
    layout_start = GPU.index('label: Some("bgl scene look view and output")')
    layout_end = GPU.index('let bgl_adjust_render =\n            reused_layout', layout_start)
    render_layout = GPU[layout_start:layout_end]
    assert 'storage_buffer_entry(16, true)' in render_layout

    group_start = GPU.index('label: Some("bg scene look view and output")')
    group_end = GPU.index('// Storage texture declarations are format-specific', group_start)
    render_group = GPU[group_start:group_end]
    assert 'binding: 16' in render_group
    assert 'resource: tone_stats_buffer.as_entire_binding()' in render_group
