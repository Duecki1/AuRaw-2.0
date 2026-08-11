// Aperture-shaped, scene-linear gather for Lens Blur. The low-discrepancy
// disk pattern avoids directional spokes without requiring a temporary image.

const MASK_LENS_BLUR_SAMPLE_COUNT: u32 = 28u;
const MASK_LENS_BLUR_GOLDEN_ANGLE: f32 = 2.39996323;
const MASK_LENS_BLUR_PI: f32 = 3.14159265;

fn mask_lens_blur_at(
    pos: vec2<i32>,
    primary: vec4<f32>,
    secondary: vec4<f32>,
) -> vec3<f32> {
    let radius = f32(SceneAdjustments::presence_step(primary.y, 144));
    let blades = clamp(round(primary.z), 3.0, 12.0);
    let rotation = primary.w * MASK_LENS_BLUR_PI / 180.0;
    let sector = 2.0 * MASK_LENS_BLUR_PI / blades;
    let polygon_numerator = cos(MASK_LENS_BLUR_PI / blades);
    let highlight_boost = clamp(secondary.x / 100.0, 0.0, 1.0);
    var sum = vec3<f32>(0.0);
    var total_weight = 0.0;

    for (var index = 0u; index < MASK_LENS_BLUR_SAMPLE_COUNT; index = index + 1u) {
        let unit = (f32(index) + 0.5) / f32(MASK_LENS_BLUR_SAMPLE_COUNT);
        let sample_angle = f32(index) * MASK_LENS_BLUR_GOLDEN_ANGLE + rotation;
        let aperture_angle = (fract((sample_angle - rotation) / sector + 0.5) - 0.5) * sector;
        let polygon_radius = polygon_numerator / max(cos(aperture_angle), 0.25);
        let sample_radius = radius * sqrt(unit) * polygon_radius;
        let offset = vec2<f32>(cos(sample_angle), sin(sample_angle)) * sample_radius;
        let sample = SceneAdjustments::local_effects_at(pos + vec2<i32>(round(offset)));
        let bright = smoothstep(0.18, 1.25, Common::safe_luma(sample));
        let weight = 1.0 + highlight_boost * 3.0 * bright;
        sum = sum + sample * weight;
        total_weight = total_weight + weight;
    }
    return sum / max(total_weight, 1e-6);
}
