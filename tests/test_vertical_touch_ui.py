from tests.source_helpers import read_source_tree
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = read_source_tree(ROOT / "src/app.rs")
SLIDER = (ROOT / "src/ui/components/adjustment_slider.rs").read_text(encoding="utf-8")
SIDEBAR = read_source_tree(ROOT / "src/ui/sidebar.rs")
MASKS = (ROOT / "src/ui/sidebar/masks.rs").read_text(encoding="utf-8")
SETTINGS = (ROOT / "src/ui/settings.rs").read_text(encoding="utf-8")


def test_touch_sliders_are_direction_locked_and_scroll_area_friendly() -> None:
    assert "TRACK_DRAG_THRESHOLD" in SLIDER
    assert "HANDLE_DRAG_THRESHOLD" in SLIDER
    assert "delta.x.abs() >= delta.y.abs() * 1.15" in SLIDER
    assert 'ui.id().with("guarded-track"), Sense::click()' in SLIDER
    assert 'ui.id().with("guarded-handle"),\n        Sense::click()' in SLIDER
    assert "input.has_touch_screen()" in SLIDER
    assert "touch_value_field" in SLIDER
    assert "Slider::new" not in SLIDER


def test_android_sliders_use_compact_unboxed_photographic_styling() -> None:
    assert '#[cfg(target_os = "android")]\nconst VALUE_FIELD_WIDTH: f32 = 60.0;' in SLIDER
    assert '#[cfg(target_os = "android")]\nconst TRACK_HEIGHT: f32 = 2.0;' in SLIDER
    assert 'format!("{value:+.decimals$}")' in SLIDER
    android_track = SLIDER[
        SLIDER.index('#[cfg(target_os = "android")]\n    {', SLIDER.index("let painter = ui.painter();")):
        SLIDER.index('#[cfg(not(target_os = "android"))]', SLIDER.index("let painter = ui.painter();"))
    ]
    assert "weak_text_color().gamma_multiply(0.72)" in android_track
    assert "circle_filled(handle_center, HANDLE_RADIUS, ui.visuals().panel_fill)" in android_track
    assert "selection.bg_fill" not in android_track.split("circle_stroke", 1)[0]


def test_vertical_adjustments_use_second_level_tabs() -> None:
    assert "pub enum AdjustmentSection" in APP
    for name in (
        "Light",
        "ToneCurve",
        "Color",
        "ColorGrading",
        "Effects",
        "ColorMixer",
    ):
        assert f"AdjustmentSection::{name}" in SIDEBAR
    assert 'id_salt("develop-portrait-context-tabs")' in SIDEBAR
    assert "show_mobile_context_tabs" in SIDEBAR
    assert "if layout == ScreenLayout::Vertical" in SIDEBAR
    assert "match app.adjustment_section" in SIDEBAR


def test_vertical_masks_use_thumbnail_strip_for_groups_and_submasks() -> None:
    assert 'id_salt("vertical-mask-card-strip")' in SIDEBAR
    assert "mask_thumbnail_group_textures" in APP
    assert "mask_thumbnail_component_textures" in APP
    assert "rasterize_layer(" in SIDEBAR
    assert "rasterize_component_layer(" in SIDEBAR
    assert '"BASE"' in SIDEBAR
    assert "selected_mask_before" in SIDEBAR


def test_android_mask_strips_claim_touch_drags_instead_of_cards() -> None:
    assert "fn mask_strip_scroll_source()" in MASKS
    assert "egui::scroll_area::ScrollSource::ALL" in MASKS
    assert MASKS.count(".scroll_source(mask_strip_scroll_source())") == 2
    thumbnail = MASKS[
        MASKS.index("fn mask_thumbnail_card"):
        MASKS.index("fn prepare_content_mask")
    ]
    assert 'if cfg!(target_os = "android")' in thumbnail
    assert "egui::Sense::click()" in thumbnail
    assert "egui::Sense::click_and_drag()" in thumbnail


def test_settings_width_and_text_wrap_follow_screen_layout() -> None:
    assert "layout: ScreenLayout" in SETTINGS
    assert "ScreenLayout::Vertical => ui.available_width()" in SETTINGS
    assert "ui.set_max_width(content_width)" in SETTINGS
    assert "horizontal_wrapped" in SETTINGS
    assert SETTINGS.count(".wrap()") >= 5


def test_vertical_mask_strip_is_a_separate_panel_above_the_sidebar() -> None:
    sidebar_panel = APP.index('Panel::bottom("develop_sidebar_bottom")')
    sidebar_show = APP.index("Sidebar::show(ui, self, layout, frame)", sidebar_panel)
    strip_panel = APP.index('Panel::bottom("develop_vertical_mask_strip")', sidebar_show)
    strip_show = APP.index("Sidebar::show_vertical_mask_strip(ui, self, frame)", strip_panel)
    assert sidebar_panel < sidebar_show < strip_panel < strip_show
    assert "VERTICAL_MASK_STRIP_HEIGHT" in SIDEBAR
    assert "show_masks_vertical_details" in SIDEBAR


def test_selected_group_expands_smaller_submasks_immediately_after_it() -> None:
    parent_card = SIDEBAR.index("MaskCardSize::Group")
    selected_expansion = SIDEBAR.index("if selected_mask_before == Some(index)", parent_card)
    child_card = SIDEBAR.index("MaskCardSize::Submask", selected_expansion)
    assert parent_card < selected_expansion < child_card
    assert "Self::Submask => egui::vec2(56.0, 62.0)" in SIDEBAR
    assert "Self::Group => egui::vec2(68.0, 72.0)" in SIDEBAR


def test_mask_thumbnail_content_preserves_raw_aspect_inside_square_well() -> None:
    assert "thumbnail_fit_size" in SIDEBAR
    assert "let mut square = vec![0_u8; edge * edge]" in SIDEBAR
    assert "thumbnail_width" in SIDEBAR
    assert "thumbnail_height" in SIDEBAR
    assert "egui::vec2(image_edge, image_edge)" in SIDEBAR


def test_vertical_mask_creation_lives_inside_thumbnail_strip() -> None:
    strip = SIDEBAR[SIDEBAR.index("fn show_vertical_mask_strip"):]
    details = SIDEBAR[SIDEBAR.index("fn show_masks_vertical_details"):]
    assert "create_mask_group_card" in strip
    assert "create_submask_card" in strip
    assert strip.index("create_mask_group_card") < strip.index("MaskCardSize::Group")
    assert strip.index("create_submask_card") > strip.index("MaskCardSize::Submask")
    assert 'ui.menu_button("Create Mask"' not in details


def test_mask_cards_and_rows_have_context_menus_for_management() -> None:
    assert SIDEBAR.count(".context_menu(|ui|") >= 2
    assert "Rename mask group" in SIDEBAR
    assert "Rename sub-mask" in SIDEBAR
    assert 'checkbox(&mut mask.enabled, "Enabled")' in SIDEBAR
    assert 'checkbox(&mut component.enabled, "Enabled")' in SIDEBAR
    assert 'checkbox(&mut component.invert, "Invert")' in SIDEBAR
    assert "Delete mask group" in SIDEBAR
    assert "Delete sub-mask" in SIDEBAR


def test_mask_preview_updates_are_throttled_and_committed_on_release() -> None:
    assert "INTERACTIVE_MASK_INTERVAL: Duration = Duration::from_millis(45)" in APP
    assert "mask_interaction_last_upload" in APP
    assert "note_mask_geometry_interaction" in APP
    assert "finish_mask_geometry_interaction" in APP
    assert "app.note_mask_geometry_interaction(mask_index)" in (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")


def test_radial_and_linear_masks_have_rotation_handles() -> None:
    preview = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
    assert "RotateRadial" in APP
    assert "RotateLinear" in APP
    assert "radial_rotation_handle" in preview
    assert "linear_rotation_handle" in preview
    assert "shortest_angle_delta" in preview


def test_slider_drag_freezes_sidebar_and_settings_scrolling() -> None:
    assert "pub fn slider_scroll_locked" in SLIDER
    assert "slider_scroll_lock_owner" in SLIDER
    assert "ctx.set_dragged_id(slider_id)" in SLIDER
    assert "slider_drag_active" in SLIDER
    assert "ScrollSource::NONE" in SIDEBAR
    assert ".scroll_source(sidebar_scroll_source)" in SIDEBAR
    assert "ScrollSource::NONE" in APP
    assert ".scroll_source(settings_scroll_source)" in APP


def test_vertical_mask_details_use_adjustment_style_tabs() -> None:
    assert "pub enum MaskSection" in APP
    assert "Properties" in APP
    assert 'id_salt("develop-portrait-context-tabs")' in SIDEBAR
    assert '(MaskSection::Properties, regular::SELECTION, "Mask", 58.0)' in SIDEBAR
    for name in (
        "Light",
        "ToneCurve",
        "Color",
        "ColorGrading",
        "Effects",
        "ColorMixer",
    ):
        assert f"MaskSection::{name}" in SIDEBAR
    assert "match mask_section" in SIDEBAR
    assert "show_local_mask_adjustment_section" in SIDEBAR


def test_vertical_section_tabs_stay_above_scrolling_content() -> None:
    shell_start = SIDEBAR.index("fn show_vertical_mobile_shell")
    primary_tabs = SIDEBAR.index('Panel::bottom("develop_portrait_primary_tabs")', shell_start)
    context_tabs = SIDEBAR.index('Panel::bottom("develop_portrait_context_tabs")', shell_start)
    content_scroll = SIDEBAR.index('id_salt("develop-sidebar-content")', shell_start)
    assert primary_tabs < context_tabs < content_scroll

    context_body = SIDEBAR[
        SIDEBAR.index("fn show_mobile_context_tabs"):SIDEBAR.index("fn mobile_icon_tab")
    ]
    assert "SidebarTab::Adjustments =>" in context_body
    assert "SidebarTab::Masks =>" in context_body

    adjustment_body = SIDEBAR[
        SIDEBAR.index("fn show_adjustments"):SIDEBAR.index("fn show_camera_profile_selector")
    ]
    mask_details = SIDEBAR[
        SIDEBAR.index("fn show_masks_vertical_details"):SIDEBAR.index(
            "fn show_vertical_mask_properties"
        )
    ]
    assert "show_mobile_context_tabs" not in adjustment_body
    assert "show_mobile_context_tabs" not in mask_details


def test_mobile_navigation_has_horizontal_separators_without_side_borders() -> None:
    frame = SIDEBAR[
        SIDEBAR.index("fn mobile_navigation_frame"):SIDEBAR.index(
            "fn paint_mobile_navigation_separators"
        )
    ]
    separators = SIDEBAR[
        SIDEBAR.index("fn paint_mobile_navigation_separators"):SIDEBAR.index(
            "fn show_mobile_primary_tabs"
        )
    ]
    assert ".stroke(egui::Stroke::NONE)" in frame
    assert separators.count(".hline(") == 2
    assert ".vline(" not in separators


def test_horizontal_masks_use_the_portrait_editor_with_a_left_vertical_strip() -> None:
    sidebar_panel = APP.index('Panel::right("develop_sidebar_right")')
    sidebar_show = APP.index("Sidebar::show(ui, self, layout, frame)", sidebar_panel)
    strip_panel = APP.index('Panel::right("develop_horizontal_mask_strip")', sidebar_show)
    strip_show = APP.index("Sidebar::show_horizontal_mask_strip(ui, self, frame)", strip_panel)
    assert sidebar_panel < sidebar_show < strip_panel < strip_show
    assert "HORIZONTAL_MASK_STRIP_WIDTH" in SIDEBAR
    assert 'id_salt("horizontal-mask-card-strip")' in SIDEBAR
    assert "egui::ScrollArea::vertical()" in SIDEBAR[
        SIDEBAR.index("MaskStripOrientation::Vertical =>"):SIDEBAR.index(
            "if group_enabled_changed"
        )
    ]


def test_horizontal_mask_sidebar_hides_tabs_and_uses_collapsible_sections() -> None:
    show = SIDEBAR[SIDEBAR.index("pub fn show("):SIDEBAR.index("fn show_adjustments")]
    assert "else if app.sidebar_tab == SidebarTab::Masks" not in show
    assert show.count("Self::show_vertical_mobile_shell(ui, app, frame);") == 1

    mask_body = SIDEBAR[
        SIDEBAR.index("fn show_masks("):SIDEBAR.index("pub(crate) fn show_vertical_mask_strip")
    ]
    assert "ScreenLayout::Vertical => Self::show_masks_vertical_details" in mask_body
    assert "ScreenLayout::Horizontal => Self::show_masks_horizontal_details" in mask_body

    desktop_details = SIDEBAR[
        SIDEBAR.index("fn show_masks_horizontal_details"):SIDEBAR.index(
            "fn show_masks_vertical_details"
        )
    ]
    assert 'Self::adjustment_section(ui, "Mask Properties", true, true' in desktop_details
    assert 'ui.strong("Local Adjustments")' in desktop_details
    for label in ("Light", "Tone Curve", "Color", "Color Grading", "Effects", "Color Mixer"):
        assert f'"{label}"' in desktop_details
    assert "show_mobile_context_tabs" not in desktop_details


def test_preview_supports_touch_pinch_zoom_and_two_finger_pan() -> None:
    preview = (ROOT / "src/ui/preview.rs").read_text(encoding="utf-8")
    assert "input.multi_touch()" in preview
    assert "multi_touch.zoom_delta" in preview
    assert "multi_touch.translation_delta" in preview
    assert "multi_touch.center_pos" in preview
    assert "multi_touch.start_pos" in preview
    assert "previous_touch_center" in preview
    assert "transform_preview_about_screen_points" in preview
    assert "input.any_touches()" in preview
    assert "preview_touch_navigation_active" in preview
    assert "let touch_navigation = app.preview_touch_navigation_active" in preview
    assert "if !touch_navigation && !fit_gesture" in preview
    assert "pinch/scroll zoom" in preview
    assert "double-tap/click fit" in preview
