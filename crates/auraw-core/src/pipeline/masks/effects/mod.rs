//! Mask effect catalog and persistent, non-destructive effect parameters.
//!
//! The catalog lives here because every effect participates in selection and
//! serialization. Actual implementations get their own module only when they
//! exist; placeholder modules are deliberately not created.

mod blur;
mod edge_glow;
mod fog;
mod glow;
mod lens_blur;
mod light_rays;
mod motion_blur;
mod neon;
mod pixelate;
mod radial_blur;
mod smoke;
mod tilt_shift;

pub use blur::BlurEffectSettings;
pub use edge_glow::EdgeGlowEffectSettings;
pub use fog::FogEffectSettings;
pub use glow::GlowEffectSettings;
pub use lens_blur::LensBlurEffectSettings;
pub use light_rays::LightRaysEffectSettings;
pub use motion_blur::MotionBlurEffectSettings;
pub use neon::NeonEffectSettings;
pub use pixelate::PixelateEffectSettings;
pub use radial_blur::{RadialBlurEffectSettings, RadialBlurMode};
pub use smoke::SmokeEffectSettings;
pub use tilt_shift::TiltShiftEffectSettings;

/// The operation driven by a mask group's combined coverage.
///
/// `MaskKind` describes how coverage is created (brush, gradient, subject,
/// and so on); `MaskEffect` describes what that coverage does to the image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum MaskEffect {
    #[default]
    Adjustment,
    Blur,
    LensBlur,
    MotionBlur,
    RadialBlur,
    TiltShift,
    BlackAndWhite,
    Colorize,
    Duotone,
    GradientMap,
    Invert,
    Sepia,
    Emboss,
    HighPass,
    Sharpen,
    Bulge,
    Glass,
    Kaleidoscope,
    Ripple,
    Twirl,
    Bloom,
    Glow,
    LightRays,
    Neon,
    Cartoon,
    EdgeGlow,
    Halftone,
    OilPaint,
    Outline,
    Pixelate,
    Posterize,
    FilmGrain,
    Fog,
    Noise,
    Smoke,
    TextureOverlay,
}

impl MaskEffect {
    pub const ALL: [Self; 36] = [
        Self::Adjustment,
        Self::Blur,
        Self::LensBlur,
        Self::MotionBlur,
        Self::RadialBlur,
        Self::TiltShift,
        Self::BlackAndWhite,
        Self::Colorize,
        Self::Duotone,
        Self::GradientMap,
        Self::Invert,
        Self::Sepia,
        Self::Emboss,
        Self::HighPass,
        Self::Sharpen,
        Self::Bulge,
        Self::Glass,
        Self::Kaleidoscope,
        Self::Ripple,
        Self::Twirl,
        Self::Bloom,
        Self::Glow,
        Self::LightRays,
        Self::Neon,
        Self::Cartoon,
        Self::EdgeGlow,
        Self::Halftone,
        Self::OilPaint,
        Self::Outline,
        Self::Pixelate,
        Self::Posterize,
        Self::FilmGrain,
        Self::Fog,
        Self::Noise,
        Self::Smoke,
        Self::TextureOverlay,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Adjustment => "Adjustment",
            Self::Blur => "Blur",
            Self::LensBlur => "Lens Blur",
            Self::MotionBlur => "Motion Blur",
            Self::RadialBlur => "Radial Blur",
            Self::TiltShift => "Tilt-Shift",
            Self::BlackAndWhite => "Black & White",
            Self::Colorize => "Colorize",
            Self::Duotone => "Duotone",
            Self::GradientMap => "Gradient Map",
            Self::Invert => "Invert",
            Self::Sepia => "Sepia",
            Self::Emboss => "Emboss",
            Self::HighPass => "High Pass",
            Self::Sharpen => "Sharpen",
            Self::Bulge => "Bulge",
            Self::Glass => "Glass",
            Self::Kaleidoscope => "Kaleidoscope",
            Self::Ripple => "Ripple",
            Self::Twirl => "Twirl",
            Self::Bloom => "Bloom",
            Self::Glow => "Glow",
            Self::LightRays => "Light Rays",
            Self::Neon => "Neon",
            Self::Cartoon => "Cartoon",
            Self::EdgeGlow => "Edge Glow",
            Self::Halftone => "Halftone",
            Self::OilPaint => "Oil Paint",
            Self::Outline => "Outline",
            Self::Pixelate => "Pixelate",
            Self::Posterize => "Posterize",
            Self::FilmGrain => "Film Grain",
            Self::Fog => "Fog",
            Self::Noise => "Noise",
            Self::Smoke => "Smoke",
            Self::TextureOverlay => "Texture Overlay",
        }
    }

    pub const fn category(self) -> Option<MaskEffectCategory> {
        match self {
            Self::Adjustment => None,
            Self::Blur | Self::LensBlur | Self::MotionBlur | Self::RadialBlur | Self::TiltShift => {
                Some(MaskEffectCategory::BlurAndFocus)
            }
            Self::BlackAndWhite
            | Self::Colorize
            | Self::Duotone
            | Self::GradientMap
            | Self::Invert
            | Self::Sepia => Some(MaskEffectCategory::Color),
            Self::Emboss | Self::HighPass | Self::Sharpen => Some(MaskEffectCategory::Detail),
            Self::Bulge | Self::Glass | Self::Kaleidoscope | Self::Ripple | Self::Twirl => {
                Some(MaskEffectCategory::Distort)
            }
            Self::Bloom | Self::Glow | Self::LightRays | Self::Neon => {
                Some(MaskEffectCategory::GlowAndLight)
            }
            Self::Cartoon
            | Self::EdgeGlow
            | Self::Halftone
            | Self::OilPaint
            | Self::Outline
            | Self::Pixelate
            | Self::Posterize => Some(MaskEffectCategory::Stylize),
            Self::FilmGrain | Self::Fog | Self::Noise | Self::Smoke | Self::TextureOverlay => {
                Some(MaskEffectCategory::Texture)
            }
        }
    }

    pub const fn is_implemented(self) -> bool {
        matches!(
            self,
            Self::Adjustment
                | Self::Blur
                | Self::LensBlur
                | Self::MotionBlur
                | Self::RadialBlur
                | Self::TiltShift
                | Self::Glow
                | Self::LightRays
                | Self::Neon
                | Self::EdgeGlow
                | Self::Pixelate
                | Self::Fog
                | Self::Smoke
        )
    }

    pub const fn uses_adjustments(self) -> bool {
        matches!(self, Self::Adjustment)
    }

    /// Stable identifier packed into the GPU mask record. Zero is reserved for
    /// the existing Adjustment path so older shader feature bits remain valid.
    pub const fn shader_id(self) -> u32 {
        match self {
            Self::Neon => 1,
            Self::Glow => 2,
            Self::LightRays => 3,
            Self::Blur => 4,
            Self::EdgeGlow => 5,
            Self::Pixelate => 6,
            Self::LensBlur => 7,
            Self::MotionBlur => 8,
            Self::RadialBlur => 9,
            Self::TiltShift => 10,
            Self::Fog => 11,
            Self::Smoke => 12,
            _ => 0,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskEffectCategory {
    BlurAndFocus,
    Color,
    Detail,
    Distort,
    GlowAndLight,
    Stylize,
    Texture,
}

impl MaskEffectCategory {
    /// Categories and their effects are intentionally alphabetized for the
    /// two-level picker.
    pub const ALL: [Self; 7] = [
        Self::BlurAndFocus,
        Self::Color,
        Self::Detail,
        Self::Distort,
        Self::GlowAndLight,
        Self::Stylize,
        Self::Texture,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::BlurAndFocus => "Blur & Focus",
            Self::Color => "Color",
            Self::Detail => "Detail",
            Self::Distort => "Distort",
            Self::GlowAndLight => "Glow & Light",
            Self::Stylize => "Stylize",
            Self::Texture => "Texture",
        }
    }
}

/// Settings for every implemented effect. Each field stays independent so a
/// type switch is reversible and never resets another effect's edit state.
#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskEffectSettings {
    #[serde(default, skip_serializing_if = "BlurEffectSettings::is_default")]
    pub blur: BlurEffectSettings,
    #[serde(default, skip_serializing_if = "LensBlurEffectSettings::is_default")]
    pub lens_blur: LensBlurEffectSettings,
    #[serde(default, skip_serializing_if = "MotionBlurEffectSettings::is_default")]
    pub motion_blur: MotionBlurEffectSettings,
    #[serde(default, skip_serializing_if = "RadialBlurEffectSettings::is_default")]
    pub radial_blur: RadialBlurEffectSettings,
    #[serde(default, skip_serializing_if = "TiltShiftEffectSettings::is_default")]
    pub tilt_shift: TiltShiftEffectSettings,
    #[serde(default, skip_serializing_if = "EdgeGlowEffectSettings::is_default")]
    pub edge_glow: EdgeGlowEffectSettings,
    #[serde(default, skip_serializing_if = "GlowEffectSettings::is_default")]
    pub glow: GlowEffectSettings,
    #[serde(default, skip_serializing_if = "LightRaysEffectSettings::is_default")]
    pub light_rays: LightRaysEffectSettings,
    #[serde(default, skip_serializing_if = "NeonEffectSettings::is_default")]
    pub neon: NeonEffectSettings,
    #[serde(default, skip_serializing_if = "PixelateEffectSettings::is_default")]
    pub pixelate: PixelateEffectSettings,
    #[serde(default, skip_serializing_if = "FogEffectSettings::is_default")]
    pub fog: FogEffectSettings,
    #[serde(default, skip_serializing_if = "SmokeEffectSettings::is_default")]
    pub smoke: SmokeEffectSettings,
}

impl MaskEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
