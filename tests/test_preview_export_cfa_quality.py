from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
PROCESSING = (ROOT / "src/pipeline/processing.rs").read_text(encoding="utf-8")
LENSFUN = (ROOT / "src/pipeline/lensfun.rs").read_text(encoding="utf-8")


def test_preview_proxy_co_sites_every_cfa_phase_in_one_source_macrocell() -> None:
    assert "one complete output CFA cell summarizes one shared source macrocell" in PROCESSING
    assert "let macro_y0 = y + (py / cfa_period) * scale * cfa_period;" in PROCESSING
    assert "let macro_x0 = x + (px / cfa_period) * scale * cfa_period;" in PROCESSING
    assert "(sy - y) % cfa_period != output_phase_y" in PROCESSING
    assert "(sx - x) % cfa_period != output_phase_x" in PROCESSING
    proxy_body = PROCESSING[PROCESSING.index("pub fn build_region_proxy") : PROCESSING.index("fn nearest_cfa_sample")]
    assert "let source_x0 = x + px * scale;" not in proxy_body
    assert "let source_y0 = y + py * scale;" not in proxy_body


def test_lensfun_bayer_warp_uses_subpixel_same_phase_interpolation() -> None:
    assert "sample_corrected_cfa_subpixel(" in LENSFUN
    assert "sample_bayer_phase_bilinear(" in LENSFUN
    assert "let top = (lerp(a.0, b.0, tx), lerp(a.1, b.1, tx));" in LENSFUN
    assert "let bottom = (lerp(c.0, d.0, tx), lerp(c.1, d.1, tx));" in LENSFUN
    assert "lerp(top.0, bottom.0, ty)" in LENSFUN
    assert "lerp(top.1, bottom.1, ty)" in LENSFUN


def test_lensfun_vignetting_is_interpolated_with_the_same_raw_samples() -> None:
    assert "corrected_sample_at(raw, indices[0], vignette_enabled, vignette_gains)" in LENSFUN
    assert "vignette_gains.get(index).copied().unwrap_or(1.0).max(0.0)" in LENSFUN


def test_reference_demosaic_rejects_isolated_false_color_without_changing_ui_defaults() -> None:
    bayer = (ROOT / "src/shaders/pass4.wgsl").read_text(encoding="utf-8")
    xtrans = (ROOT / "src/shaders/xtrans_pass7.wgsl").read_text(encoding="utf-8")
    assert "fn bayer_reference_false_color_guard" in bayer
    assert "bayer_median5(" in bayer
    assert "0.55 * smoothstep(0.006, 0.055, disagreement)" in bayer
    assert "camera_rgb = bayer_reference_false_color_guard(pos, reference);" in bayer
    assert "fn xt_reference_false_color_guard" in xtrans
    assert "xt_median5(" in xtrans
    assert "0.50 * smoothstep(0.006, 0.055, disagreement)" in xtrans
    assert "camera_rgb = xt_reference_false_color_guard(pos, reference);" in xtrans
