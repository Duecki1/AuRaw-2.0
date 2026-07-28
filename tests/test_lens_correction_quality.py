from pathlib import Path

from tests.source_helpers import read_source_tree

ROOT = Path(__file__).resolve().parents[1]
LENSFUN = (ROOT / "src/pipeline/lensfun.rs").read_text(encoding="utf-8")
APP = read_source_tree(ROOT / "src/app.rs")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
SLIDER = (ROOT / "src/ui/components/adjustment_slider.rs").read_text(encoding="utf-8")
WHEEL = (ROOT / "src/ui/components/color_grading.rs").read_text(encoding="utf-8")
RAW_LOADER = read_source_tree(ROOT / "src/pipeline/raw_loader.rs")
PROCESSING = (ROOT / "src/pipeline/processing.rs").read_text(encoding="utf-8")


def test_double_click_resets_adjustment_sliders_and_color_wheels() -> None:
    assert SLIDER.count("double_clicked()") >= 3
    assert "set_numeric(value, reset_value" in SLIDER
    assert "Double-click to reset" in SLIDER
    assert "if response.double_clicked()" in WHEEL
    assert "wheel.reset();" in WHEEL


def test_slider_reset_logic_remains_rust_2021_compatible() -> None:
    assert "&& let" not in SLIDER
    assert "if !reset_requested {" in SLIDER
    assert "if let (Some(origin), Some(position), true) = pointer" in SLIDER


def test_optics_ui_has_enable_brand_and_lens_controls() -> None:
    assert "AdjustmentSection::Optics" in SIDEBAR
    assert '"Optics"' in SIDEBAR
    assert 'adjustment_section(ui, "Lens Corrections"' in SIDEBAR
    assert 'Checkbox::new(&mut state.enabled, "Enabled")' in SIDEBAR
    assert 'ui.label("Brand")' in SIDEBAR
    assert 'ui.label("Lens")' in SIDEBAR


def test_raw_metadata_drives_automatic_lensfun_application() -> None:
    for field in (
        "lens_make",
        "lens_model",
        "focal_length",
        "aperture",
        "focus_distance",
    ):
        assert f"pub {field}:" in RAW_LOADER
        assert f"{field}: raw.{field}" in PROCESSING
    assert "LensCorrectionState::from_catalog(lensfun_catalog(&original_raw))" in APP
    assert "if lens_correction.enabled" in APP
    assert "apply_lensfun_correction(&original_raw, &selection)" in APP
    assert "Automatically applied {} from RAW metadata" in APP
    assert "enabled: catalog.available && selected.is_some()" in APP


def test_automatic_lens_matching_is_metadata_tolerant_and_conservative() -> None:
    assert "fn canonical_lens_model" in LENSFUN
    assert "fn maker_is_compatible" in LENSFUN
    assert "fn profile_supports_capture" in LENSFUN
    assert "compatible_lenses(database, camera)" in LENSFUN
    assert "if *best_score < 0.90" in LENSFUN
    assert "if *best_score - *runner_up < 0.08" in LENSFUN
    assert "Auto-detected {} from RAW metadata" in LENSFUN
    assert "let auto_match = find_auto_lens(&database, camera, raw);" in LENSFUN
    assert "raw.lens_model.trim().is_empty() && lenses.len() == 1" not in LENSFUN
    assert "fn lens_model_codes" in LENSFUN
    assert "return Some(0.995);" in LENSFUN
    assert '“E 28-75mm F2.8 A063”' in LENSFUN

def test_lensfun_uses_stable_03_database_and_modifier_abi() -> None:
    for symbol in (
        "lf_db_new",
        "lf_db_load_file",
        "lf_db_find_lenses_hd",
        "lf_modifier_new",
        "lf_modifier_initialize",
        "lf_modifier_add_coord_callback_scale",
        "lf_modifier_apply_subpixel_geometry_distortion",
        "lf_modifier_apply_color_modification",
    ):
        assert symbol in LENSFUN
    for development_only_symbol in (
        "lf_db_create",
        "lf_db_load_path",
        "lf_modifier_create",
        "lf_modifier_enable_distortion_correction",
        "lf_modifier_enable_tca_correction",
        "lf_modifier_enable_vignetting_correction",
        "lf_modifier_enable_scaling",
    ):
        assert development_only_symbol not in LENSFUN


def test_lensfun_search_radius_has_an_explicit_signed_type() -> None:
    assert "let radius_limit: i32 = match raw.cfa_kind" in LENSFUN


def test_lens_correction_preserves_bayer_green_phase_without_nearest_neighbor_warping() -> None:
    assert "fn lensfun_rgb_channel(cfa_index: u8)" in LENSFUN
    assert "_ => 1" in LENSFUN
    assert "sample_corrected_cfa_subpixel(" in LENSFUN
    assert "sample_bayer_phase_bilinear(" in LENSFUN
    assert "bayer_axis_samples(" in LENSFUN
    assert "raw.color_indices.get(index).copied() != Some(channel)" in LENSFUN
    correction_loop = LENSFUN[LENSFUN.index("fn correct_mosaic") : LENSFUN.index("fn build_vignette_gain_map")]
    assert "nearest_matching_sample(raw, source_x, source_y, cfa_index)" not in correction_loop


def test_lensfun_defers_common_distortion_until_float_geometry() -> None:
    apply_body = LENSFUN[LENSFUN.index("pub(super) fn apply") : LENSFUN.index("fn initialize_modifier")]
    assert "LF_MODIFY_TCA | LF_MODIFY_VIGNETTING" in apply_body
    assert "LF_MODIFY_DISTORTION" in apply_body
    assert "build_lens_geometry_map(raw, &geometry_modifier, geometry_flags)" in apply_body
    assert "correct_mosaic(raw, &early_modifier, early_flags, lens_geometry)" in apply_body
    assert "LF_MODIFY_TCA | LF_MODIFY_VIGNETTING | LF_MODIFY_DISTORTION" not in apply_body


def test_xtrans_tca_interpolates_same_color_before_nearest_fallback() -> None:
    sampler = LENSFUN[
        LENSFUN.index("fn sample_corrected_cfa_subpixel") : LENSFUN.index("fn sample_bayer_phase_bilinear")
    ]
    assert "sample_xtrans_same_color_weighted(" in sampler
    assert sampler.index("sample_xtrans_same_color_weighted(") < sampler.index("nearest_matching_sample(")
    assert "raw.color_indices[index] != channel" in sampler


def test_manual_profile_selection_remains_available_without_camera_match() -> None:
    assert ".unwrap_or_else(|| all_lenses(&database))" in LENSFUN
    assert "let camera = find_camera(&database" in LENSFUN
    assert "find_lens(&database, camera, selection)" in LENSFUN
    assert "no Lensfun camera match" in LENSFUN


def test_lens_toggle_preserves_masks_and_requests_refresh() -> None:
    processing_export = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
    masks_ui = (ROOT / "src/ui/sidebar/masks.rs").read_text(encoding="utf-8")
    lens_rebuild = processing_export[
        processing_export.index("fn apply_pending_lens_correction") : processing_export.index(
            "pub(crate) fn note_preview_motion"
        )
    ]
    assert "self.masks.clear()" not in lens_rebuild
    assert "let preview_masks = self.masks.clone();" in lens_rebuild
    assert "self.note_lens_correction_changed_for_masks();" in lens_rebuild
    assert 'ui.button("Update masks")' in masks_ui


def test_lens_toggle_prepares_the_preview_off_the_ui_thread_on_all_platforms() -> None:
    processing_export = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
    worker = processing_export[
        processing_export.index("fn start_lens_correction_task")
        : processing_export.index("fn poll_lens_correction_worker")
    ]
    assert '.name("auraw-lens-correction".to_owned())' in worker
    assert ".spawn(move ||" in worker
    assert "apply_lensfun_correction(&original_raw, selection)" in worker
    assert "RawGpuPipeline::new_headless_with_quality(" not in worker
    assert "pipeline.upload_raw_tile(" in processing_export
    assert "lens_correction_receiver.is_some()" in processing_export
    assert "!lens_correction_busy" in SIDEBAR
