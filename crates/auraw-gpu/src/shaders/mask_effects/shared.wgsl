// Helpers shared by non-destructive mask effects. These identifiers mirror the
// stable values assigned by MaskEffect::shader_id on the Rust side.

const MASK_EFFECT_NEON_ID: u32 = 1u;
const MASK_EFFECT_GLOW_ID: u32 = 2u;
const MASK_EFFECT_LIGHT_RAYS_ID: u32 = 3u;
const MASK_EFFECT_BLUR_ID: u32 = 4u;
const MASK_EFFECT_EDGE_GLOW_ID: u32 = 5u;
const MASK_EFFECT_PIXELATE_ID: u32 = 6u;
const MASK_EFFECT_LENS_BLUR_ID: u32 = 7u;
const MASK_EFFECT_MOTION_BLUR_ID: u32 = 8u;
const MASK_EFFECT_RADIAL_BLUR_ID: u32 = 9u;
const MASK_EFFECT_TILT_SHIFT_ID: u32 = 10u;
const MASK_EFFECT_FOG_ID: u32 = 11u;
const MASK_EFFECT_SMOKE_ID: u32 = 12u;

fn mask_effect_srgb_component_to_linear(value: f32) -> f32 {
    let encoded = clamp(value, 0.0, 1.0);
    if encoded <= 0.04045 {
        return encoded / 12.92;
    }
    return pow((encoded + 0.055) / 1.055, 2.4);
}

fn mask_effect_picker_color_to_working(color: vec3<f32>) -> vec3<f32> {
    let linear_srgb = vec3<f32>(
        mask_effect_srgb_component_to_linear(color.r),
        mask_effect_srgb_component_to_linear(color.g),
        mask_effect_srgb_component_to_linear(color.b),
    );
    return max(Common::SRGB_TO_REC2020 * linear_srgb, vec3<f32>(0.0));
}
