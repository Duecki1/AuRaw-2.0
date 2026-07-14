from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
SLIDER = (ROOT / "src/ui/components/adjustment_slider.rs").read_text(encoding="utf-8")
SIDEBAR = (ROOT / "src/ui/sidebar.rs").read_text(encoding="utf-8")
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
    assert 'id_salt("adjustment-section-tabs")' in SIDEBAR
    assert "if layout == ScreenLayout::Vertical" in SIDEBAR
    assert "match app.adjustment_section" in SIDEBAR


def test_vertical_masks_use_thumbnail_strip_for_groups_and_submasks() -> None:
    assert 'id_salt("vertical-mask-card-strip")' in SIDEBAR
    assert "mask_thumbnail_group_textures" in APP
    assert "mask_thumbnail_component_textures" in APP
    assert "rasterize_layer(" in SIDEBAR
    assert "rasterize_component_layer(" in SIDEBAR
    assert '"BASE"' in SIDEBAR
    assert 'MaskCombineMode::Add => "+"' in SIDEBAR
    assert 'MaskCombineMode::Subtract => "−"' in SIDEBAR
    assert 'MaskCombineMode::Intersect => "∩"' in SIDEBAR
    assert "selected_mask_before" in SIDEBAR


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


def test_mask_creation_buttons_are_compact_ascii_plus_buttons() -> None:
    assert SIDEBAR.count('egui::RichText::new("+")') >= 2
    assert 'ui.menu_button("＋"' not in SIDEBAR
    assert '"+\nCreate\nMask"' not in SIDEBAR
    assert '"+\nSub-mask"' not in SIDEBAR


def test_mask_creation_buttons_are_thin_on_the_strip_axis() -> None:
    assert "fn create_button_size" in SIDEBAR
    assert "MaskStripOrientation::Horizontal => egui::vec2(THIN_EDGE, card.y)" in SIDEBAR
    assert "MaskStripOrientation::Vertical => egui::vec2(card.x, THIN_EDGE)" in SIDEBAR
    assert "MaskCardSize::Group.create_button_size(orientation)" in SIDEBAR
    assert "MaskCardSize::Submask.create_button_size(orientation)" in SIDEBAR


def test_mask_preview_updates_are_throttled_and_committed_on_release() -> None:
    assert "UPDATE_EVERY_CHANGED_FRAMES: u8 = 10" in APP
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
    assert 'id_salt("mask-section-tabs")' in SIDEBAR
    assert '(MaskSection::Properties, "Mask Properties")' in SIDEBAR
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
    show_start = SIDEBAR.index("pub fn show(")
    fixed_tabs = SIDEBAR.index("Self::show_vertical_section_tabs(ui, app);", show_start)
    content_scroll = SIDEBAR.index('id_salt("develop-sidebar-content")', show_start)
    assert fixed_tabs < content_scroll

    vertical_tabs = SIDEBAR[
        SIDEBAR.index("fn show_vertical_section_tabs"):SIDEBAR.index("fn show_adjustment_tabs")
    ]
    assert "Self::show_adjustment_tabs(ui, app);" in vertical_tabs
    assert "SidebarTab::Masks => Self::show_mask_tabs(ui, app)" in vertical_tabs

    adjustment_body = SIDEBAR[
        SIDEBAR.index("fn show_adjustments"):SIDEBAR.index("fn show_vertical_section_tabs")
    ]
    mask_details = SIDEBAR[
        SIDEBAR.index("fn show_masks_vertical_details"):SIDEBAR.index(
            "fn show_vertical_mask_properties"
        )
    ]
    assert "show_adjustment_tabs" not in adjustment_body
    assert "show_mask_tabs" not in mask_details


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
    assert show.count("Self::show_vertical_section_tabs(ui, app);") == 1

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
    assert "show_mask_tabs" not in desktop_details
