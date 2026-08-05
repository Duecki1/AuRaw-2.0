from tests.source_helpers import read_source_tree
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
MASKS = (ROOT / "src/pipeline/masks.rs").read_text(encoding="utf-8")
APP = read_source_tree(ROOT / "src/app.rs")
PREVIEW = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
GPU = read_source_tree(ROOT / "src/pipeline/gpu.rs")
COMMON = (ROOT / "src/shaders/common.wgsl").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")


def test_mask_types_and_placeholders_are_present() -> None:
    for kind in (
        "Brush", "Radial", "Linear", "Subject", "Background", "Object",
    ):
        assert kind in MASKS
        assert f"MaskKind::{kind}" in SIDEBAR
    assert "· soon" in SIDEBAR


def test_brush_radial_and_linear_interactions_are_cross_platform_egui() -> None:
    assert "Sense::drag()" in PREVIEW
    assert "response.is_pointer_button_down_on()" in PREVIEW
    assert "BrushMode::Paint" in PREVIEW
    assert "BrushMode::Erase" in PREVIEW
    assert "MaskGeometry::Radial" in PREVIEW
    assert "MaskGeometry::Linear" in PREVIEW
    # Mask editing itself stays on the shared egui path. Android-only code in
    # this module is limited to the press-and-hold original-preview gesture.
    mask_interaction = PREVIEW[PREVIEW.index("fn handle_mask_interaction"):]
    assert "cfg(target_os = \"android\")" not in mask_interaction
    assert "distance_px" in PREVIEW


def test_submasks_support_lightroom_style_boolean_composition() -> None:
    assert "MaskCombineMode::Add" in MASKS
    assert "MaskCombineMode::Subtract" in MASKS
    assert "MaskCombineMode::Intersect" in MASKS
    assert "*dst = dst.max(src)" in MASKS
    assert "*dst *= 1.0 - src" in MASKS
    assert "*dst *= src" in MASKS


def test_submasks_have_user_renameable_names() -> None:
    assert "pub name: String" in MASKS
    assert "name: kind.label().to_owned()" in MASKS
    assert "component.name" in SIDEBAR


def test_export_mask_atlas_helpers_are_reexported() -> None:
    pipeline_mod = (ROOT / "src/pipeline/mod.rs").read_text(encoding="utf-8")
    assert "export_mask_atlas_edge," in pipeline_mod
    assert "export_mask_atlas_edge_limit," in pipeline_mod


def test_mask_atlas_is_shared_by_preview_and_export() -> None:
    assert "R16Float" in GPU
    assert "TextureViewDimension::D2Array" in GPU
    assert "update_mask_layer" in GPU
    assert "rasterize_layer_f16" in MASKS
    assert "f16::from_f32" in MASKS
    assert "mark_mask_geometry_dirty" in APP
    assert "upload_mask_atlas" in EXPORT
    assert "rasterize_layer_f16" in EXPORT
    assert "first.padded_width.saturating_add(margin.saturating_mul(2))" in EXPORT
    assert "tile_mask_source_region(" in EXPORT
    assert "cropped_for_region(" in EXPORT
    assert "with_mask_uv_rect_and_extent(" in EXPORT
    assert "update_mask_layer_region" in EXPORT
    assert "MASK_ATLAS_EDGE_EXPORT_DESKTOP: u32 = 4096" in MASKS
    assert "MASK_ATLAS_EDGE_EXPORT_ANDROID: u32 = 2048" in MASKS


def test_local_adjustments_are_scene_linear_and_mask_weighted() -> None:
    assert "mask_adjust_0: array<vec4<f32>, 32>" in COMMON
    assert "mask_adjust_1: array<vec4<f32>, 32>" in COMMON
    assert "mask_adjust_2: array<vec4<f32>, 32>" in COMMON
    assert "fn local_adjustment_mix" not in ADJUSTMENTS
    assert "fn apply_local_exposure_nodes" in ADJUSTMENTS
    assert "fn apply_local_scene_tone_nodes" in ADJUSTMENTS
    assert "fn apply_local_scene_effect_nodes" in ADJUSTMENTS
    assert "fn apply_local_color_mixer" in ADJUSTMENTS
    assert "apply_local_basic_tone_values" in ADJUSTMENTS
    assert "apply_temperature_tint_values" in ADJUSTMENTS
    assert "apply_texture_and_clarity_values" in ADJUSTMENTS
    assert "apply_dehaze_value" in ADJUSTMENTS
    assert "apply_saturation_value" in ADJUSTMENTS


def test_ai_and_range_masks_use_a_stable_unedited_raw_reference() -> None:
    capture = APP[
        APP.index('#[cfg(target_os = "android")]\n    fn capture_mask_source_from_active_preview'):
        APP.index("pub(crate) fn request_subject_mask")
    ]
    android = capture[
        capture.index("fn capture_mask_source_from_active_preview"):
        capture.index("pub(crate) fn capture_mask_source")
    ]
    assert "preview_raw" in android
    assert "gpu_pipeline" in android
    assert "ExposureParams::scene_referred_default()" in android
    assert "MaskStack::default()" in android
    assert "pipeline.recompute" in android
    assert "pipeline.read_output_region_blocking" in android
    assert "restore_params" in android
    assert "target_exposure" in android
    assert "RawGpuPipeline::new_" not in android

    desktop = capture[capture.index('#[cfg(not(target_os = "android"))]'):]
    assert "loaded_raw" in desktop
    assert "const AI_MASK_SOURCE_MAX_EDGE: u32 = 4096;" in APP
    assert "const AI_MASK_SOURCE_MAX_PIXELS: u64 = 12_000_000;" in APP
    assert "max_edge: source_edge" in desktop
    assert "RawGpuPipeline::new_headless_reusing_program_template" in desktop
    assert "let rgba = reference_pipeline" in desktop


def test_brush_input_and_rasterization_avoid_progressive_slowdown() -> None:
    assert "if first_dab" in PREVIEW
    assert "distance_px >= spacing_px * 0.80" in PREVIEW
    assert "if changed" in PREVIEW and "last emitted point" in PREVIEW
    assert "use rayon::prelude::*" in MASKS
    assert "par_chunks_mut" in MASKS
    assert "ROW_BAND_HEIGHT" in MASKS
    assert "into_par_iter" in MASKS
    assert "take ownership" in MASKS


def test_ai_subject_feather_is_resolution_relative_and_background_is_exact_complement() -> None:
    assert "fn chamfer_distance" in MASKS
    assert "let edge = width.min(height) as f32" in MASKS
    assert "edge * 0.045" in MASKS
    assert "distance_to_outside[index] - distance_to_inside[index]" in MASKS
    assert "shape_probability_mask(&mut coverage" in MASKS
    assert "component.kind == MaskKind::Background" in MASKS
    assert "*value = 1.0 - *value" in MASKS
    assert "fixed 32-texel box blur" not in MASKS
    assert 'Zero feather means "use' in MASKS
    assert "*value = value.clamp(0.0, 1.0)" in MASKS
    assert "centered on the original 0.5 contour" in MASKS


def test_zoomed_overlays_and_adjustment_atlas_are_viewport_local() -> None:
    processing = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
    assert "fn overlay_raster_region(" in PREVIEW
    assert "cropped_for_region(" in PREVIEW
    assert "physical_pixels_per_point(ui.ctx())" in PREVIEW
    assert "source_overlay_texture_dimensions" not in PREVIEW
    assert "fn detail_mask_source_region(" in processing
    assert "upload_detail_masks(" in processing
    assert "with_mask_uv_rect_and_extent(" in processing
    assert "update_mask_layer_region" in processing
    assert "params.mask_counts.w == 0u" in ADJUSTMENTS
    assert "fn local_mask_texture_uv(" in ADJUSTMENTS


def test_ai_feather_sliders_reset_to_creation_default() -> None:
    assert "adjustment_slider_with_reset" in SIDEBAR
    for label in ('"Feather"', '"Mask feather"'):
        assert label in SIDEBAR
    assert SIDEBAR.count("adjustment_slider_with_reset(") >= 8


def test_ai_mask_resampling_is_bilinear_before_feathering() -> None:
    assert "let top = sample(x0, y0)" in MASKS
    assert "let bottom = sample(x0, y1)" in MASKS
    assert "*value = top + (bottom - top) * fy" in MASKS


    ai = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
    for category in ("Sky", "Vegetation", "Architecture", "Ground", "Water", "Mountains"):
        assert f"Self::{category}" in MASKS
    assert "ade20k_class_ids" in MASKS
    assert 'ui.button("Generate Mask")' in SIDEBAR
    assert "ADE20K_CLASS_COUNT: usize = 150" in ai
    assert "class_queries_logits" in ai
    assert "masks_queries_logits" in ai
    assert "let mut semantic_scores = [0.0f32; ADE20K_CLASS_COUNT]" in ai
    assert "let competition = best_selected / total" in ai
    assert "vitmatte_path" in ai
    assert "resize_probability_u8" in ai




def test_ai_subject_preserves_birefnet_soft_output_without_vitmatte_drift() -> None:
    ai = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
    subject = ai[ai.index("fn infer_subject(") : ai.index("fn run_subject_session")]
    assert "refine_mask_with_vitmatte(" not in subject
    assert "let mask = restore_from_letterbox(" in subject
    assert "Preserve it" in subject

def test_mask_brushes_stay_on_image_but_geometry_handles_can_leave_preview() -> None:
    assert "let mut interaction_rect = if app.sidebar_tab == SidebarTab::Masks" in PREVIEW
    assert "let geometry_can_leave_image = matches!(kind, MaskKind::Radial | MaskKind::Linear)" in PREVIEW
    assert "let pointer_bounds = if geometry_can_leave_image" in PREVIEW
    assert "screen_to_normalized_unclamped(image_rect, pointer)" in PREVIEW
    assert "Self::paint_mask_overlay(" in PREVIEW
    assert "let painter = ui.painter_at(overlay_rect);" in PREVIEW
    # Raster coverage stays clipped to the image while brush cursors stay inside the preview.
    assert "paint_textured_geometry_quad(ui, texture_id, image_rect" in PREVIEW
    assert "ui.painter_at(clip_rect).add(Shape::mesh(mesh));" in PREVIEW
    assert ".filter(|position| preview_rect.contains(*position))" in PREVIEW


def test_object_prompt_overlay_stays_visible_over_adjusted_masks_while_drawing() -> None:
    overlay = PREVIEW[PREVIEW.index("fn paint_mask_overlay"):PREVIEW.index("fn paint_coverage_texture")]
    assert "component.kind == MaskKind::Object" in overlay
    assert "painted prompt is exactly what the AI model will see" in overlay
    assert "selected_component.map(Some)" in overlay


def test_object_prompt_brush_is_hard_edged_without_a_feather_setting() -> None:
    object_variant = MASKS[MASKS.index("    Object {"):MASKS.index("    LuminanceRange {", MASKS.index("    Object {"))]
    assert "brush_feather" not in object_variant
    assert "fn object_prompt_dabs(strokes: &[ObjectStroke], size: f32)" in MASKS
    prompt_dabs = MASKS[MASKS.index("fn object_prompt_dabs"):MASKS.index("#[derive(Clone, Copy)]", MASKS.index("fn object_prompt_dabs"))]
    assert "feather: 0.0" in prompt_dabs
    assert '"Enhanced fine edges"' not in SIDEBAR


def test_object_masks_always_run_vitmatte_fine_edge_refinement() -> None:
    ai = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
    app = APP
    spawn = ai[ai.index("pub fn spawn_object_mask"):ai.index("fn infer_object_mask", ai.index("pub fn spawn_object_mask"))]
    assert "ensure_vitmatte_model(&vitmatte_path" in spawn
    assert "request.detailed_edges" not in ai
    infer = ai[ai.index("fn infer_object_mask"):ai.index("fn edge_aware_refine")]
    assert "full_mask = match refine_mask_with_vitmatte(" in infer
    assert "ViTMatte object-edge refinement failed; using the cleaned SAM mask" in infer
    assert "if request.detailed_edges" not in infer
    request = app[app.index("pub(crate) fn request_object_mask"):app.index("fn start_object_worker")]
    assert "encoder.is_file()" in request
    assert "decoder.is_file()" in request
    assert "self.vitmatte_model_path().is_file()" in request
    assert "crate::ai_masks::object_models_are_verified(" not in request
    assert "verify_vitmatte_model(vitmatte).is_ok()" in verified


def test_all_canvas_brushes_keep_constant_screen_size_across_zoom() -> None:
    assert "fn zoom_scaled_brush_size(tool_size: f32, preview_zoom: f32) -> f32" in PREVIEW
    assert "tool_size.max(0.0) / preview_zoom.max(MIN_PREVIEW_ZOOM)" in PREVIEW

    # Local adjustment brush dabs capture the zoom-adjusted image-space radius.
    assert "let dab_size = zoom_scaled_brush_size(*size, app.preview_zoom);" in PREVIEW
    assert "size: dab_size" in PREVIEW

    # Object prompt strokes capture their zoom-adjusted radius so delayed SAM
    # inference/recalculation does not depend on whatever zoom happens to be active later.
    assert "let stroke_brush_size = zoom_scaled_brush_size(*brush_size, app.preview_zoom);" in PREVIEW
    assert "brush_size: stroke_brush_size" in PREVIEW
    assert "let captured_size = if stroke.brush_size > 0.0" in MASKS


def test_inpainting_brush_uses_the_same_zoom_scaled_screen_space_radius() -> None:
    assert "let dab_size = zoom_scaled_brush_size(app.inpaint_brush_size, app.preview_zoom);" in PREVIEW
    assert "zoom_scaled_brush_size(app.inpaint_brush_size, app.preview_zoom)" in PREVIEW
    assert "dab.size.clamp(f32::EPSILON, 0.5)" in (ROOT / "src/inpainting.rs").read_text(encoding="utf-8")


def test_preview_can_zoom_out_below_fit_without_changing_fit_baseline() -> None:
    assert "const MIN_PREVIEW_ZOOM: f32 = 0.70;" in PREVIEW
    assert "app.preview_zoom = app.preview_zoom.clamp(MIN_PREVIEW_ZOOM, MAX_PREVIEW_ZOOM);" in PREVIEW
    assert "(previous_zoom * zoom_factor).clamp(MIN_PREVIEW_ZOOM, MAX_PREVIEW_ZOOM)" in PREVIEW
    assert "app.preview_zoom = 1.0;" in PREVIEW


def test_radial_and_linear_move_rotate_are_not_clamped_to_image_bounds() -> None:
    assert "center[0] = original_center[0] + uv[0] - origin[0];" in PREVIEW
    assert "center[1] = original_center[1] + uv[1] - origin[1];" in PREVIEW
    assert "let dx = uv[0] - origin[0];" in PREVIEW
    assert "let dy = uv[1] - origin[1];" in PREVIEW
    assert "*start = screen_to_normalized_unclamped(image_rect, midpoint - half_vector);" in PREVIEW
    assert "*end = screen_to_normalized_unclamped(image_rect, midpoint + half_vector);" in PREVIEW


def test_object_mask_postprocessing_prevents_speckled_interior_holes() -> None:
    ai = (ROOT / "src/ai_masks.rs").read_text(encoding="utf-8")
    keep = ai[ai.index("fn keep_prompt_connected_component"):ai.index("fn nearest_foreground")]
    assert "fill_enclosed_component_holes" in keep
    assert "probability.max(0.82)" in keep
    assert "1.0" in keep
    trimap = ai[ai.index("fn build_vitmatte_trimap"):ai.index("fn padded_to_divisor")]
    assert "(96..=160).contains(&value)" in trimap
    assert "(8..=247).contains(&value)" not in trimap


def test_local_mask_capacity_is_32_end_to_end() -> None:
    assert "pub const MAX_LOCAL_MASKS: usize = 32;" in MASKS
    assert "min(params.mask_counts.x, 32u)" in ADJUSTMENTS
    assert "mask_meta: array<vec4<u32>, 32>" in COMMON
    assert "mask_adjust_0: array<vec4<f32>, 32>" in COMMON
    assert "masks.masks.len().min(MAX_LOCAL_MASKS)" in EXPORT

    # Basic tone/effects must iterate the actual mask count too. A leftover
    # eight-mask loop here makes mask geometry visible for masks 9+ while
    # silently dropping exposure/contrast/WB/presence adjustments.
    for function_name in (
        "apply_local_exposure_nodes",
        "apply_local_scene_tone_nodes",
        "apply_local_scene_effect_nodes",
        "apply_local_color_mixer",
        "apply_local_color_grading",
    ):
        start = ADJUSTMENTS.index(f"fn {function_name}")
        end = ADJUSTMENTS.find("\nfn ", start + 4)
        body = ADJUSTMENTS[start:] if end < 0 else ADJUSTMENTS[start:end]
        assert "for (var index = 0u; index < count; index = index + 1u)" in body
        assert "index < 8u" not in body


def test_mask_geometry_overlays_follow_native_space_through_final_warp() -> None:
    preview = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
    assert "linear_axis_geometry_screen_points" in preview
    assert "linear_isot_geometry_screen_points" in preview
    assert "linear_rotation_handle_geometry" in preview
    assert "distance_to_polyline" in preview
    assert "brush_outline_geometry_screen_points" in preview
    # Regression guard: transformed linear-mask feather boundaries must not be
    # rebuilt from a screen-space perpendicular to the endpoint chord.
    linear_block = preview[
        preview.index("MaskGeometry::Linear {", preview.index("fn paint_mask_overlay")) :
        preview.index("_ => {}", preview.index("MaskGeometry::Linear {", preview.index("fn paint_mask_overlay")))
    ]
    assert "linear_isot_geometry_screen_points" in linear_block
    assert "center - normal * span" not in linear_block


def test_lens_preview_mesh_adapts_to_source_resolution() -> None:
    preview = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
    mesh = preview[
        preview.index("fn paint_textured_combined_geometry_mesh") :
        preview.index("fn native_source_to_corrected_uv")
    ]
    assert "grid_x" in mesh and "grid_y" in mesh
    assert "/ 96.0" in mesh
    assert "const GRID: usize = 32" not in mesh



def test_inpaint_focus_bounds_follow_warped_brush_footprint() -> None:
    preview = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
    fn = preview[preview.index("fn inpaint_stroke_geometry_screen_bounds(") :]
    fn = fn[: fn.index("\nfn screen_to_normalized_unclamped(")]
    assert "brush_outline_geometry_screen_points(" in fn
    assert "dab.center" in fn and "dab.size" in fn
    assert "dab.center[0] - du" not in fn
