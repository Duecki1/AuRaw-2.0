from pathlib import Path

from tests.source_helpers import read_source_tree

ROOT = Path(__file__).resolve().parents[1]
BASIC = (ROOT / "src/pipeline/basicadj.rs").read_text(encoding="utf-8")
GPU = read_source_tree(ROOT / "src/pipeline/gpu.rs")
PROFILE_RS = (ROOT / "src/pipeline/color_profile.rs").read_text(encoding="utf-8")
PROFILE_WGSL = (ROOT / "src/shaders/profile.wgsl").read_text(encoding="utf-8")
TONE = (ROOT / "src/shaders/tone_analysis.wgsl").read_text(encoding="utf-8")
ADJUSTMENTS = (ROOT / "src/shaders/adjustments.wgsl").read_text(encoding="utf-8")
SIDECAR = (ROOT / "src/sidecar.rs").read_text(encoding="utf-8")


def test_render_graph_declares_explicit_domain_contracts() -> None:
    assert "enum RenderDomain" in GPU
    for domain in (
        "CameraLinear",
        "SceneLinear",
        "LookAdjustedScene",
        "DisplayLinear",
        "OutputEncoded",
    ):
        assert domain in GPU
    for stage in (
        'name: "camera_characterization"',
        'name: "scene_edits"',
        'name: "optional_look"',
        'name: "view_transform"',
        'name: "output_encoding"',
    ):
        assert stage in GPU
    assert "explicit_render_graph_contracts_are_contiguous" in GPU


def test_dcp_payload_is_split_by_domain_role() -> None:
    assert "CameraCharacterizationGpuStage" in PROFILE_RS
    assert "OptionalLookGpuStage" in PROFILE_RS
    assert "ViewTransformGpuStage" in PROFILE_RS
    assert "OutputEncodingGpuStage" in PROFILE_RS
    assert "fn apply_camera_characterization" in PROFILE_WGSL
    assert "fn apply_optional_profile_look" in PROFILE_WGSL
    assert "fn apply_profile_view_tone" in PROFILE_WGSL


def test_scene_controls_precede_optional_look_and_view_transform() -> None:
    prepare_start = ADJUSTMENTS.index("fn prepare_scene_node")
    tone_start = ADJUSTMENTS.index("fn apply_scene_tone_node", prepare_start)
    effects_start = ADJUSTMENTS.index("fn apply_scene_effects_node", tone_start)
    explicit_view_start = ADJUSTMENTS.index("fn apply_explicit_view_node", effects_start)

    prepare = ADJUSTMENTS[prepare_start:tone_start]
    tone = ADJUSTMENTS[tone_start:effects_start]
    view = ADJUSTMENTS[explicit_view_start:]

    assert "apply_camera_characterization(scene_working_at(pos))" in prepare
    assert "rgb = apply_exposure(rgb)" in prepare
    assert "if !uses_explicit_scene_display_domains()" in prepare

    # The only profile view-tone call in the scene-tone node is compatibility-gated.
    legacy_gate = tone.index("if !uses_explicit_scene_display_domains()")
    profile_tone = tone.index("rgb = apply_profile_view_tone(rgb)", legacy_gate)
    adaptive_tone = tone.index("rgb = apply_lightroom_tone(rgb, pos)", profile_tone)
    local_tone = tone.index("rgb = apply_local_scene_tone_nodes(pos, rgb)", adaptive_tone)
    assert legacy_gate < profile_tone < adaptive_tone < local_tone

    look = view.index("apply_optional_profile_look(scene_rgb)")
    profile_view = view.index("apply_dcp_view_transform(view_input)", look)
    sigmoid_view = view.index("apply_sigmoid_view_transform(view_input)", look)
    assert look < profile_view
    assert look < sigmoid_view
    assert "apply_dcp_view_transform(view_input)" not in view[view.index("return apply_sigmoid_view_transform(view_input)") :]


def test_tone_analysis_is_profile_independent_on_new_process_path() -> None:
    explicit_branch = TONE.index("if uses_explicit_scene_display_domains()")
    legacy_look = TONE.index("apply_optional_profile_look(exposed)", explicit_branch)
    legacy_tone = TONE.index("apply_profile_view_tone(looked)", legacy_look)
    assert explicit_branch < legacy_look < legacy_tone
    assert "return map_negative_gamut(exposed);" in TONE[explicit_branch:legacy_look]


def test_process_version_preserves_legacy_appearance() -> None:
    assert "LEGACY_SCENE_DISPLAY_PROCESS_VERSION: u32 = 12" in BASIC
    assert "SCENE_DISPLAY_BOUNDARY_PROCESS_VERSION: u32 = 13" in BASIC
    assert "does *not* auto-upgrade process-12 edits" in BASIC
    assert "process_twelve_sidecars_keep_the_legacy_render_graph" in SIDECAR
    assert "assert!(!loaded.migrated);" in SIDECAR
