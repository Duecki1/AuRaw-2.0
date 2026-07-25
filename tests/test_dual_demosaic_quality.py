from __future__ import annotations

import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
DUAL = (ROOT / "src/shaders/dual_demosaic.wgsl").read_text(encoding="utf-8")
BAYER = (ROOT / "src/shaders/pass4.wgsl").read_text(encoding="utf-8")
XTRANS = (ROOT / "src/shaders/xtrans_pass7.wgsl").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")
CORPUS = (ROOT / "regression/corpus.yaml").read_text(encoding="utf-8")
GENERATOR = (ROOT / "regression/generate_synthetic_corpus.py").read_text(encoding="utf-8")


def test_dual_demosaic_has_real_low_frequency_buffers_and_confidence() -> None:
    assert "fn dual_green_reconstruct" in DUAL
    assert "fn dual_rgb_reconstruct" in DUAL
    assert "dual_green_write" in DUAL
    assert "dual_low_write" in DUAL
    assert "params.noise_read" in DUAL
    assert "params.noise_shot" in DUAL
    assert "red.confidence" in DUAL
    assert "green_sample.a" in DUAL
    assert "q0 = pos + vec2<i32>(dx, dy)" in DUAL
    assert "q1 = pos - vec2<i32>(dx, dy)" in DUAL


def test_dual_finish_blends_independent_buffers_by_noise_adjusted_confidence() -> None:
    for shader in (BAYER, XTRANS):
        assert "noise_floor = 2.25 * sqrt" in shader
        assert "let low_confidence = clamp(low.a" in shader
        assert "let disagreement = smoothstep" in shader
        assert "mix(low.rgb, reference" in shader
    assert "fn low_detail_rgb_at" not in BAYER
    assert "fn xt_low_detail" not in XTRANS


def test_dual_gpu_cost_is_dispatched_only_when_enabled() -> None:
    assert "fn needs_dual_demosaic_passes" in GPU
    assert "self.demosaic_mode >= 1.5" in GPU
    assert "if params.needs_dual_demosaic_passes()" in GPU
    assert '"dual_green_reconstruct"' in GPU
    assert '"dual_rgb_reconstruct"' in GPU
    assert "CfaKind::Bayer => (&highlight_work_a_view, &highlight_work_b_view)" in GPU
    assert "CfaKind::XTrans => (&tex1_view, &tex2_view)" in GPU


def test_synthetic_cfa_alias_suite_covers_four_distinct_failure_modes() -> None:
    for name in (
        "alias-weave",
        "alias-zone-plate",
        "alias-foliage",
        "alias-chroma-crossing",
    ):
        assert CORPUS.count(f"name: {name}") == 2

    for marker in (
        "woven fabric",
        "radial zone plate",
        "foliage-like",
        "chromatic stripe crossings",
    ):
        assert marker in GENERATOR


def test_synthetic_cfa_alias_fixture_hashes_match_manifest() -> None:
    fixtures = {
        "synthetic-bayer.dng": "74b940c8020ea0572d553f10ae1fb8fa3858965a1feabcf600bd74ee068340ff",
        "synthetic-xtrans.dng": "a1d6b7d8dd3590fec4a1c64afd635603b015c23be65144b0959835dbe352b2c4",
    }
    for name, expected in fixtures.items():
        path = ROOT / "regression/raw" / name
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected
        assert expected in CORPUS
