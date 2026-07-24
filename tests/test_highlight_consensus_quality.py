from __future__ import annotations

from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HIGHLIGHT = (ROOT / "src/shaders/highlight_lch_pass.wgsl").read_text(encoding="utf-8")
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar/develop.rs").read_text(encoding="utf-8")


def test_new_edits_use_process_versioned_consensus_highlight_solver() -> None:
    assert "pub const HIGHLIGHT_CONSENSUS_PROCESS_VERSION: u32 = 15;" in BASIC
    assert "pub const BASIC_TONE_RESPONSE_PROCESS_VERSION: u32 = 16;" in BASIC
    assert "pub const CURRENT_PROCESS_VERSION: u32 = BASIC_TONE_RESPONSE_PROCESS_VERSION;" in BASIC
    assert "const HIGHLIGHT_CONSENSUS_PROCESS_VERSION: u32 = 15u;" in HIGHLIGHT
    # The new behaviour must be gated so process-14 sidecars keep their saved look.
    assert HIGHLIGHT.count("params.process_info.x >= HIGHLIGHT_CONSENSUS_PROCESS_VERSION") >= 3


def test_guided_highlight_propagation_rejects_cross_colour_boundaries() -> None:
    assert "fn highlight_chroma_distance_squared" in HIGHLIGHT
    assert "let chroma_continuity = 1.0 / (1.0 + 7.0 * chroma_disagreement);" in HIGHLIGHT
    assert "boundary_weight = mix(1.0, chroma_continuity, outward_reliability);" in HIGHLIGHT
    assert "fn highlight_mask_mismatch_weight" in HIGHLIGHT
    assert "topology_weight = highlight_mask_mismatch_weight(center_mask, neighbour_mask);" in HIGHLIGHT


def test_surviving_sensor_channels_gate_candidate_colour() -> None:
    assert "fn highlight_known_channel_compatibility" in HIGHLIGHT
    assert "if known_count < 1.5" in HIGHLIGHT
    assert "let center_shape = center_rgb / center_mean;" in HIGHLIGHT
    assert "let candidate_shape = candidate_rgb / candidate_mean;" in HIGHLIGHT
    assert "anchor_weight = highlight_known_channel_compatibility" in HIGHLIGHT


def test_fully_clipped_regions_use_chroma_consensus_instead_of_blind_rgb_average() -> None:
    assert "chroma_signature_sum" in HIGHLIGHT
    assert "chroma_signature_energy_sum" in HIGHLIGHT
    assert "let signature_variance = max(" in HIGHLIGHT
    assert "consensus = 1.0 / (1.0 + 5.0 * signature_variance);" in HIGHLIGHT
    assert "let neutral_candidate = vec3<f32>(highlight_intensity(candidate));" in HIGHLIGHT
    assert "candidate = mix(neutral_candidate, candidate, colour_gate);" in HIGHLIGHT
    # Confidence must also fall when colour evidence conflicts, allowing later
    # refinement passes to replace a weak solution instead of locking it in.
    assert "* mix(1.0, consensus, smoothstep(0.34, 1.0, unknown_fraction))" in HIGHLIGHT


def test_process_13_denoise_opt_in_does_not_silently_enable_process_15_highlights() -> None:
    # Editing denoise on a saved process-13 image should opt only into process 14's
    # denoise formulas; otherwise an unrelated NR tweak would change highlights.
    assert "exposure.process_version = SENSOR_DENOISE_PROCESS_VERSION;" in SIDEBAR
    assert "exposure.process_version = CURRENT_PROCESS_VERSION;" not in SIDEBAR
