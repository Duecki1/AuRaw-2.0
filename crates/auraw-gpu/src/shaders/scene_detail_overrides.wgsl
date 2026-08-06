#import auraw::scene_adjustments as SceneAdjustments
#import auraw::detail_capture as DetailCapture

// naga_oil tree-shakes ordinary imports. This module is therefore supplied as
// an additional import whenever scene_adjustments is consumed as a reusable
// module, retaining the exact capture-sharpening callbacks from the top-level
// scene shader without changing their calculations.
override fn DetailCapture::adjustment_base_at(pos: vec2<i32>) -> vec3<f32> {
    return SceneAdjustments::adjustment_base_at(pos);
}

override fn DetailCapture::log_luminance(rgb: vec3<f32>) -> f32 {
    return SceneAdjustments::log_luminance(rgb);
}

override fn DetailCapture::presence_reference_scale() -> f32 {
    return SceneAdjustments::presence_reference_scale();
}

override fn DetailCapture::soft_detail_threshold(detail: f32, threshold: f32) -> f32 {
    return SceneAdjustments::soft_detail_threshold(detail, threshold);
}
