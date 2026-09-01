use super::{
    work_shader_source, SHADER_BASIC_ADJUSTMENTS, SHADER_COLOR, SHADER_COMMON,
    SHADER_CREATIVE_EFFECTS, SHADER_DETAIL_CAPTURE, SHADER_DETAIL_SCALE_SPACE,
    SHADER_MASK_ATMOSPHERE, SHADER_MASK_BLUR, SHADER_MASK_EDGE_GLOW, SHADER_MASK_EFFECTS_SHARED,
    SHADER_MASK_GLOW, SHADER_MASK_LENS_BLUR, SHADER_MASK_LIGHT_RAYS, SHADER_MASK_MOTION_BLUR,
    SHADER_MASK_NEON, SHADER_MASK_PIXELATE, SHADER_MASK_RADIAL_BLUR, SHADER_MASK_TILT_SHIFT,
    SHADER_NOISE, SHADER_NOISE_CA_FINISH, SHADER_PROFILE, SHADER_RAW_SAMPLING,
    SHADER_SCENE_ADJUSTMENTS, SHADER_TONEMAP, SHADER_TONE_COMMON,
};
use anyhow::{anyhow, Context, Result};
use naga_oil::compose::{
    ComposableModuleDescriptor, Composer, ComposerError, NagaModuleDescriptor, ShaderLanguage,
    ShaderType,
};
use std::borrow::Cow;

const SHADER_ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/src/shaders/");
const SCENE_ADJUSTMENTS_IMPORT: &str = "calibraw::scene_adjustments";

const SHADER_XTRANS_SEED: &str = include_str!("../../shaders/xtrans/seed.wgsl");
const SHADER_XTRANS_MARKESTEIJN_INTERPOLATE: &str =
    include_str!("../../shaders/xtrans/markesteijn_interpolate.wgsl");
const SHADER_XTRANS_MARKESTEIJN_REFINE: &str =
    include_str!("../../shaders/xtrans/markesteijn_refine.wgsl");
const SHADER_XTRANS_MARKESTEIJN_CANDIDATES: &str =
    include_str!("../../shaders/xtrans/markesteijn_candidates.wgsl");
const SHADER_XTRANS_MARKESTEIJN_DERIVATIVES: &str =
    include_str!("../../shaders/xtrans/markesteijn_derivatives.wgsl");
const SHADER_XTRANS_MARKESTEIJN_HOMOGENEITY: &str =
    include_str!("../../shaders/xtrans/markesteijn_homogeneity.wgsl");
const SHADER_XTRANS_MARKESTEIJN_ACCUMULATE: &str =
    include_str!("../../shaders/xtrans/markesteijn_accumulate.wgsl");

fn creative_effects_source() -> String {
    format!(
        "{SHADER_CREATIVE_EFFECTS}\n{SHADER_MASK_EFFECTS_SHARED}\n{SHADER_MASK_LENS_BLUR}\n{SHADER_MASK_MOTION_BLUR}\n{SHADER_MASK_RADIAL_BLUR}\n{SHADER_MASK_TILT_SHIFT}\n{SHADER_MASK_BLUR}\n{SHADER_MASK_EDGE_GLOW}\n{SHADER_MASK_GLOW}\n{SHADER_MASK_NEON}\n{SHADER_MASK_PIXELATE}\n{SHADER_MASK_LIGHT_RAYS}\n{SHADER_MASK_ATMOSPHERE}"
    )
}

pub(super) struct ShaderManager {
    composer: Composer,
}

impl ShaderManager {
    pub(super) fn new(work_format: wgpu::TextureFormat) -> Result<Self> {
        let mut manager = Self {
            composer: Composer::default(),
        };

        manager.register("calibraw::common", "common.wgsl", SHADER_COMMON)?;
        manager.register("calibraw::color", "color.wgsl", SHADER_COLOR)?;
        manager.register("calibraw::noise", "noise.wgsl", SHADER_NOISE)?;
        manager.register(
            "calibraw::raw_sampling",
            "raw_sampling.wgsl",
            SHADER_RAW_SAMPLING,
        )?;
        manager.register(
            "calibraw::xtrans::seed",
            "xtrans/seed.wgsl",
            SHADER_XTRANS_SEED,
        )?;
        manager.register(
            "calibraw::xtrans::markesteijn_interpolate",
            "xtrans/markesteijn_interpolate.wgsl",
            SHADER_XTRANS_MARKESTEIJN_INTERPOLATE,
        )?;
        manager.register(
            "calibraw::xtrans::markesteijn_refine",
            "xtrans/markesteijn_refine.wgsl",
            SHADER_XTRANS_MARKESTEIJN_REFINE,
        )?;
        manager.register(
            "calibraw::xtrans::markesteijn_candidates",
            "xtrans/markesteijn_candidates.wgsl",
            SHADER_XTRANS_MARKESTEIJN_CANDIDATES,
        )?;
        manager.register(
            "calibraw::xtrans::markesteijn_derivatives",
            "xtrans/markesteijn_derivatives.wgsl",
            SHADER_XTRANS_MARKESTEIJN_DERIVATIVES,
        )?;
        manager.register(
            "calibraw::xtrans::markesteijn_homogeneity",
            "xtrans/markesteijn_homogeneity.wgsl",
            SHADER_XTRANS_MARKESTEIJN_HOMOGENEITY,
        )?;
        manager.register(
            "calibraw::xtrans::markesteijn_accumulate",
            "xtrans/markesteijn_accumulate.wgsl",
            SHADER_XTRANS_MARKESTEIJN_ACCUMULATE,
        )?;
        manager.register("calibraw::profile", "profile.wgsl", SHADER_PROFILE)?;
        manager.register(
            "calibraw::basic_adjustments",
            "basic_adjustments.wgsl",
            SHADER_BASIC_ADJUSTMENTS,
        )?;
        manager.register(
            "calibraw::tone_common",
            "tone_common.wgsl",
            SHADER_TONE_COMMON,
        )?;
        manager.register("calibraw::tonemap", "tonemap.wgsl", SHADER_TONEMAP)?;
        manager.register(
            "calibraw::noise_ca_finish",
            "noise_ca_finish.wgsl",
            SHADER_NOISE_CA_FINISH,
        )?;
        manager.register(
            "calibraw::detail_capture",
            "detail_capture.wgsl",
            SHADER_DETAIL_CAPTURE,
        )?;

        let scene_adjustments = work_shader_source(SHADER_SCENE_ADJUSTMENTS, work_format)
            .context("specialize reusable scene-adjustments module")?;
        manager.register(
            SCENE_ADJUSTMENTS_IMPORT,
            "scene_adjustments.wgsl",
            scene_adjustments.as_ref(),
        )?;
        manager.register(
            "calibraw::detail_scale_space",
            "detail_scale_space.wgsl",
            SHADER_DETAIL_SCALE_SPACE,
        )?;
        let creative_effects = creative_effects_source();
        manager.register(
            "calibraw::creative_effects",
            "creative_effects.wgsl",
            &creative_effects,
        )?;
        Ok(manager)
    }

    fn register(&mut self, import_path: &str, file_name: &str, source: &str) -> Result<()> {
        let file_path = format!("{SHADER_ROOT}{file_name}");
        let result = self
            .composer
            .add_composable_module(ComposableModuleDescriptor {
                source,
                file_path: &file_path,
                language: ShaderLanguage::Wgsl,
                as_name: Some(import_path.to_owned()),
                ..Default::default()
            });
        match result {
            Ok(_) => Ok(()),
            Err(error) => Err(self.composer_error("register WGSL module", error)),
        }
    }

    pub(super) fn compose_naga_module(
        &mut self,
        source: &str,
        file_name: &str,
    ) -> Result<wgpu::naga::Module> {
        let file_path = format!("{SHADER_ROOT}{file_name}");
        let creative_effects;
        let source = if source == SHADER_CREATIVE_EFFECTS {
            creative_effects = creative_effects_source();
            creative_effects.as_str()
        } else {
            source
        };
        let result = self.composer.make_naga_module(NagaModuleDescriptor {
            source,
            file_path: &file_path,
            shader_type: ShaderType::Wgsl,
            shader_defs: Default::default(),
            additional_imports: &[],
        });
        match result {
            Ok(module) => Ok(module),
            Err(error) => Err(self.composer_error("compose WGSL entrypoint", error)),
        }
    }

    pub(super) fn create_shader_module(
        &mut self,
        device: &wgpu::Device,
        label: &'static str,
        source: &str,
        file_name: &str,
    ) -> Result<wgpu::ShaderModule> {
        let module = self
            .compose_naga_module(source, file_name)
            .with_context(|| format!("compose {label}"))?;
        Ok(device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Naga(Cow::Owned(module)),
        }))
    }

    fn composer_error(&self, operation: &str, error: ComposerError) -> anyhow::Error {
        anyhow!(
            "{operation} failed:\n{}",
            error.emit_to_string(&self.composer)
        )
    }
}
