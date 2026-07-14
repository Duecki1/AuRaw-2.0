use crate::app::{
    AdjustmentSection, AurawApp, ColorGradeTab, MaskSection, SidebarTab, ToneCurveTab,
};
use crate::pipeline::{
    BrushMode, DemosaicMode, ExportResizeMode, ExposureParams, MaskCombineMode, MaskGeometry,
    MaskKind, SigmoidColorProcessing, MAX_EXPORT_EDGE, MAX_LOCAL_MASKS,
};
use crate::ui::components::adjustment_slider::{adjustment_slider, slider_scroll_locked};
use crate::ui::components::color_grading::color_grading_editor;
use crate::ui::components::tone_curve_editor::tone_curve_editor;
use crate::ui::layout::ScreenLayout;
use crate::ui::mask_component_color;
use eframe::egui::{self, Ui};

pub struct Sidebar;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaskCardSize {
    Group,
    Submask,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MaskStripOrientation {
    Horizontal,
    Vertical,
}

impl MaskCardSize {
    fn card_size(self) -> egui::Vec2 {
        match self {
            Self::Group => egui::vec2(68.0, 72.0),
            Self::Submask => egui::vec2(56.0, 62.0),
        }
    }

    fn image_edge(self) -> f32 {
        match self {
            Self::Group => 54.0,
            Self::Submask => 44.0,
        }
    }

    fn label_font_size(self) -> f32 {
        match self {
            Self::Group => 9.5,
            Self::Submask => 8.5,
        }
    }

    fn create_button_size(self, orientation: MaskStripOrientation) -> egui::Vec2 {
        const THIN_EDGE: f32 = 26.0;
        let card = self.card_size();
        match orientation {
            // Portrait: the strip runs left-to-right, so creation controls are
            // narrow columns that keep the full card height.
            MaskStripOrientation::Horizontal => egui::vec2(THIN_EDGE, card.y),
            // Wider screens: the strip runs top-to-bottom, so creation controls
            // become short rows that keep the full card width.
            MaskStripOrientation::Vertical => egui::vec2(card.x, THIN_EDGE),
        }
    }
}

impl Sidebar {
    const SCROLLBAR_GUTTER: f32 = 18.0;
    const MASK_THUMBNAIL_EDGE: u32 = 64;
    pub(crate) const VERTICAL_MASK_STRIP_HEIGHT: f32 = 92.0;
    pub(crate) const HORIZONTAL_MASK_STRIP_WIDTH: f32 = 92.0;

    pub fn show(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout, frame: &eframe::Frame) {
        let available_width = ui.available_width().max(1.0);
        let content_width = match layout {
            ScreenLayout::Horizontal => (available_width - Self::SCROLLBAR_GUTTER)
                .max(220.0)
                .min(available_width),
            ScreenLayout::Vertical => (available_width - Self::SCROLLBAR_GUTTER).max(1.0),
        };
        ui.set_width(content_width);
        ui.set_max_width(content_width);
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        egui::ScrollArea::horizontal()
            .id_salt("develop-sidebar-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label) in [
                        (SidebarTab::Adjustments, "Adjustments"),
                        (SidebarTab::Masks, "Masks"),
                        (SidebarTab::Inpainting, "Inpainting"),
                        (SidebarTab::Export, "Export"),
                    ] {
                        ui.selectable_value(&mut app.sidebar_tab, tab, label);
                    }
                });
            });
        ui.add_space(2.0);
        ui.separator();

        if layout == ScreenLayout::Vertical {
            Self::show_vertical_section_tabs(ui, app);
        }

        let sidebar_scroll_source = if slider_scroll_locked(ui.ctx()) {
            egui::scroll_area::ScrollSource::NONE
        } else {
            egui::scroll_area::ScrollSource::default()
        };
        egui::ScrollArea::vertical()
            .id_salt("develop-sidebar-content")
            .scroll_source(sidebar_scroll_source)
            .auto_shrink([false, false])
            .show(ui, |ui| match app.sidebar_tab {
                SidebarTab::Adjustments => Self::show_adjustments(ui, app, layout),
                SidebarTab::Masks => Self::show_masks(ui, app, layout, frame),
                SidebarTab::Inpainting => Self::show_placeholder(
                    ui,
                    "Inpainting",
                    "Healing, object removal, and generative inpainting controls are coming later.",
                ),
                SidebarTab::Export => Self::show_export(ui, app, frame),
            });
    }

    fn show_adjustments(ui: &mut Ui, app: &mut AurawApp, layout: ScreenLayout) {
        ui.horizontal(|ui| {
            ui.heading("Adjustments");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset all").clicked() {
                    app.reset_develop_adjustments();
                }
            });
        });
        ui.label(
            egui::RichText::new("Scene-referred RAW controls")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(2.0);
        ui.separator();

        let mut changed = false;
        if layout == ScreenLayout::Vertical {
            match app.adjustment_section {
                AdjustmentSection::Light => {
                    changed |= Self::show_basic(ui, &mut app.exposure, false);
                }
                AdjustmentSection::ToneCurve => {
                    changed |= Self::show_tone_curve(
                        ui,
                        &mut app.exposure,
                        &mut app.tone_curve_tab,
                        false,
                    );
                }
                AdjustmentSection::Color => {
                    changed |= Self::show_color(ui, &mut app.exposure, false);
                }
                AdjustmentSection::ColorGrading => {
                    changed |= Self::show_color_grading(
                        ui,
                        &mut app.exposure.color_grading,
                        &mut app.color_grade_tab,
                        false,
                    );
                }
                AdjustmentSection::Effects => {
                    changed |= Self::show_presence(
                        ui,
                        &mut app.exposure,
                        app.expert_mode,
                        false,
                    );
                }
                AdjustmentSection::ColorMixer => {
                    changed |= Self::show_hsl(ui, &mut app.exposure, false);
                }
                AdjustmentSection::AdvancedRendering if app.expert_mode => {
                    changed |= Self::show_rendering(ui, &mut app.exposure, false);
                }
                AdjustmentSection::Raw if app.expert_mode => {
                    changed |= Self::show_raw(ui, &mut app.exposure, false);
                }
                _ => {}
            }
        } else {
            changed |= Self::show_basic(ui, &mut app.exposure, true);
            changed |= Self::show_tone_curve(
                ui,
                &mut app.exposure,
                &mut app.tone_curve_tab,
                true,
            );
            changed |= Self::show_color(ui, &mut app.exposure, true);
            changed |= Self::show_color_grading(
                ui,
                &mut app.exposure.color_grading,
                &mut app.color_grade_tab,
                true,
            );
            changed |= Self::show_presence(ui, &mut app.exposure, app.expert_mode, true);
            changed |= Self::show_hsl(ui, &mut app.exposure, true);
            if app.expert_mode {
                changed |= Self::show_rendering(ui, &mut app.exposure, true);
                changed |= Self::show_raw(ui, &mut app.exposure, true);
            }
        }

        if changed {
            app.exposure.sanitize_tone_curves();
            app.mark_pipeline_dirty();
        }
    }

    fn show_vertical_section_tabs(ui: &mut Ui, app: &mut AurawApp) {
        match app.sidebar_tab {
            SidebarTab::Adjustments => {
                if !app.expert_mode
                    && matches!(
                        app.adjustment_section,
                        AdjustmentSection::AdvancedRendering | AdjustmentSection::Raw
                    )
                {
                    app.adjustment_section = AdjustmentSection::Light;
                }
                Self::show_adjustment_tabs(ui, app);
            }
            SidebarTab::Masks => Self::show_mask_tabs(ui, app),
            SidebarTab::Inpainting | SidebarTab::Export => {}
        }
    }

    fn show_adjustment_tabs(ui: &mut Ui, app: &mut AurawApp) {
        egui::ScrollArea::horizontal()
            .id_salt("adjustment-section-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (section, label) in [
                        (AdjustmentSection::Light, "Light"),
                        (AdjustmentSection::ToneCurve, "Tone Curve"),
                        (AdjustmentSection::Color, "Color"),
                        (AdjustmentSection::ColorGrading, "Color Grading"),
                        (AdjustmentSection::Effects, "Effects"),
                        (AdjustmentSection::ColorMixer, "Color Mixer"),
                    ] {
                        ui.selectable_value(&mut app.adjustment_section, section, label);
                    }
                    if app.expert_mode {
                        ui.selectable_value(
                            &mut app.adjustment_section,
                            AdjustmentSection::AdvancedRendering,
                            "Advanced",
                        );
                        ui.selectable_value(
                            &mut app.adjustment_section,
                            AdjustmentSection::Raw,
                            "Raw",
                        );
                    }
                });
            });
        ui.separator();
    }

    fn show_mask_tabs(ui: &mut Ui, app: &mut AurawApp) {
        egui::ScrollArea::horizontal()
            .id_salt("mask-section-tabs")
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysHidden)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (section, label) in [
                        (MaskSection::Properties, "Mask Properties"),
                        (MaskSection::Light, "Light"),
                        (MaskSection::ToneCurve, "Tone Curve"),
                        (MaskSection::Color, "Color"),
                        (MaskSection::ColorGrading, "Color Grading"),
                        (MaskSection::Effects, "Effects"),
                        (MaskSection::ColorMixer, "Color Mixer"),
                    ] {
                        ui.selectable_value(&mut app.mask_section, section, label);
                    }
                });
            });
        ui.separator();
    }

    fn adjustment_section(
        ui: &mut Ui,
        title: &'static str,
        default_open: bool,
        foldable: bool,
        contents: impl FnOnce(&mut Ui),
    ) {
        if foldable {
            egui::CollapsingHeader::new(title)
                .default_open(default_open)
                .show(ui, contents);
        } else {
            contents(ui);
        }
    }

    fn show_masks(
        ui: &mut Ui,
        app: &mut AurawApp,
        layout: ScreenLayout,
        frame: &eframe::Frame,
    ) {
        ui.heading("Masks");
        ui.add_space(4.0);

        if app.masks.masks.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.weak("No masks created yet.");
                ui.weak("Use the + card in the mask strip to create one.");
            });
            return;
        }

        // Portrait keeps the compact fixed tabs. Wider screens expose all
        // mask controls as normal collapsible sections, matching the desktop
        // adjustment sidebar while the thumbnail strip remains beside it.
        match layout {
            ScreenLayout::Vertical => Self::show_masks_vertical_details(ui, app, frame),
            ScreenLayout::Horizontal => Self::show_masks_horizontal_details(ui, app, frame),
        }
    }

    pub(crate) fn show_vertical_mask_strip(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
    ) {
        Self::show_mask_strip(ui, app, frame, MaskStripOrientation::Horizontal);
    }

    pub(crate) fn show_horizontal_mask_strip(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
    ) {
        Self::show_mask_strip(ui, app, frame, MaskStripOrientation::Vertical);
    }

    fn show_mask_strip(
        ui: &mut Ui,
        app: &mut AurawApp,
        frame: &eframe::Frame,
        orientation: MaskStripOrientation,
    ) {
        ui.spacing_mut().item_spacing = egui::vec2(4.0, 2.0);

        if app.masks.masks.is_empty() {
            app.masks.selected_mask = None;
            app.masks.selected_component = None;
        } else if app
            .masks
            .selected_mask
            .is_none_or(|index| index >= app.masks.masks.len())
        {
            app.masks.selected_mask = Some(app.masks.masks.len().saturating_sub(1));
            app.masks.selected_component = Some(0);
        }

        Self::refresh_mask_thumbnails(ui, app);

        let selected_mask_before = app.masks.selected_mask;
        let selected_component_before = app.masks.selected_component;
        let mut select_mask = None;
        let mut select_component = None;
        let mut new_mask = None;
        let mut add_component = None;
        let mut remove_mask = None;
        let mut remove_component = None;
        let mut group_enabled_changed = false;
        let mut component_dirty_mask = None;

        {
            let mut show_cards = |ui: &mut Ui| {
                ui.add_enabled_ui(app.masks.masks.len() < MAX_LOCAL_MASKS, |ui| {
                    Self::create_mask_group_card(ui, &mut new_mask, orientation);
                });
                ui.add_space(2.0);

                for index in (0..app.masks.masks.len()).rev() {
                    let mask_name = app.masks.masks[index].name.clone();
                    let mask_enabled = app.masks.masks[index].enabled;
                    let component_count = app.masks.masks[index].components.len();
                    let badge = component_count.to_string();
                    let response = Self::mask_thumbnail_card(
                        ui,
                        app.mask_thumbnail_group_textures.get(index),
                        &mask_name,
                        selected_mask_before == Some(index),
                        Some(&badge),
                        mask_enabled,
                        MaskCardSize::Group,
                    );
                    if response.clicked() {
                        select_mask = Some(index);
                    }
                    response.context_menu(|ui| {
                        Self::mask_group_context_menu(
                            ui,
                            &mut app.masks.masks[index],
                            &mut group_enabled_changed,
                            &mut remove_mask,
                            index,
                        );
                    });

                    // The selected group's sub-masks are inserted directly
                    // after the parent. That means to its right in portrait
                    // mode and directly below it in the desktop vertical strip.
                    if selected_mask_before == Some(index) {
                        ui.add_space(1.0);
                        for component_index in 0..component_count {
                            let component = &app.masks.masks[index].components[component_index];
                            let component_name = component.name.clone();
                            let component_enabled = component.enabled;
                            let component_badge = if component_index == 0 {
                                "BASE"
                            } else {
                                match component.combine {
                                    MaskCombineMode::Add => "+",
                                    MaskCombineMode::Subtract => "−",
                                    MaskCombineMode::Intersect => "∩",
                                }
                            };
                            let response = Self::mask_thumbnail_card(
                                ui,
                                app.mask_thumbnail_component_textures.get(component_index),
                                &component_name,
                                selected_component_before == Some(component_index),
                                Some(component_badge),
                                component_enabled,
                                MaskCardSize::Submask,
                            );
                            if response.clicked() {
                                select_component = Some(component_index);
                            }
                            let mut menu_geometry_changed = false;
                            let mut menu_remove_component = None;
                            response.context_menu(|ui| {
                                Self::submask_context_menu(
                                    ui,
                                    &mut app.masks.masks[index].components[component_index],
                                    component_count > 1,
                                    &mut menu_geometry_changed,
                                    &mut menu_remove_component,
                                    component_index,
                                );
                            });
                            if menu_geometry_changed {
                                component_dirty_mask = Some(index);
                            }
                            if let Some(component_index) = menu_remove_component {
                                remove_component = Some((index, component_index));
                            }
                        }
                        Self::create_submask_card(ui, &mut add_component, orientation);
                        ui.add_space(2.0);
                    }
                }
            };

            match orientation {
                MaskStripOrientation::Horizontal => {
                    egui::ScrollArea::horizontal()
                        .id_salt("vertical-mask-card-strip")
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        )
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.horizontal(|ui| show_cards(ui));
                        });
                }
                MaskStripOrientation::Vertical => {
                    egui::ScrollArea::vertical()
                        .id_salt("horizontal-mask-card-strip")
                        .scroll_bar_visibility(
                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                        )
                        .auto_shrink([false, false])
                        .show(ui, |ui| {
                            ui.vertical_centered(|ui| show_cards(ui));
                        });
                }
            }
        }

        if group_enabled_changed {
            app.mark_mask_adjustments_dirty();
        }
        if let Some(mask_index) = component_dirty_mask {
            app.mark_mask_geometry_dirty(mask_index);
        }

        if let Some(index) = remove_mask {
            app.masks.selected_mask = Some(index);
            app.masks.remove_selected_mask();
            app.mark_all_mask_layers_dirty();
            app.mask_thumbnail_component_mask = None;
            if let Some(kind) = app.masks.selected_component().map(|component| component.kind) {
                app.select_mask_tool(kind);
            } else {
                app.active_mask_tool = None;
            }
            Self::refresh_mask_thumbnails(ui, app);
        } else if let Some((mask_index, component_index)) = remove_component {
            app.masks.selected_mask = Some(mask_index);
            app.masks.selected_component = Some(component_index);
            if app.masks.remove_selected_component().is_some() {
                app.mark_mask_geometry_dirty(mask_index);
                app.mask_thumbnail_component_mask = None;
                if let Some(kind) = app.masks.selected_component().map(|component| component.kind) {
                    app.select_mask_tool(kind);
                }
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(kind) = new_mask {
            if let Some((mask_index, _)) = app.masks.add_mask(kind) {
                app.activate_mask_tool(kind);
                Self::prepare_content_mask(app, frame, kind);
                app.mark_mask_geometry_dirty(mask_index);
                app.mask_thumbnail_component_mask = None;
                app.blink_selected_mask();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some((kind, combine)) = add_component {
            if let Some((mask_index, _)) = app.masks.add_component(kind, combine) {
                app.activate_mask_tool(kind);
                Self::prepare_content_mask(app, frame, kind);
                app.mark_mask_geometry_dirty(mask_index);
                app.mask_thumbnail_component_mask = None;
                app.blink_selected_component();
                Self::refresh_mask_thumbnails(ui, app);
            }
        } else if let Some(index) = select_mask {
            app.masks.selected_mask = Some(index);
            app.masks.selected_component = Some(0);
            app.mask_thumbnail_component_mask = None;
            if let Some(kind) = app.masks.selected_component().map(|component| component.kind) {
                app.select_mask_tool(kind);
            }
            app.blink_selected_mask();
            Self::refresh_mask_thumbnails(ui, app);
        } else if let Some(component_index) = select_component {
            app.masks.selected_component = Some(component_index);
            if let Some(kind) = app.masks.selected_component().map(|component| component.kind) {
                app.select_mask_tool(kind);
            }
            app.blink_selected_component();
        }
    }

    fn mask_kind_menu(ui: &mut Ui, unavailable_message: &str) -> Option<MaskKind> {
        let mut selected = None;
        for kind in [
            MaskKind::Brush,
            MaskKind::Radial,
            MaskKind::Linear,
            MaskKind::Subject,
            MaskKind::Background,
            MaskKind::Object,
            MaskKind::Landscape,
            MaskKind::LuminanceRange,
            MaskKind::ColorRange,
            MaskKind::DepthRange,
        ] {
            let label = if kind.is_available() {
                kind.label().to_owned()
            } else {
                format!("{} · soon", kind.label())
            };
            if ui
                .add_enabled(kind.is_available(), egui::Button::new(label))
                .on_disabled_hover_text(unavailable_message)
                .clicked()
            {
                selected = Some(kind);
                ui.close();
            }
        }
        selected
    }

    fn submask_creation_menu(
        ui: &mut Ui,
        unavailable_message: &str,
    ) -> Option<(MaskKind, MaskCombineMode)> {
        let mut selected = None;
        ui.label(egui::RichText::new("Combine as").weak());
        for combine in [
            MaskCombineMode::Add,
            MaskCombineMode::Subtract,
            MaskCombineMode::Intersect,
        ] {
            ui.menu_button(combine.label(), |ui| {
                if let Some(kind) = Self::mask_kind_menu(ui, unavailable_message) {
                    selected = Some((kind, combine));
                }
            });
        }
        selected
    }

    fn mask_group_context_menu(
        ui: &mut Ui,
        mask: &mut crate::pipeline::LocalMask,
        enabled_changed: &mut bool,
        remove_mask: &mut Option<usize>,
        mask_index: usize,
    ) {
        ui.label(egui::RichText::new("Rename mask group").weak());
        ui.add_sized(
            [190.0, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(&mut mask.name),
        );
        ui.separator();
        *enabled_changed |= ui.checkbox(&mut mask.enabled, "Enabled").changed();
        ui.separator();
        if ui.button("Delete mask group").clicked() {
            *remove_mask = Some(mask_index);
            ui.close();
        }
    }

    fn submask_context_menu(
        ui: &mut Ui,
        component: &mut crate::pipeline::MaskComponent,
        can_delete: bool,
        geometry_changed: &mut bool,
        remove_component: &mut Option<usize>,
        component_index: usize,
    ) {
        ui.label(egui::RichText::new("Rename sub-mask").weak());
        ui.add_sized(
            [190.0, ui.spacing().interact_size.y],
            egui::TextEdit::singleline(&mut component.name),
        );
        ui.separator();
        *geometry_changed |= ui.checkbox(&mut component.enabled, "Enabled").changed();
        *geometry_changed |= ui.checkbox(&mut component.invert, "Invert").changed();
        ui.separator();
        if ui
            .add_enabled(can_delete, egui::Button::new("Delete sub-mask"))
            .on_disabled_hover_text("A mask group must contain at least one sub-mask")
            .clicked()
        {
            *remove_component = Some(component_index);
            ui.close();
        }
    }

    fn create_mask_group_card(
        ui: &mut Ui,
        new_mask: &mut Option<MaskKind>,
        orientation: MaskStripOrientation,
    ) {
        let size = MaskCardSize::Group.create_button_size(orientation);
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
            |ui| {
                ui.spacing_mut().interact_size = size;
                ui.menu_button(egui::RichText::new("+").size(20.0).strong(), |ui| {
                    *new_mask = Self::mask_kind_menu(
                        ui,
                        "This mask type is planned but not implemented yet.",
                    );
                })
                .response
                .on_hover_text("Create a new mask group");
            },
        );
    }

    fn create_submask_card(
        ui: &mut Ui,
        add_component: &mut Option<(MaskKind, MaskCombineMode)>,
        orientation: MaskStripOrientation,
    ) {
        let size = MaskCardSize::Submask.create_button_size(orientation);
        ui.allocate_ui_with_layout(
            size,
            egui::Layout::centered_and_justified(egui::Direction::LeftToRight),
            |ui| {
                ui.spacing_mut().interact_size = size;
                ui.menu_button(egui::RichText::new("+").size(18.0).strong(), |ui| {
                    *add_component = Self::submask_creation_menu(
                        ui,
                        "This sub-mask type is planned but not implemented yet.",
                    );
                })
                .response
                .on_hover_text("Add a sub-mask to the selected group");
            },
        );
    }

    fn show_masks_horizontal_details(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        if mask_index >= app.masks.masks.len() {
            return;
        }

        ui.label(
            egui::RichText::new(app.masks.masks[mask_index].name.clone())
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(5.0);

        let component_index = app
            .masks
            .selected_component
            .unwrap_or(0)
            .min(app.masks.masks[mask_index].components.len().saturating_sub(1));
        app.masks.selected_component = Some(component_index);

        let mut geometry_changed = false;
        let mut adjustments_changed = false;
        let mut request_subject = false;
        let mut brush_mode = app.brush_mode;
        let mut local_curve_tab = app.tone_curve_tab;
        let mut local_color_grade_tab = app.color_grade_tab;

        {
            let mask = &mut app.masks.masks[mask_index];

            Self::adjustment_section(ui, "Mask Properties", true, true, |ui| {
                geometry_changed |= Self::show_vertical_mask_properties(
                    ui,
                    mask,
                    component_index,
                    &mut brush_mode,
                    &mut request_subject,
                );
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.strong("Local Adjustments");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Reset adjustments").clicked() {
                        mask.adjustments.reset();
                        adjustments_changed = true;
                    }
                });
            });

            for (section, label, default_open) in [
                (MaskSection::Light, "Light", true),
                (MaskSection::ToneCurve, "Tone Curve", false),
                (MaskSection::Color, "Color", false),
                (MaskSection::ColorGrading, "Color Grading", false),
                (MaskSection::Effects, "Effects", false),
                (MaskSection::ColorMixer, "Color Mixer", false),
            ] {
                Self::adjustment_section(ui, label, default_open, true, |ui| {
                    adjustments_changed |= Self::show_local_mask_adjustment_section(
                        ui,
                        &mut mask.adjustments,
                        section,
                        &mut local_curve_tab,
                        &mut local_color_grade_tab,
                    );
                });
            }
        }

        app.tone_curve_tab = local_curve_tab;
        app.color_grade_tab = local_color_grade_tab;
        app.brush_mode = brush_mode;
        if request_subject {
            app.request_subject_mask(frame);
        }
        if geometry_changed {
            app.mark_mask_geometry_dirty(mask_index);
        }
        if adjustments_changed {
            app.mark_mask_adjustments_dirty();
        }
    }

    fn show_masks_vertical_details(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        if mask_index >= app.masks.masks.len() {
            return;
        }

        ui.label(
            egui::RichText::new(app.masks.masks[mask_index].name.clone())
                .strong()
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(5.0);

        let mask_section = app.mask_section;
        let component_index = app
            .masks
            .selected_component
            .unwrap_or(0)
            .min(app.masks.masks[mask_index].components.len().saturating_sub(1));
        app.masks.selected_component = Some(component_index);

        let mut geometry_changed = false;
        let mut adjustments_changed = false;
        let mut request_subject = false;
        let mut brush_mode = app.brush_mode;
        let mut local_curve_tab = app.tone_curve_tab;
        let mut local_color_grade_tab = app.color_grade_tab;

        {
            let mask = &mut app.masks.masks[mask_index];
            match mask_section {
                MaskSection::Properties => {
                    geometry_changed |= Self::show_vertical_mask_properties(
                        ui,
                        mask,
                        component_index,
                        &mut brush_mode,
                        &mut request_subject,
                    );
                }
                section => {
                    ui.horizontal(|ui| {
                        ui.strong(match section {
                            MaskSection::Light => "Light",
                            MaskSection::ToneCurve => "Tone Curve",
                            MaskSection::Color => "Color",
                            MaskSection::ColorGrading => "Color Grading",
                            MaskSection::Effects => "Effects",
                            MaskSection::ColorMixer => "Color Mixer",
                            MaskSection::Properties => "Mask Properties",
                        });
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.small_button("Reset adjustments").clicked() {
                                mask.adjustments.reset();
                                adjustments_changed = true;
                            }
                        });
                    });
                    ui.add_space(4.0);
                    adjustments_changed |= Self::show_local_mask_adjustment_section(
                        ui,
                        &mut mask.adjustments,
                        section,
                        &mut local_curve_tab,
                        &mut local_color_grade_tab,
                    );
                }
            }
        }

        app.tone_curve_tab = local_curve_tab;
        app.color_grade_tab = local_color_grade_tab;
        app.brush_mode = brush_mode;
        if request_subject {
            app.request_subject_mask(frame);
        }
        if geometry_changed {
            app.mark_mask_geometry_dirty(mask_index);
        }
        if adjustments_changed {
            app.mark_mask_adjustments_dirty();
        }
    }

    fn show_vertical_mask_properties(
        ui: &mut Ui,
        mask: &mut crate::pipeline::LocalMask,
        component_index: usize,
        brush_mode: &mut BrushMode,
        request_subject: &mut bool,
    ) -> bool {
        let mut geometry_changed = adjustment_slider(
            ui,
            "Mask opacity",
            &mut mask.opacity,
            0.0..=1.0,
            2,
            0.01,
            Some("Controls the strength of the entire mask before local adjustments."),
        );

        let Some(component) = mask.components.get_mut(component_index) else {
            return geometry_changed;
        };

        ui.add_space(4.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                ui.strong(component.name.as_str());
                ui.weak(component.kind.label());
                geometry_changed |= ui.checkbox(&mut component.invert, "Invert").changed();
                if component_index > 0 {
                    let before = component.combine;
                    egui::ComboBox::from_id_salt("vertical-mask-combine")
                        .selected_text(component.combine.label())
                        .show_ui(ui, |ui| {
                            for mode in [
                                MaskCombineMode::Add,
                                MaskCombineMode::Subtract,
                                MaskCombineMode::Intersect,
                            ] {
                                ui.selectable_value(&mut component.combine, mode, mode.label());
                            }
                        });
                    geometry_changed |= before != component.combine;
                }
            });

            match &mut component.geometry {
                MaskGeometry::Brush {
                    size,
                    feather,
                    dabs,
                } => {
                    ui.horizontal(|ui| {
                        ui.selectable_value(brush_mode, BrushMode::Paint, "Brush");
                        ui.selectable_value(brush_mode, BrushMode::Erase, "Eraser");
                    });
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Size",
                        size,
                        0.0025..=0.25,
                        3,
                        0.0025,
                        Some("Brush radius relative to the shorter image edge."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softness from the brush core to its edge."),
                    );
                    if ui.small_button("Clear strokes").clicked() {
                        dabs.clear();
                        geometry_changed = true;
                    }
                    ui.small(format!("{} brush dabs", dabs.len()));
                }
                MaskGeometry::Radial { feather, .. } => {
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Soft transition from the ellipse interior to its edge."),
                    );
                }
                MaskGeometry::Linear { feather, .. } => {
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Feather",
                        feather,
                        0.02..=1.0,
                        2,
                        0.01,
                        Some("Controls the width of the gradient transition."),
                    );
                }
                MaskGeometry::Ai {
                    mask: generated_mask,
                    feather,
                } => {
                    if generated_mask.is_none() {
                        ui.horizontal(|ui| {
                            ui.spinner();
                            ui.label("Waiting for subject selection");
                        });
                        if ui.button("Generate subject mask").clicked() {
                            *request_subject = true;
                        }
                    }
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens the BiRefNet subject boundary."),
                    );
                }
                MaskGeometry::LuminanceRange {
                    low,
                    high,
                    feather,
                    ..
                } => {
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Range low",
                        low,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Lowest included scene luminance."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Range high",
                        high,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Highest included scene luminance."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Range feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens both luminance-range boundaries."),
                    );
                }
                MaskGeometry::ColorRange {
                    tolerance,
                    feather,
                    sampled,
                    ..
                } => {
                    ui.label(if *sampled {
                        "Drag on the image to choose another color."
                    } else {
                        "Drag on the image to sample a color."
                    });
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Tolerance",
                        tolerance,
                        0.005..=1.0,
                        3,
                        0.005,
                        Some("Expands the selected color region in perceptual OkLab space."),
                    );
                    geometry_changed |= adjustment_slider(
                        ui,
                        "Color feather",
                        feather,
                        0.0..=1.0,
                        2,
                        0.01,
                        Some("Softens the color-distance cutoff."),
                    );
                }
                MaskGeometry::Placeholder => {
                    ui.label("This mask type is not implemented yet.");
                }
            }
        });

        geometry_changed
    }

    fn refresh_mask_thumbnails(ui: &mut Ui, app: &mut AurawApp) {
        let selected_mask = app.masks.selected_mask;
        let group_cache_valid = app.mask_thumbnail_revision == app.mask_overlay_revision
            && app.mask_thumbnail_group_textures.len() == app.masks.masks.len();
        let component_len = selected_mask
            .and_then(|index| app.masks.masks.get(index))
            .map_or(0, |mask| mask.components.len());
        let component_cache_valid = group_cache_valid
            && app.mask_thumbnail_component_mask == selected_mask
            && app.mask_thumbnail_component_textures.len() == component_len;
        if group_cache_valid && component_cache_valid {
            return;
        }

        let (image_width, image_height) = app
            .preview_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height))
            .unwrap_or((1, 1));
        let edge = Self::MASK_THUMBNAIL_EDGE;
        let (thumbnail_width, thumbnail_height) =
            Self::thumbnail_fit_size(image_width, image_height, edge);

        if !group_cache_valid {
            let images: Vec<_> = (0..app.masks.masks.len())
                .map(|index| {
                    let gray = app.masks.rasterize_layer(
                        index,
                        thumbnail_width,
                        thumbnail_height,
                        image_width,
                        image_height,
                    );
                    Self::gray_thumbnail_image(
                        gray,
                        thumbnail_width,
                        thumbnail_height,
                        edge,
                    )
                })
                .collect();
            Self::update_thumbnail_textures(
                ui,
                &mut app.mask_thumbnail_group_textures,
                images,
                "mask-group-thumbnail",
            );
        }

        if !component_cache_valid {
            let images: Vec<_> = selected_mask
                .and_then(|mask_index| {
                    app.masks
                        .masks
                        .get(mask_index)
                        .map(|mask| (mask_index, mask))
                })
                .map(|(mask_index, mask)| {
                    (0..mask.components.len())
                        .map(|component_index| {
                            let gray = app.masks.rasterize_component_layer(
                                mask_index,
                                component_index,
                                thumbnail_width,
                                thumbnail_height,
                                image_width,
                                image_height,
                            );
                            Self::gray_thumbnail_image(
                                gray,
                                thumbnail_width,
                                thumbnail_height,
                                edge,
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            Self::update_thumbnail_textures(
                ui,
                &mut app.mask_thumbnail_component_textures,
                images,
                "mask-component-thumbnail",
            );
        }

        app.mask_thumbnail_revision = app.mask_overlay_revision;
        app.mask_thumbnail_component_mask = selected_mask;
    }

    fn thumbnail_fit_size(image_width: u32, image_height: u32, edge: u32) -> (u32, u32) {
        let image_width = image_width.max(1);
        let image_height = image_height.max(1);
        if image_width >= image_height {
            let height = ((edge as f64 * image_height as f64 / image_width as f64).round() as u32)
                .clamp(1, edge);
            (edge, height)
        } else {
            let width = ((edge as f64 * image_width as f64 / image_height as f64).round() as u32)
                .clamp(1, edge);
            (width, edge)
        }
    }

    fn gray_thumbnail_image(
        gray: Vec<u8>,
        width: u32,
        height: u32,
        edge: u32,
    ) -> egui::ColorImage {
        let width = width.min(edge) as usize;
        let height = height.min(edge) as usize;
        let edge = edge as usize;
        let mut square = vec![0_u8; edge * edge];
        let offset_x = (edge - width) / 2;
        let offset_y = (edge - height) / 2;

        for row in 0..height {
            let source_start = row * width;
            let source_end = (source_start + width).min(gray.len());
            let copied = source_end.saturating_sub(source_start);
            if copied == 0 {
                break;
            }
            let destination_start = (offset_y + row) * edge + offset_x;
            square[destination_start..destination_start + copied]
                .copy_from_slice(&gray[source_start..source_end]);
        }

        egui::ColorImage::from_gray([edge, edge], &square)
    }

    fn update_thumbnail_textures(
        ui: &mut Ui,
        textures: &mut Vec<egui::TextureHandle>,
        images: Vec<egui::ColorImage>,
        prefix: &str,
    ) {
        let desired_len = images.len();
        for (index, image) in images.into_iter().enumerate() {
            if let Some(texture) = textures.get_mut(index) {
                texture.set(image, egui::TextureOptions::LINEAR);
            } else {
                textures.push(ui.ctx().load_texture(
                    format!("{prefix}-{index}"),
                    image,
                    egui::TextureOptions::LINEAR,
                ));
            }
        }
        textures.truncate(desired_len);
    }

    fn mask_thumbnail_card(
        ui: &mut Ui,
        texture: Option<&egui::TextureHandle>,
        label: &str,
        selected: bool,
        badge: Option<&str>,
        enabled: bool,
        card_size: MaskCardSize,
    ) -> egui::Response {
        use eframe::egui::{Align2, Color32, FontId, Stroke, StrokeKind};

        let size = card_size.card_size();
        let image_edge = card_size.image_edge();
        let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
        let visuals = ui.visuals();
        let fill = if selected {
            visuals.selection.bg_fill.gamma_multiply(0.24)
        } else if response.hovered() {
            visuals.widgets.hovered.bg_fill
        } else {
            visuals.widgets.inactive.bg_fill
        };
        let stroke = if selected {
            Stroke::new(2.0, visuals.selection.bg_fill)
        } else {
            Stroke::new(1.0, visuals.widgets.noninteractive.bg_stroke.color)
        };
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 5.0, fill);
        painter.rect_stroke(rect, 5.0, stroke, StrokeKind::Inside);

        // The thumbnail well is always square. The texture itself contains a
        // centered, letterboxed rendering at the RAW image's aspect ratio.
        let image_rect = egui::Rect::from_min_size(
            egui::pos2(rect.center().x - image_edge * 0.5, rect.min.y + 5.0),
            egui::vec2(image_edge, image_edge),
        );
        painter.rect_filled(image_rect, 3.0, Color32::BLACK);
        if let Some(texture) = texture {
            let tint = if enabled {
                Color32::WHITE
            } else {
                Color32::from_white_alpha(80)
            };
            painter.image(
                texture.id(),
                image_rect,
                egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
                tint,
            );
        }

        if let Some(badge) = badge {
            let (font_size, badge_height, horizontal_padding) = match card_size {
                MaskCardSize::Group => (10.5, 18.0, 10.0),
                MaskCardSize::Submask => (9.0, 16.0, 8.0),
            };
            let badge_size = egui::vec2(
                (badge.chars().count() as f32 * font_size * 0.62 + horizontal_padding)
                    .max(badge_height + 2.0),
                badge_height,
            );
            let badge_rect =
                egui::Rect::from_min_size(image_rect.right_bottom() - badge_size, badge_size);
            painter.rect_filled(badge_rect, 3.0, Color32::from_black_alpha(210));
            painter.text(
                badge_rect.center(),
                Align2::CENTER_CENTER,
                badge,
                FontId::proportional(font_size),
                Color32::WHITE,
            );
        }

        let max_label_chars = match card_size {
            MaskCardSize::Group => 13,
            MaskCardSize::Submask => 10,
        };
        let display_label: String = label.chars().take(max_label_chars).collect();
        painter.text(
            egui::pos2(rect.center().x, rect.bottom() - 9.0),
            Align2::CENTER_CENTER,
            display_label,
            FontId::proportional(card_size.label_font_size()),
            if enabled {
                visuals.text_color()
            } else {
                visuals.weak_text_color()
            },
        );
        response
    }

    fn prepare_content_mask(app: &mut AurawApp, frame: &eframe::Frame, kind: MaskKind) {
        match kind {
            MaskKind::Subject | MaskKind::Background => app.request_subject_mask(frame),
            MaskKind::LuminanceRange | MaskKind::ColorRange => {
                if let Err(error) = app.capture_mask_source(frame) {
                    app.status = error;
                    return;
                }
                let source = app.mask_source_cache.clone();
                if let Some(component) = app.masks.selected_component_mut() {
                    match &mut component.geometry {
                        MaskGeometry::LuminanceRange { source: target, .. }
                        | MaskGeometry::ColorRange { source: target, .. } => *target = source,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }

    fn show_local_mask_adjustment_section(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        section: MaskSection,
        selected_tab: &mut ToneCurveTab,
        selected_grade_tab: &mut ColorGradeTab,
    ) -> bool {
        match section {
            MaskSection::Properties => false,
            MaskSection::Light => Self::show_local_mask_light(ui, adjustment),
            MaskSection::ToneCurve => {
                Self::show_local_mask_tone_curve(ui, adjustment, selected_tab)
            }
            MaskSection::Color => Self::show_local_mask_color(ui, adjustment),
            MaskSection::ColorGrading => {
                Self::show_local_mask_color_grading(ui, adjustment, selected_grade_tab)
            }
            MaskSection::Effects => Self::show_local_mask_effects(ui, adjustment),
            MaskSection::ColorMixer => Self::show_local_mask_color_mixer(ui, adjustment),
        }
    }

    fn show_local_mask_light(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        changed |= adjustment_slider(
            ui,
            "Exposure",
            &mut adjustment.exposure,
            -5.0..=5.0,
            2,
            0.05,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Contrast",
            &mut adjustment.contrast,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Highlights",
            &mut adjustment.highlights,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Shadows",
            &mut adjustment.shadows,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Whites",
            &mut adjustment.whites,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Blacks",
            &mut adjustment.blacks,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed
    }

    fn show_local_mask_color(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        changed |= adjustment_slider(
            ui,
            "Temperature",
            &mut adjustment.temperature,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Tint",
            &mut adjustment.tint,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Saturation",
            &mut adjustment.saturation,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed
    }

    fn show_local_mask_effects(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        changed |= adjustment_slider(
            ui,
            "Texture",
            &mut adjustment.texture,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Clarity",
            &mut adjustment.clarity,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed |= adjustment_slider(
            ui,
            "Dehaze",
            &mut adjustment.dehaze,
            -100.0..=100.0,
            0,
            1.0,
            None,
        );
        changed
    }

    fn show_local_mask_color_grading(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_grade_tab: &mut ColorGradeTab,
    ) -> bool {
        color_grading_editor(
            ui,
            &mut adjustment.color_grading,
            selected_grade_tab,
        )
    }

    fn show_local_mask_tone_curve(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_tab: &mut ToneCurveTab,
    ) -> bool {
        let mut changed = false;
        ui.horizontal(|ui| {
            for (tab, label, color) in [
                (ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE),
                (ToneCurveTab::Red, "R", egui::Color32::from_rgb(238, 84, 84)),
                (
                    ToneCurveTab::Green,
                    "G",
                    egui::Color32::from_rgb(92, 210, 116),
                ),
                (
                    ToneCurveTab::Blue,
                    "B",
                    egui::Color32::from_rgb(88, 150, 245),
                ),
            ] {
                ui.selectable_value(
                    selected_tab,
                    tab,
                    egui::RichText::new(label).color(color),
                );
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("Reset curve").clicked() {
                    match *selected_tab {
                        ToneCurveTab::Rgb => adjustment.tone_curve.reset(),
                        ToneCurveTab::Red => adjustment.tone_curve_red.reset(),
                        ToneCurveTab::Green => adjustment.tone_curve_green.reset(),
                        ToneCurveTab::Blue => adjustment.tone_curve_blue.reset(),
                    }
                    changed = true;
                }
            });
        });
        let (curve, color, description) = match *selected_tab {
            ToneCurveTab::Rgb => (
                &mut adjustment.tone_curve,
                egui::Color32::WHITE,
                "Composite luminance curve",
            ),
            ToneCurveTab::Red => (
                &mut adjustment.tone_curve_red,
                egui::Color32::from_rgb(238, 84, 84),
                "Red channel curve",
            ),
            ToneCurveTab::Green => (
                &mut adjustment.tone_curve_green,
                egui::Color32::from_rgb(92, 210, 116),
                "Green channel curve",
            ),
            ToneCurveTab::Blue => (
                &mut adjustment.tone_curve_blue,
                egui::Color32::from_rgb(88, 150, 245),
                "Blue channel curve",
            ),
        };
        ui.label(
            egui::RichText::new(description)
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        changed |= tone_curve_editor(ui, curve, color);
        if changed {
            adjustment.sanitize_tone_curves();
        }
        changed
    }

    fn show_local_mask_color_mixer(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        const COLORS: [&str; 8] = [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ];
        let mut changed = false;
        for (index, color) in COLORS.iter().enumerate() {
            ui.push_id(("local-hsl", index), |ui| {
                ui.strong(*color);
                changed |= adjustment_slider(
                    ui,
                    "Hue",
                    &mut adjustment.hsl_hue[index],
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Saturation",
                    &mut adjustment.hsl_saturation[index],
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
                changed |= adjustment_slider(
                    ui,
                    "Luminance",
                    &mut adjustment.hsl_luminance[index],
                    -100.0..=100.0,
                    0,
                    1.0,
                    None,
                );
            });
            if index + 1 < COLORS.len() {
                ui.separator();
            }
        }
        changed
    }

    fn show_local_mask_adjustments(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
        selected_tab: &mut ToneCurveTab,
        selected_grade_tab: &mut ColorGradeTab,
    ) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Light")
            .default_open(true)
            .show(ui, |ui| {
                changed |= Self::show_local_mask_light(ui, adjustment);
            });
        egui::CollapsingHeader::new("Color")
            .default_open(true)
            .show(ui, |ui| {
                changed |= Self::show_local_mask_color(ui, adjustment);
            });
        egui::CollapsingHeader::new("Effects")
            .default_open(true)
            .show(ui, |ui| {
                changed |= Self::show_local_mask_effects(ui, adjustment);
            });
        egui::CollapsingHeader::new("Color Grading")
            .default_open(false)
            .show(ui, |ui| {
                changed |=
                    Self::show_local_mask_color_grading(ui, adjustment, selected_grade_tab);
            });
        egui::CollapsingHeader::new("Tone Curve")
            .default_open(false)
            .show(ui, |ui| {
                changed |= Self::show_local_mask_tone_curve(ui, adjustment, selected_tab);
            });
        egui::CollapsingHeader::new("Color Mixer")
            .default_open(false)
            .show(ui, |ui| {
                changed |= Self::show_local_mask_color_mixer(ui, adjustment);
            });
        changed
    }

    fn show_placeholder(ui: &mut Ui, title: &str, message: &str) {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.heading(title);
            ui.add_space(6.0);
            ui.label(egui::RichText::new(message).color(ui.visuals().weak_text_color()));
        });
    }

    fn show_export(ui: &mut Ui, app: &mut AurawApp, frame: &eframe::Frame) {
        ui.heading("Export");
        ui.label(
            egui::RichText::new("PNG · sRGB · high-quality processing")
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
        );
        ui.add_space(6.0);

        let source_dimensions = app.loaded_raw.as_ref().map(|raw| (raw.width, raw.height));
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong("Image sizing");
            egui::ComboBox::from_label("Resize to fit")
                .selected_text(app.export_settings.resize_mode.label())
                .show_ui(ui, |ui| {
                    for mode in [
                        ExportResizeMode::Original,
                        ExportResizeMode::LongEdge,
                        ExportResizeMode::ShortEdge,
                        ExportResizeMode::Width,
                        ExportResizeMode::Height,
                        ExportResizeMode::Percentage,
                    ] {
                        ui.selectable_value(
                            &mut app.export_settings.resize_mode,
                            mode,
                            mode.label(),
                        );
                    }
                });

            match app.export_settings.resize_mode {
                ExportResizeMode::Original => {
                    ui.label("Exports the complete processed image.");
                }
                ExportResizeMode::Percentage => {
                    ui.horizontal(|ui| {
                        ui.label("Scale");
                        ui.add(
                            egui::DragValue::new(&mut app.export_settings.percentage)
                                .range(1.0..=400.0)
                                .speed(1.0)
                                .suffix("%"),
                        );
                    });
                }
                mode => {
                    ui.horizontal(|ui| {
                        ui.label(mode.label());
                        ui.add(
                            egui::DragValue::new(&mut app.export_settings.edge_or_dimension)
                                .range(64..=MAX_EXPORT_EDGE)
                                .speed(10.0)
                                .suffix(" px"),
                        );
                    });
                }
            }

            if app.export_settings.resize_mode != ExportResizeMode::Original {
                ui.checkbox(&mut app.export_settings.allow_upscale, "Allow upscaling")
                    .on_hover_text(
                        "Disabled by default to avoid enlarging beyond the source dimensions.",
                    );
            }

            if let Some((width, height)) = source_dimensions {
                match app.export_settings.checked_output_dimensions(width, height) {
                    Ok((output_width, output_height)) => {
                        ui.label(format!(
                            "Source: {width}×{height}  →  Export: {output_width}×{output_height}"
                        ));
                    }
                    Err(error) => {
                        ui.colored_label(egui::Color32::RED, error.to_string());
                    }
                }
            } else {
                ui.label("Open a RAW file to calculate export dimensions.");
            }
        });

        ui.add_space(6.0);
        ui.group(|ui| {
            ui.set_width(ui.available_width());
            ui.strong("Metadata");
            ui.checkbox(&mut app.export_settings.keep_metadata, "Keep metadata")
                .on_hover_text(
                    "Embeds available camera, source-file, original-size, software, and orientation metadata in the PNG.",
                );
        });

        ui.add_space(10.0);
        let dimensions_valid = source_dimensions.is_some_and(|(width, height)| {
            app.export_settings
                .checked_output_dimensions(width, height)
                .is_ok()
        });
        let button =
            egui::Button::new("Export PNG…").min_size(egui::vec2(ui.available_width(), 30.0));
        if ui
            .add_enabled(app.can_export() && dimensions_valid, button)
            .clicked()
        {
            app.export_png(frame);
        }
        if !app.can_export() {
            ui.label(
                egui::RichText::new(
                    "Export becomes available after a RAW image has finished loading.",
                )
                .small()
                .color(ui.visuals().weak_text_color()),
            );
        }
    }

    fn show_basic(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Light", true, foldable, |ui| {
            changed |= adjustment_slider(
                ui,
                "Exposure",
                &mut exposure.exposure,
                -5.0..=5.0,
                2,
                0.05,
                Some("Overall scene-linear brightness in exposure stops."),
            );
            changed |= adjustment_slider(
                ui,
                "Contrast",
                &mut exposure.contrast,
                -100.0..=100.0,
                0,
                1.0,
                Some("Expands or compresses tones around photographic middle gray."),
            );
            changed |= adjustment_slider(
                ui,
                "Highlights",
                &mut exposure.highlights,
                -100.0..=100.0,
                0,
                1.0,
                Some("Recovers or brightens the upper tonal range without hard clipping."),
            );
            changed |= adjustment_slider(
                ui,
                "Shadows",
                &mut exposure.shadows,
                -100.0..=100.0,
                0,
                1.0,
                Some("Opens or deepens the lower tonal range."),
            );
            changed |= adjustment_slider(
                ui,
                "Whites",
                &mut exposure.whites,
                -100.0..=100.0,
                0,
                1.0,
                Some("Moves the bright endpoint and specular range."),
            );
            changed |= adjustment_slider(
                ui,
                "Blacks",
                &mut exposure.blacks,
                -100.0..=100.0,
                0,
                1.0,
                Some("Moves the dark endpoint while preserving sensor black calibration."),
            );
        });
        changed
    }

    fn show_tone_curve(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        selected_tab: &mut ToneCurveTab,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Tone Curve", true, foldable, |ui| {
            ui.horizontal(|ui| {
                for (tab, label, color) in [
                    (ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE),
                    (ToneCurveTab::Red, "R", egui::Color32::from_rgb(238, 84, 84)),
                    (
                        ToneCurveTab::Green,
                        "G",
                        egui::Color32::from_rgb(92, 210, 116),
                    ),
                    (
                        ToneCurveTab::Blue,
                        "B",
                        egui::Color32::from_rgb(88, 150, 245),
                    ),
                ] {
                    let text = egui::RichText::new(label).color(color);
                    ui.selectable_value(selected_tab, tab, text);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Reset curve").clicked() {
                        match selected_tab {
                            ToneCurveTab::Rgb => exposure.tone_curve.reset(),
                            ToneCurveTab::Red => exposure.tone_curve_red.reset(),
                            ToneCurveTab::Green => exposure.tone_curve_green.reset(),
                            ToneCurveTab::Blue => exposure.tone_curve_blue.reset(),
                        }
                        changed = true;
                    }
                });
            });

            let (curve, color, description) = match selected_tab {
                ToneCurveTab::Rgb => (
                    &mut exposure.tone_curve,
                    egui::Color32::WHITE,
                    "Composite luminance curve",
                ),
                ToneCurveTab::Red => (
                    &mut exposure.tone_curve_red,
                    egui::Color32::from_rgb(238, 84, 84),
                    "Red channel curve",
                ),
                ToneCurveTab::Green => (
                    &mut exposure.tone_curve_green,
                    egui::Color32::from_rgb(92, 210, 116),
                    "Green channel curve",
                ),
                ToneCurveTab::Blue => (
                    &mut exposure.tone_curve_blue,
                    egui::Color32::from_rgb(88, 150, 245),
                    "Blue channel curve",
                ),
            };
            ui.label(
                egui::RichText::new(description)
                    .size(11.5)
                    .color(ui.visuals().weak_text_color()),
            );
            changed |= tone_curve_editor(ui, curve, color);
        });
        changed
    }

    fn show_color(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Color", true, foldable, |ui| {
            changed |= adjustment_slider(
                ui,
                "Temperature",
                &mut exposure.temperature,
                -100.0..=100.0,
                0,
                1.0,
                Some("Relative blue-yellow adaptation; zero preserves the camera as-shot white balance."),
            );
            changed |= adjustment_slider(
                ui,
                "Tint",
                &mut exposure.tint,
                -100.0..=100.0,
                0,
                1.0,
                Some("Relative green-magenta adaptation."),
            );
            changed |= adjustment_slider(
                ui,
                "Vibrance",
                &mut exposure.vibrance,
                -100.0..=100.0,
                0,
                1.0,
                Some("Perceptual colorfulness with protection for saturated colors and skin hues."),
            );
            changed |= adjustment_slider(
                ui,
                "Saturation",
                &mut exposure.saturation,
                -100.0..=100.0,
                0,
                1.0,
                Some("Uniform perceptual chroma scaling."),
            );
        });
        changed
    }

    fn show_color_grading(
        ui: &mut Ui,
        grading: &mut crate::pipeline::ColorGrading,
        selected_tab: &mut ColorGradeTab,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        let mut contents = |ui: &mut Ui| {
            ui.label(
                egui::RichText::new(
                    "Perceptual four-way grading in scene-linear Rec.2020",
                )
                .size(11.5)
                .color(ui.visuals().weak_text_color()),
            );
            changed |= color_grading_editor(ui, grading, selected_tab);
        };
        if foldable {
            egui::CollapsingHeader::new("Color Grading")
                .default_open(false)
                .show(ui, contents);
        } else {
            contents(ui);
        }
        changed
    }

    fn show_presence(
        ui: &mut Ui,
        exposure: &mut ExposureParams,
        expert_mode: bool,
        foldable: bool,
    ) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Effects", false, foldable, |ui| {
            changed |= adjustment_slider(
                ui,
                "Texture",
                &mut exposure.texture,
                -100.0..=100.0,
                0,
                1.0,
                Some("Enhances or softens fine surface detail without changing overall exposure."),
            );
            changed |= adjustment_slider(
                ui,
                "Clarity",
                &mut exposure.clarity,
                -100.0..=100.0,
                0,
                1.0,
                Some("Changes edge-aware midtone local contrast while protecting highlights and deep shadows."),
            );
            changed |= adjustment_slider(
                ui,
                "Dehaze",
                &mut exposure.dehaze,
                -100.0..=100.0,
                0,
                1.0,
                Some("Removes or adds atmospheric veil while preserving color relationships."),
            );

            ui.separator();
            ui.push_id("glow", |ui| {
                ui.strong("Glow");
                changed |= adjustment_slider(
                    ui,
                    "Amount",
                    &mut exposure.glow_amount,
                    0.0..=100.0,
                    0,
                    1.0,
                    Some("Softens and blooms bright light sources without lifting the entire image."),
                );
                if expert_mode {
                    ui.push_id("advanced-glow", |ui| {
                        changed |= adjustment_slider(
                            ui,
                            "Radius",
                            &mut exposure.glow_radius,
                            0.0..=100.0,
                            0,
                            1.0,
                            Some("Controls the spatial spread of the highlight bloom."),
                        );
                        changed |= adjustment_slider(
                            ui,
                            "Threshold",
                            &mut exposure.glow_threshold,
                            0.0..=100.0,
                            0,
                            1.0,
                            Some("Higher values restrict glow to brighter highlights."),
                        );
                    });
                }
            });

            ui.separator();
            ui.push_id("vignette", |ui| {
                ui.strong("Vignette");
                changed |= adjustment_slider(
                    ui,
                    "Amount",
                    &mut exposure.vignette_amount,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Darkens negative values or brightens positive values toward the image edges."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Midpoint",
                    &mut exposure.vignette_midpoint,
                    0.0..=100.0,
                    0,
                    1.0,
                    Some("Moves the vignette transition inward or confines it to the outermost edge."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Roundness",
                    &mut exposure.vignette_roundness,
                    -100.0..=100.0,
                    0,
                    1.0,
                    Some("Changes the vignette shape from frame-like to circular."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Feather",
                    &mut exposure.vignette_feather,
                    0.0..=100.0,
                    0,
                    1.0,
                    Some("Controls the softness of the vignette transition."),
                );
                changed |= adjustment_slider(
                    ui,
                    "Highlights",
                    &mut exposure.vignette_highlights,
                    0.0..=100.0,
                    0,
                    1.0,
                    Some("Restores bright edge highlights when using a dark vignette."),
                );
            });
        });
        changed
    }

    fn show_hsl(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        const COLORS: [&str; 8] = [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ];

        let mut changed = false;
        Self::adjustment_section(ui, "Color Mixer", false, foldable, |ui| {
            for (index, color) in COLORS.iter().enumerate() {
                ui.push_id(index, |ui| {
                    ui.strong(*color);
                    changed |= adjustment_slider(
                        ui,
                        "Hue",
                        &mut exposure.hsl_hue[index],
                        -100.0..=100.0,
                        0,
                        1.0,
                        None,
                    );
                    changed |= adjustment_slider(
                        ui,
                        "Saturation",
                        &mut exposure.hsl_saturation[index],
                        -100.0..=100.0,
                        0,
                        1.0,
                        None,
                    );
                    changed |= adjustment_slider(
                        ui,
                        "Luminance",
                        &mut exposure.hsl_luminance[index],
                        -100.0..=100.0,
                        0,
                        1.0,
                        None,
                    );
                });

                if index + 1 < COLORS.len() {
                    ui.separator();
                }
            }
        });
        changed
    }

    fn show_rendering(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Advanced Rendering", false, foldable, |ui| {
            ui.label(
                egui::RichText::new("darktable sigmoid view transform")
                    .strong()
                    .size(11.5),
            );
            changed |= adjustment_slider(
                ui,
                "View contrast",
                &mut exposure.sigmoid.contrast,
                0.1..=10.0,
                3,
                0.01,
                Some("Advanced darktable sigmoid slope; separate from the Lightroom-style Contrast slider."),
            );
            changed |= adjustment_slider(
                ui,
                "Skew",
                &mut exposure.sigmoid.skew,
                -1.0..=1.0,
                3,
                0.01,
                None,
            );
            changed |= adjustment_slider(
                ui,
                "Target white (%)",
                &mut exposure.sigmoid.display_white_target,
                20.0..=1600.0,
                1,
                1.0,
                None,
            );
            changed |= adjustment_slider(
                ui,
                "Target black (%)",
                &mut exposure.sigmoid.display_black_target,
                0.0..=15.0,
                4,
                0.0001,
                None,
            );

            let old_method = exposure.sigmoid.color_processing;
            egui::ComboBox::from_label("Color processing")
                .selected_text(exposure.sigmoid.color_processing.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut exposure.sigmoid.color_processing,
                        SigmoidColorProcessing::PerChannel,
                        SigmoidColorProcessing::PerChannel.label(),
                    );
                    ui.selectable_value(
                        &mut exposure.sigmoid.color_processing,
                        SigmoidColorProcessing::RgbRatio,
                        SigmoidColorProcessing::RgbRatio.label(),
                    );
                });
            changed |= old_method != exposure.sigmoid.color_processing;

            if exposure.sigmoid.color_processing == SigmoidColorProcessing::PerChannel {
                changed |= adjustment_slider(
                    ui,
                    "Preserve hue (%)",
                    &mut exposure.sigmoid.hue_preservation,
                    0.0..=100.0,
                    1,
                    1.0,
                    None,
                );
            }
        });
        changed
    }

    fn show_raw(ui: &mut Ui, exposure: &mut ExposureParams, foldable: bool) -> bool {
        let mut changed = false;
        Self::adjustment_section(ui, "Raw", false, foldable, |ui| {
            changed |= adjustment_slider(
                ui,
                "Raw Black Point",
                &mut exposure.black_point,
                -0.25..=0.25,
                3,
                0.01,
                None,
            );
            let previous_mode = exposure.demosaic_mode;
            egui::ComboBox::from_label("Demosaic")
                .selected_text(exposure.demosaic_mode.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(
                        &mut exposure.demosaic_mode,
                        DemosaicMode::Reference,
                        DemosaicMode::Reference.label(),
                    );
                    ui.selectable_value(
                        &mut exposure.demosaic_mode,
                        DemosaicMode::FrequencyDomainChroma,
                        DemosaicMode::FrequencyDomainChroma.label(),
                    );
                    ui.selectable_value(
                        &mut exposure.demosaic_mode,
                        DemosaicMode::Dual,
                        DemosaicMode::Dual.label(),
                    );
                });
            changed |= previous_mode != exposure.demosaic_mode;

            changed |= adjustment_slider(
                ui,
                "Chroma Denoise",
                &mut exposure.chroma_denoise,
                0.0..=1.0,
                2,
                0.01,
                None,
            );
            if exposure.demosaic_mode == DemosaicMode::FrequencyDomainChroma {
                changed |= adjustment_slider(
                    ui,
                    "Frequency Chroma",
                    &mut exposure.frequency_chroma,
                    0.0..=1.0,
                    2,
                    0.01,
                    None,
                );
            }
            if exposure.demosaic_mode == DemosaicMode::Dual {
                changed |= adjustment_slider(
                    ui,
                    "Dual Detail Threshold",
                    &mut exposure.dual_threshold,
                    0.0..=100.0,
                    1,
                    1.0,
                    None,
                );
            }
            changed |= adjustment_slider(
                ui,
                "Red CA",
                &mut exposure.ca_red,
                -2.0..=2.0,
                2,
                0.01,
                None,
            );
            changed |= adjustment_slider(
                ui,
                "Blue CA",
                &mut exposure.ca_blue,
                -2.0..=2.0,
                2,
                0.01,
                None,
            );
        });
        changed
    }
}
