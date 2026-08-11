// Helpers shared by non-destructive mask effects. These identifiers mirror the
// stable values assigned by MaskEffect::shader_id on the Rust side.

const MASK_EFFECT_NEON_ID: u32 = 1u;
const MASK_EFFECT_GLOW_ID: u32 = 2u;

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
