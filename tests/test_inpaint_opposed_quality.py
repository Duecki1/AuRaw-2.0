from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HIGHLIGHT = (ROOT / "src/shaders/highlights.wgsl").read_text(encoding="utf-8")
RAW_LOADER = (ROOT / "src/pipeline/raw_loader.rs").read_text(encoding="utf-8")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar/develop.rs").read_text(encoding="utf-8")


def test_all_supported_edits_migrate_to_inpaint_opposed() -> None:
    assert "pub const INPAINT_OPPOSED_PROCESS_VERSION: u32 = 29;" in BASIC
    assert "pub const CURRENT_PROCESS_VERSION: u32 = PHOTOGRAPHIC_SIGMOID_CONTRAST_PROCESS_VERSION;" in BASIC
    assert "self.highlight_method = HighlightReconstructionMethod::InpaintOpposed;" in BASIC
    assert "#[serde(other)]" in BASIC


def test_full_image_chrominance_matches_darktable_contract() -> None:
    assert "let clip = 0.987 * clip_threshold.max(0.01);" in RAW_LOADER
    assert "let mask_width = width / 3;" in RAW_LOADER
    assert "let mask_height = height / 3;" in RAW_LOADER
    assert "let aligned_mask_width = mask_width.div_ceil(8) * 8;" in RAW_LOADER
    assert "let aligned_mask_height = mask_height.div_ceil(8) * 8;" in RAW_LOADER
    assert "for offset_y in -3isize..=3" in RAW_LOADER
    assert "for offset_x in -3isize..=3" in RAW_LOADER
    assert "value > 0.2 * channel_clip" in RAW_LOADER
    assert "counts[color] > 100.0" in RAW_LOADER


def test_shader_uses_cube_root_opposed_reference_and_preserves_lower_bound() -> None:
    assert "fn inpaint_opposed_refavg" in HIGHLIGHT
    assert "pow(mean[channel] / count[channel], 1.0 / 3.0)" in HIGHLIGHT
    assert "let chrominance = params.highlight_options[color + 1u];" in HIGHLIGHT
    assert "return max(original, reference + chrominance);" in HIGHLIGHT


def test_ui_controls_do_not_select_image_specific_process_versions() -> None:
    assert "exposure.process_version =" not in SIDEBAR
