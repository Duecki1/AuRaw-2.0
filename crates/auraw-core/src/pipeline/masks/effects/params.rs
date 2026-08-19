//! Shared metadata for editable mask-effect parameters.
//!
//! These specs are intentionally small and static. They feed defaults, UI
//! controls, sidecar validation, and GPU safety clamps without introducing a
//! runtime parameter registry or changing serialization.

use std::ops::RangeInclusive;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FloatParamSpec {
    pub label: &'static str,
    pub min: f32,
    pub max: f32,
    pub default: f32,
    pub step: f64,
    pub decimals: usize,
    pub tooltip: Option<&'static str>,
}

impl FloatParamSpec {
    pub fn range(self) -> RangeInclusive<f32> {
        self.min..=self.max
    }

    pub fn clamp(self, value: f32) -> f32 {
        value.clamp(self.min, self.max)
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ColorParamSpec {
    pub label: &'static str,
    pub title: &'static str,
    pub default: [f32; 3],
    pub min: f32,
    pub max: f32,
    pub tooltip: &'static str,
}

impl ColorParamSpec {
    pub fn clamp(self, color: [f32; 3]) -> [f32; 3] {
        color.map(|channel| channel.clamp(self.min, self.max))
    }
}

macro_rules! float_param {
    (
        $name:ident,
        $label:literal,
        $min:expr,
        $max:expr,
        $default:expr,
        $step:expr,
        $decimals:expr,
        $tooltip:expr
        $(,)?
    ) => {
        pub const $name: FloatParamSpec = FloatParamSpec {
            label: $label,
            min: $min,
            max: $max,
            default: $default,
            step: $step,
            decimals: $decimals,
            tooltip: $tooltip,
        };
    };
}

macro_rules! color_param {
    ($name:ident, $label:literal, $title:literal, $default:expr, $tooltip:literal $(,)?) => {
        pub const $name: ColorParamSpec = ColorParamSpec {
            label: $label,
            title: $title,
            default: $default,
            min: 0.0,
            max: 1.0,
            tooltip: $tooltip,
        };
    };
}

pub mod adjustment {
    use super::*;
    use crate::pipeline::basicadj::HUE_ROTATION_LIMIT_DEGREES;

    float_param!(EXPOSURE, "Exposure", -5.0, 5.0, 0.0, 0.05, 2, None);
    float_param!(CONTRAST, "Contrast", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(HIGHLIGHTS, "Highlights", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(SHADOWS, "Shadows", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(WHITES, "Whites", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(BLACKS, "Blacks", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(TEMPERATURE, "Temperature", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(TINT, "Tint", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(
        HUE, "Hue", -HUE_ROTATION_LIMIT_DEGREES, HUE_ROTATION_LIMIT_DEGREES, 0.0, 1.0, 1,
        Some("Rotates colors inside the mask around the perceptual color wheel."),
    );
    float_param!(SATURATION, "Saturation", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(TEXTURE, "Texture", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(CLARITY, "Clarity", -100.0, 100.0, 0.0, 1.0, 0, None);
    float_param!(DEHAZE, "Dehaze", -100.0, 100.0, 0.0, 1.0, 0, None);
}

pub mod blur {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Blends the blurred result into the developed image."),
    );
    float_param!(
        RADIUS, "Radius", 0.0, 16.0, 8.0, 0.1, 1,
        Some("Controls the scale-aware blur radius."),
    );
}

pub mod lens_blur {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Blends the lens-blurred result into the developed image."),
    );
    float_param!(
        RADIUS, "Radius", 0.0, 48.0, 12.0, 0.1, 1,
        Some("Controls the aperture radius in reference-image pixels."),
    );
    float_param!(
        BLADES, "Blades", 3.0, 12.0, 6.0, 1.0, 0,
        Some("Sets the number of sides in the simulated aperture."),
    );
    float_param!(
        ROTATION, "Rotation", -180.0, 180.0, 0.0, 1.0, 0,
        Some("Rotates the simulated aperture."),
    );
    float_param!(
        HIGHLIGHTS, "Highlights", 0.0, 100.0, 0.0, 0.5, 0,
        Some("Gives bright samples more weight so bokeh highlights stand out."),
    );
}

pub mod motion_blur {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Blends the directional blur into the developed image."),
    );
    float_param!(
        DISTANCE, "Distance", 0.0, 96.0, 32.0, 0.1, 1,
        Some("Controls the total shutter trail in reference-image pixels."),
    );
    float_param!(ANGLE, "Angle", -180.0, 180.0, 0.0, 1.0, 0, Some("Sets the direction of motion."));
}

pub mod radial_blur {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Blends the radial trail into the developed image."),
    );
    float_param!(
        STRENGTH, "Strength", 0.0, 96.0, 36.0, 0.1, 1,
        Some("Sets the maximum trail length in reference-image pixels."),
    );
    float_param!(
        CENTER_X, "Center X", -50.0, 150.0, 50.0, 1.0, 0,
        Some("Horizontal origin in the full image; values may extend beyond the frame."),
    );
    float_param!(
        CENTER_Y, "Center Y", -50.0, 150.0, 50.0, 1.0, 0,
        Some("Vertical origin in the full image; values may extend beyond the frame."),
    );
}

pub mod tilt_shift {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 75.0, 0.5, 0,
        Some("Controls the maximum defocus strength outside the focus band."),
    );
    float_param!(
        RADIUS, "Radius", 0.0, 48.0, 16.0, 0.1, 1,
        Some("Controls the defocus radius in reference-image pixels."),
    );
    float_param!(
        CENTER_X, "Center X", -50.0, 150.0, 50.0, 1.0, 0,
        Some("Horizontal position of a point on the sharp band."),
    );
    float_param!(
        CENTER_Y, "Center Y", -50.0, 150.0, 50.0, 1.0, 0,
        Some("Vertical position of a point on the sharp band."),
    );
    float_param!(ANGLE, "Angle", -180.0, 180.0, 0.0, 1.0, 0, Some("Rotates the in-focus band."));
    float_param!(
        FOCUS_WIDTH, "Focus Width", 0.0, 100.0, 24.0, 0.5, 0,
        Some("Width of the sharp band as a percentage of the image's shorter edge."),
    );
    float_param!(
        FEATHER, "Feather", 0.1, 100.0, 18.0, 0.1, 1,
        Some("Softens the transition from sharp to defocused areas."),
    );
}

pub mod edge_glow {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Controls the strength of the emitted edge light."),
    );
    float_param!(
        EDGE_WIDTH, "Edge Width", 0.5, 8.0, 1.5, 0.05, 1,
        Some("Sets the scale used to detect and widen edges."),
    );
    float_param!(
        DETAIL, "Detail", 0.0, 100.0, 35.0, 0.5, 0,
        Some("Higher values include finer, lower-contrast edges."),
    );
    float_param!(
        GLOW, "Glow", 0.0, 100.0, 55.0, 0.5, 0,
        Some("Adds a broader halo around the detected edges."),
    );
    color_param!(
        COLOR, "Color", "Edge Glow color", [1.0, 0.42, 0.08],
        "Choose the color emitted by the Edge Glow effect.",
    );
}

pub mod glow {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Controls the strength of the bright core and emitted halo."),
    );
    float_param!(
        RADIUS, "Radius", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Controls how far the glow spreads beyond the mask."),
    );
    float_param!(
        CORE, "Core", 0.0, 100.0, 65.0, 0.5, 0,
        Some("Makes the masked source brighter and more white-hot."),
    );
    color_param!(
        COLOR, "Color", "Glow color", [0.1, 0.65, 1.0],
        "Choose the color emitted by the Glow effect.",
    );
}

pub mod light_rays {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Controls the strength of the emitted light shafts."),
    );
    float_param!(
        LENGTH, "Length", 0.0, 200.0, 100.0, 1.0, 0,
        Some("Ray reach as a percentage of the image's shorter edge."),
    );
    float_param!(
        SOURCE_X, "Source X", -50.0, 150.0, 50.0, 1.0, 0,
        Some("Horizontal source position in the full image; values outside 0–100 place it beyond the frame."),
    );
    float_param!(
        SOURCE_Y, "Source Y", -50.0, 150.0, 35.0, 1.0, 0,
        Some("Vertical source position in the full image; values outside 0–100 place it beyond the frame."),
    );
    float_param!(
        SPREAD, "Spread", 0.0, 45.0, 10.0, 0.25, 1,
        Some("Widens the cone sampled around each radial shaft."),
    );
    float_param!(
        FADE, "Fade", 0.0, 100.0, 45.0, 0.5, 0,
        Some("Controls how quickly ray intensity falls off with distance."),
    );
    float_param!(
        RAY_COUNT, "Ray Count", 4.0, 96.0, 32.0, 1.0, 0,
        Some("Controls the approximate number of broad shafts around the source."),
    );
    float_param!(
        VARIATION, "Variation", 0.0, 100.0, 55.0, 0.5, 0,
        Some("Breaks uniform emission into stronger and weaker god rays."),
    );
    float_param!(
        SOFTNESS, "Softness", 0.0, 100.0, 40.0, 0.5, 0,
        Some("Softens shaft edges and blends neighbouring source directions."),
    );
    color_param!(
        COLOR, "Color", "Light Rays color", [1.0, 0.85, 0.62],
        "Choose the color emitted by the Light Rays effect.",
    );
}

pub mod neon {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Controls the strength of the emitted neon lines."),
    );
    float_param!(
        EDGE_WIDTH, "Edge Width", 0.5, 8.0, 1.0, 0.05, 1,
        Some("Sets the scale used to find and widen image edges."),
    );
    float_param!(
        DETAIL, "Detail", 0.0, 100.0, 10.0, 0.5, 0,
        Some("Higher values include finer, lower-contrast edges."),
    );
    float_param!(
        GLOW, "Glow", 0.0, 100.0, 10.0, 0.5, 0,
        Some("Adds a broader halo around the detected edge lines."),
    );
    float_param!(
        BACKGROUND, "Background", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Retains the original image behind the Neon effect."),
    );
    color_param!(COLOR, "Color", "Neon color", [0.05, 0.85, 1.0], "Choose the emitted Neon color.");
}

pub mod pixelate {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 100.0, 0.5, 0,
        Some("Blends the pixelated result into the developed image."),
    );
    float_param!(
        BLOCK_SIZE, "Block Size", 2.0, 32.0, 16.0, 1.0, 0,
        Some("Controls the scale-aware size of each square pixel block."),
    );
}

pub mod fog {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Controls the overall strength of the atmospheric veil."),
    );
    float_param!(
        DENSITY, "Density", 0.0, 100.0, 55.0, 0.5, 0,
        Some("Controls how opaque the fog becomes."),
    );
    float_param!(
        SCALE, "Scale", 1.0, 100.0, 65.0, 0.5, 0,
        Some("Higher values create broader fog banks."),
    );
    float_param!(
        SOFTNESS, "Softness", 0.0, 100.0, 70.0, 0.5, 0,
        Some("Softens transitions between clear and foggy areas."),
    );
    float_param!(
        VARIATION, "Variation", 0.0, 100.0, 45.0, 0.5, 0,
        Some("Varies the fog density across the image."),
    );
    float_param!(
        SEED, "Seed", 0.0, 1_000.0, 0.0, 1.0, 0,
        Some("Chooses another deterministic fog pattern."),
    );
    color_param!(
        COLOR, "Color", "Fog color", [0.82, 0.87, 0.92],
        "Choose the color of the atmospheric veil.",
    );
}

pub mod smoke {
    use super::*;

    float_param!(
        AMOUNT, "Amount", 0.0, 100.0, 50.0, 0.5, 0,
        Some("Controls the overall strength of the smoke overlay."),
    );
    float_param!(
        DENSITY, "Density", 0.0, 100.0, 60.0, 0.5, 0,
        Some("Controls the opacity and body of the plumes."),
    );
    float_param!(
        SCALE, "Scale", 1.0, 100.0, 55.0, 0.5, 0,
        Some("Higher values create larger smoke plumes."),
    );
    float_param!(
        TURBULENCE, "Turbulence", 0.0, 100.0, 65.0, 0.5, 0,
        Some("Adds curls and distortion to the smoke."),
    );
    float_param!(
        SOFTNESS, "Softness", 0.0, 100.0, 55.0, 0.5, 0,
        Some("Softens the boundaries of individual plumes."),
    );
    float_param!(
        ANGLE, "Angle", -180.0, 180.0, -12.0, 1.0, 0,
        Some("Rotates the direction of the smoke flow."),
    );
    float_param!(
        SEED, "Seed", 0.0, 1_000.0, 0.0, 1.0, 0,
        Some("Chooses another deterministic smoke pattern."),
    );
    color_param!(
        COLOR, "Color", "Smoke color", [0.32, 0.34, 0.37],
        "Choose the color of the smoke plumes.",
    );
}
