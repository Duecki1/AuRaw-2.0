import unittest

from pathlib import Path

from tests.source_helpers import read_source_tree


class DcpProfileSelectionTests(unittest.TestCase):
    def test_settings_expose_three_profile_modes_and_recursive_root_picker(self) -> None:
        settings = read_source_tree(Path("src/ui/settings.rs"))
        self.assertIn('CameraProfileMode::Automatic', settings)
        self.assertIn('CameraProfileMode::DcpProfiles', settings)
        self.assertIn('CameraProfileMode::MatrixOnly', settings)
        self.assertIn('choose_camera_profile_folder', settings)
        self.assertIn('Camera profile folder', settings)
        self.assertIn('top-level profile root', settings)
        self.assertIn('searches every subfolder recursively', settings)

    def test_raw_loading_collects_all_camera_matches_and_supports_explicit_selection(self) -> None:
        lifecycle = read_source_tree(Path("src/app/lifecycle.rs"))
        loader = read_source_tree(Path("src/pipeline/raw_loader/libraw_loader.rs"))
        raw_loader = read_source_tree(Path("src/pipeline/raw_loader.rs"))
        self.assertIn('load_raw_file_with_profile_selection(', lifecycle)
        self.assertIn('find_matching_dcp_profiles(', loader)
        self.assertIn('available_camera_profiles', loader)
        self.assertIn('path_key.contains(model_key)', loader)
        self.assertIn('selected_profile: Option<&Path>', loader)
        self.assertIn('pub struct CameraProfileCandidate', raw_loader)
        self.assertIn('profile.camera_calibration_signature = raw_camera_signature', loader)
        self.assertIn('automatic fallback to camera matrix', loader)

    def test_develop_view_offers_dropdown_when_camera_has_multiple_profiles(self) -> None:
        sidebar = read_source_tree(Path("src/ui/sidebar/navigation.rs"))
        lifecycle = read_source_tree(Path("src/app/lifecycle.rs"))
        self.assertIn('candidates.len() == 1', sidebar)
        self.assertIn('current-image-camera-profile', sidebar)
        self.assertIn('Automatic (recommended)', sidebar)
        self.assertIn('select_camera_profile_for_current', sidebar)
        self.assertIn('select_camera_profile_for_current', lifecycle)
        self.assertIn('Some(selection)', lifecycle)

    def test_per_image_profile_choice_is_saved_relative_to_profile_root(self) -> None:
        sidecar = read_source_tree(Path("src/sidecar.rs"))
        persistence = read_source_tree(Path("src/app/sidecar_persistence.rs"))
        lifecycle = read_source_tree(Path("src/app/lifecycle.rs"))
        self.assertIn('pub camera_profile: Option<PathBuf>', sidecar)
        self.assertIn('selected.strip_prefix(root)', persistence)
        self.assertIn('root.join(relative)', lifecycle)
        self.assertIn('camera profile path must stay inside', sidecar)

    def test_dcp_parser_keeps_unique_camera_model(self) -> None:
        parser = read_source_tree(Path("src/pipeline/color_profile/dcp.rs"))
        profile = read_source_tree(Path("src/pipeline/color_profile.rs"))
        self.assertIn('const UNIQUE_CAMERA_MODEL: u16 = 50708;', parser)
        self.assertIn('camera_model = read_ascii_tag', parser)
        self.assertIn('pub camera_model: Option<String>', profile)


    def test_dcp_rendering_has_explicit_scene_look_and_view_boundaries(self) -> None:
        adjustments = read_source_tree(Path("src/shaders/adjustments.wgsl"))
        profile = read_source_tree(Path("src/shaders/profile.wgsl"))
        gpu = read_source_tree(Path("src/pipeline/gpu.rs"))
        lifecycle = read_source_tree(Path("src/app/lifecycle.rs"))

        scene_start = adjustments.index("fn prepare_scene_node")
        tone_start = adjustments.index("fn apply_scene_tone_node", scene_start)
        view_start = adjustments.index("fn apply_explicit_view_node", tone_start)
        scene = adjustments[scene_start:tone_start]
        view = adjustments[view_start:]

        self.assertLess(
            scene.index("apply_camera_characterization(scene_working_at(pos))"),
            scene.index("rgb = apply_exposure(rgb)"),
        )
        self.assertIn("if !uses_explicit_scene_display_domains()", scene)
        self.assertNotIn("apply_profile_view_tone", scene)
        self.assertIn("apply_optional_profile_look(scene_rgb)", view)
        self.assertIn("return apply_dcp_view_transform(view_input)", view)
        self.assertIn("return apply_sigmoid_view_transform(view_input)", view)
        self.assertIn("RENDER_GRAPH_EXPLICIT_SCENE_DISPLAY", gpu)
        self.assertIn("RenderStageContract", gpu)

        self.assertIn("mapped_low", profile)
        self.assertIn("mapped_high", profile)
        self.assertIn("(prophoto - vec3<f32>(low)) * scale", profile)
        self.assertIn("use_profile_base_tone", gpu)
        self.assertIn("profile_headroom", profile)
        self.assertIn("profile_linear / profile_headroom", profile)
        self.assertIn("apply_local_exposure_nodes(pos, rgb)", adjustments)
        self.assertIn("profile_tone_display_shoulder", adjustments)
        self.assertNotIn("exposure.exposure + super::GLOBAL_EXPOSURE_BACKEND_OFFSET_EV", gpu)
        self.assertIn("exposure: exposure.exposure", gpu)
        self.assertIn("profile.default_exposure_ev.to_bits()", read_source_tree(Path("src/pipeline/color_profile.rs")))
        self.assertNotIn("rendered_exposure.exposure = 0.0", lifecycle)

    def test_profile_preferences_are_persisted_and_cache_aware(self) -> None:
        persistence = read_source_tree(Path("src/performance_settings.rs"))
        lifecycle = read_source_tree(Path("src/app/lifecycle.rs"))
        self.assertIn('camera_profile_mode: CameraProfileMode', persistence)
        self.assertIn('camera_profile_folder: Option<PathBuf>', persistence)
        self.assertIn('last_camera_profile: Option<PathBuf>', persistence)
        self.assertIn('last_camera_profile: self.last_camera_profile.clone()', lifecycle)
        self.assertIn('Ok(None) => (', lifecycle)
        self.assertIn('last_camera_profile.as_ref().and_then', lifecycle)
        self.assertIn('requested_profile_from_sidecar', lifecycle)
        self.assertIn('Only an explicit user dropdown change', lifecycle)
        self.assertNotIn('keep using what I used on the last photo', lifecycle)
        self.assertIn('self.persist_performance_settings();', lifecycle)
        self.assertIn('|profile:{}|folder:{}|selection:{}', lifecycle)
        self.assertIn('cache_selection_is_known', lifecycle)


if __name__ == "__main__":
    unittest.main()
