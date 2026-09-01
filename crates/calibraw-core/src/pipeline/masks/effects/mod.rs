mod blur;
mod edge_glow;
mod fog;
mod glow;
mod lens_blur;
mod light_rays;
mod motion_blur;
mod neon;
pub mod params;
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

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum MaskEffect {
    #[default]
    Adjustment,
    Blur,
    LensBlur,
    MotionBlur,
    RadialBlur,
    TiltShift,
    Glow,
    LightRays,
    Neon,
    EdgeGlow,
    Pixelate,
    Fog,
    Smoke,
}

impl MaskEffect {
    pub const ALL: [Self; 13] = [
        Self::Adjustment,
        Self::Blur,
        Self::LensBlur,
        Self::MotionBlur,
        Self::RadialBlur,
        Self::TiltShift,
        Self::Glow,
        Self::LightRays,
        Self::Neon,
        Self::EdgeGlow,
        Self::Pixelate,
        Self::Fog,
        Self::Smoke,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Adjustment => "Adjustment",
            Self::Blur => "Blur",
            Self::LensBlur => "Lens Blur",
            Self::MotionBlur => "Motion Blur",
            Self::RadialBlur => "Radial Blur",
            Self::TiltShift => "Tilt-Shift",
            Self::Glow => "Glow",
            Self::LightRays => "Light Rays",
            Self::Neon => "Neon",
            Self::EdgeGlow => "Edge Glow",
            Self::Pixelate => "Pixelate",
            Self::Fog => "Fog",
            Self::Smoke => "Smoke",
        }
    }

    pub const fn category(self) -> Option<MaskEffectCategory> {
        match self {
            Self::Adjustment => None,
            Self::Blur | Self::LensBlur | Self::MotionBlur | Self::RadialBlur | Self::TiltShift => {
                Some(MaskEffectCategory::BlurAndFocus)
            }
            Self::Glow | Self::LightRays | Self::Neon => Some(MaskEffectCategory::GlowAndLight),
            Self::EdgeGlow | Self::Pixelate => Some(MaskEffectCategory::Stylize),
            Self::Fog | Self::Smoke => Some(MaskEffectCategory::Texture),
        }
    }

    pub const fn uses_adjustments(self) -> bool {
        matches!(self, Self::Adjustment)
    }

    pub const fn shader_id(self) -> u32 {
        match self {
            Self::Adjustment => 0,
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
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaskEffectCategory {
    BlurAndFocus,
    GlowAndLight,
    Stylize,
    Texture,
}

impl MaskEffectCategory {
    pub const ALL: [Self; 4] = [
        Self::BlurAndFocus,
        Self::GlowAndLight,
        Self::Stylize,
        Self::Texture,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::BlurAndFocus => "Blur & Focus",
            Self::GlowAndLight => "Glow & Light",
            Self::Stylize => "Stylize",
            Self::Texture => "Texture",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct MaskEffectSettings {
    #[serde(default, skip_serializing_if = "is_default")]
    pub blur: BlurEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub lens_blur: LensBlurEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub motion_blur: MotionBlurEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub radial_blur: RadialBlurEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub tilt_shift: TiltShiftEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub edge_glow: EdgeGlowEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub glow: GlowEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub light_rays: LightRaysEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub neon: NeonEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub pixelate: PixelateEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fog: FogEffectSettings,
    #[serde(default, skip_serializing_if = "is_default")]
    pub smoke: SmokeEffectSettings,
}

impl MaskEffectSettings {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }
}
