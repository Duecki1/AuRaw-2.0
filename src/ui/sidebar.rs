use crate::app::{AurawApp, SidebarTab, ToneCurveTab};
use crate::pipeline::{
    BrushMode, DemosaicMode, ExportResizeMode, ExposureParams, MaskCombineMode, MaskGeometry,
    MaskKind, SigmoidColorProcessing, MAX_LOCAL_MASKS,
};
use crate::ui::components::adjustment_slider::adjustment_slider;
use crate::ui::components::tone_curve_editor::tone_curve_editor;
use crate::ui::layout::ScreenLayout;
use eframe::egui::{self, Ui};

pub struct Sidebar;

#[derive(Clone, Copy, Debug)]
enum MaskDragPayload {
    Group(usize),
    Component { mask: usize, component: usize },
}

impl Sidebar {
    const SCROLLBAR_GUTTER: f32 = 18.0;

    pub fn show(
        ui: &mut Ui,
        app: &mut AurawApp,
        _layout: ScreenLayout,
        frame: &eframe::Frame,
    ) {
        let content_width = (ui.available_width() - Self::SCROLLBAR_GUTTER).max(220.0);
        ui.set_width(content_width);
        ui.set_max_width(content_width);
        ui.spacing_mut().item_spacing = egui::vec2(6.0, 3.0);

        ui.horizontal_wrapped(|ui| {
            for (tab, label) in [
                (SidebarTab::Adjustments, "Adjustments"),
                (SidebarTab::Masks, "Masks"),
                (SidebarTab::Inpainting, "Inpainting"),
                (SidebarTab::Export, "Export"),
            ] {
                ui.selectable_value(&mut app.sidebar_tab, tab, label);
            }
        });
        ui.add_space(2.0);
        ui.separator();

        match app.sidebar_tab {
            SidebarTab::Adjustments => Self::show_adjustments(ui, app),
            SidebarTab::Masks => Self::show_masks(ui, app),
            SidebarTab::Inpainting => Self::show_placeholder(
                ui,
                "Inpainting",
                "Healing, object removal, and generative inpainting controls are coming later.",
            ),
            SidebarTab::Export => Self::show_export(ui, app, frame),
        }
    }

    fn show_adjustments(ui: &mut Ui, app: &mut AurawApp) {
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
        changed |= Self::show_basic(ui, &mut app.exposure);
        changed |= Self::show_tone_curve(ui, &mut app.exposure, &mut app.tone_curve_tab);
        changed |= Self::show_color(ui, &mut app.exposure);
        changed |= Self::show_presence(ui, &mut app.exposure, app.expert_mode);
        changed |= Self::show_hsl(ui, &mut app.exposure);
        if app.expert_mode {
            changed |= Self::show_rendering(ui, &mut app.exposure);
            changed |= Self::show_raw(ui, &mut app.exposure);
        }

        if changed {
            app.exposure.sanitize_tone_curves();
            app.mark_pipeline_dirty();
        }
    }

    fn show_masks(ui: &mut Ui, app: &mut AurawApp) {
        ui.heading("Masking Groups");
        ui.add_space(4.0);

        let mut new_mask = None;
        ui.add_enabled_ui(app.masks.masks.len() < MAX_LOCAL_MASKS, |ui| {
            ui.menu_button("Create Mask", |ui| {
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
                        .on_disabled_hover_text(
                            "This mask type is planned but not implemented yet.",
                        )
                        .clicked()
                    {
                        new_mask = Some(kind);
                        ui.close();
                    }
                }
            });
        });
        if let Some(kind) = new_mask {
            if let Some((mask_index, _)) = app.masks.add_mask(kind) {
                app.activate_mask_tool(kind);
                app.mark_mask_geometry_dirty(mask_index);
            }
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(4.0);

        if app.masks.masks.is_empty() {
            ui.add_space(16.0);
            ui.vertical_centered(|ui| {
                ui.weak("No masks created yet.");
                ui.weak("Create a mask to apply local adjustments.");
            });
            return;
        }

        let mut select_mask = None;
        let mut remove_mask = None;
        let mut move_mask = None;
        let mut enabled_changed = false;
        egui::Frame::new()
            .fill(ui.visuals().widgets.noninteractive.bg_fill)
            .stroke(ui.visuals().widgets.noninteractive.fg_stroke)
            .corner_radius(4.0)
            .inner_margin(4.0)
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                for index in (0..app.masks.masks.len()).rev() {
                    let selected = app.masks.selected_mask == Some(index);
                    let row = ui.dnd_drag_source(
                        ui.id().with(("mask-row", index)),
                        MaskDragPayload::Group(index),
                        |ui| {
                            ui.horizontal(|ui| {
                                let mask = &mut app.masks.masks[index];
                                let visibility = if mask.enabled { "On" } else { "Off" };
                                if ui.selectable_label(mask.enabled, visibility).clicked() {
                                    mask.enabled = !mask.enabled;
                                    enabled_changed = true;
                                }
                                if ui
                                    .selectable_label(
                                        selected,
                                        egui::RichText::new(format!(
                                            "{}  ·  {}",
                                            mask.name,
                                            mask.components.len()
                                        ))
                                        .strong(),
                                    )
                                    .clicked()
                                {
                                    select_mask = Some(index);
                                }
                                ui.with_layout(
                                    egui::Layout::right_to_left(egui::Align::Center),
                                    |ui| {
                                        if ui
                                            .small_button("Delete")
                                            .on_hover_text("Delete mask group")
                                            .clicked()
                                        {
                                            remove_mask = Some(index);
                                        }
                                    },
                                );
                            })
                        },
                    );
                    if let Some(payload) = row.response.dnd_release_payload::<MaskDragPayload>() {
                        if let MaskDragPayload::Group(from) = *payload {
                            move_mask = Some((from, index));
                        }
                    }
                    if index > 0 {
                        ui.separator();
                    }
                }
            });
        if enabled_changed {
            app.mark_mask_adjustments_dirty();
        }
        if let Some(index) = select_mask {
            app.masks.selected_mask = Some(index);
            app.masks.selected_component = Some(0);
            if let Some(kind) = app
                .masks
                .selected_component()
                .map(|component| component.kind)
            {
                app.select_mask_tool(kind);
            }
        }
        if let Some((from, to)) = move_mask {
            if app.masks.move_mask(from, to) {
                app.mark_all_mask_layers_dirty();
            }
        }
        if let Some(index) = remove_mask {
            app.masks.selected_mask = Some(index);
            app.masks.remove_selected_mask();
            app.mark_all_mask_layers_dirty();
            if let Some(kind) = app
                .masks
                .selected_component()
                .map(|component| component.kind)
            {
                app.select_mask_tool(kind);
            } else {
                app.active_mask_tool = None;
            }
        }

        let Some(mask_index) = app.masks.selected_mask else {
            return;
        };
        if mask_index >= app.masks.masks.len() {
            return;
        }

        ui.add_space(8.0);
        ui.separator();
        ui.add_space(8.0);
        let mut geometry_changed = false;
        let mut adjustments_changed = false;
        let mut remove_component = None;
        let mut move_component = None;
        let mut add_component = None;
        let selected_component_before = app.masks.selected_component;
        let mut selected_component_choice = None;
        let mut brush_mode = app.brush_mode;

        {
            let mask = &mut app.masks.masks[mask_index];
            ui.label(egui::RichText::new(format!("Sub-Masks of {}", mask.name)).strong());
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                for combine in [
                    MaskCombineMode::Add,
                    MaskCombineMode::Subtract,
                    MaskCombineMode::Intersect,
                ] {
                    ui.menu_button(combine.label(), |ui| {
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
                                .on_disabled_hover_text(
                                    "This sub-mask type is planned but not implemented yet.",
                                )
                                .clicked()
                            {
                                add_component = Some((kind, combine));
                                ui.close();
                            }
                        }
                    });
                }
            });

            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label("Name");
                ui.text_edit_singleline(&mut mask.name);
            });
            geometry_changed |= adjustment_slider(
                ui,
                "Mask opacity",
                &mut mask.opacity,
                0.0..=1.0,
                2,
                0.01,
                Some("Controls the strength of the entire mask before local adjustments."),
            );

            ui.add_space(4.0);
            egui::Frame::new()
                .fill(ui.visuals().widgets.noninteractive.bg_fill)
                .stroke(ui.visuals().widgets.noninteractive.fg_stroke)
                .corner_radius(2.0)
                .inner_margin(4.0)
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    for component_index in (0..mask.components.len()).rev() {
                        let selected = selected_component_before == Some(component_index);
                        let can_delete = mask.components.len() > 1;
                        let component = &mut mask.components[component_index];
                        let badge = if component_index == 0 {
                            "_"
                        } else {
                            match component.combine {
                                MaskCombineMode::Add => "+",
                                MaskCombineMode::Subtract => "−",
                                MaskCombineMode::Intersect => "/",
                            }
                        };
                        let row = ui.dnd_drag_source(
                            ui.id().with(("submask-row", mask_index, component_index)),
                            MaskDragPayload::Component {
                                mask: mask_index,
                                component: component_index,
                            },
                            |ui| {
                                ui.horizontal(|ui| {
                                    let visibility = if component.enabled { "On" } else { "Off" };
                                    if ui.selectable_label(component.enabled, visibility).clicked()
                                    {
                                        component.enabled = !component.enabled;
                                        geometry_changed = true;
                                    }
                                    ui.label(egui::RichText::new(badge).strong());
                                    if ui
                                        .selectable_label(selected, component.kind.label())
                                        .clicked()
                                    {
                                        selected_component_choice = Some(component_index);
                                    }
                                    ui.with_layout(
                                        egui::Layout::right_to_left(egui::Align::Center),
                                        |ui| {
                                            if can_delete
                                                && ui
                                                    .small_button("Delete")
                                                    .on_hover_text("Delete sub-mask")
                                                    .clicked()
                                            {
                                                remove_component = Some(component_index);
                                            }
                                        },
                                    );
                                })
                            },
                        );
                        if let Some(payload) = row.response.dnd_release_payload::<MaskDragPayload>()
                        {
                            if let MaskDragPayload::Component {
                                mask: source_mask,
                                component: from,
                            } = *payload
                            {
                                if source_mask == mask_index {
                                    move_component = Some((from, component_index));
                                }
                            }
                        }
                        if component_index > 0 {
                            ui.separator();
                        }
                    }
                });
        }

        if let Some(component_index) = selected_component_choice {
            app.masks.selected_component = Some(component_index);
            if let Some(kind) = app
                .masks
                .selected_component()
                .map(|component| component.kind)
            {
                app.select_mask_tool(kind);
            }
        }

        if let Some((kind, combine)) = add_component {
            if app.masks.add_component(kind, combine).is_some() {
                app.activate_mask_tool(kind);
                geometry_changed = true;
            }
        }

        if let Some((from, to)) = move_component {
            geometry_changed |= app.masks.move_component(from, to);
        }

        if let Some(component_index) = remove_component {
            app.masks.selected_component = Some(component_index);
            if app.masks.remove_selected_component().is_some() {
                geometry_changed = true;
                if let Some(kind) = app
                    .masks
                    .selected_component()
                    .map(|component| component.kind)
                {
                    app.select_mask_tool(kind);
                }
            }
        }

        let component_index = app.masks.selected_component.unwrap_or(0).min(
            app.masks.masks[mask_index]
                .components
                .len()
                .saturating_sub(1),
        );
        app.masks.selected_component = Some(component_index);

        {
            let mask = &mut app.masks.masks[mask_index];
            if let Some(component) = mask.components.get_mut(component_index) {
                ui.add_space(6.0);
                ui.separator();
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("Spatial Settings: {}", component.kind.label()))
                        .weak(),
                );
                ui.group(|ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.strong(component.kind.label());
                        geometry_changed |= ui.checkbox(&mut component.invert, "Invert").changed();
                    });
                    if component_index > 0 {
                        let before = component.combine;
                        egui::ComboBox::from_label("Combine")
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

                    match &mut component.geometry {
                        MaskGeometry::Brush {
                            size,
                            feather,
                            dabs,
                        } => {
                            ui.horizontal(|ui| {
                                ui.selectable_value(&mut brush_mode, BrushMode::Paint, "Brush");
                                ui.selectable_value(&mut brush_mode, BrushMode::Erase, "Eraser");
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
                            ui.horizontal(|ui| {
                                if ui.small_button("Clear strokes").clicked() {
                                    dabs.clear();
                                    geometry_changed = true;
                                }
                            });
                            ui.label(format!("{} brush dabs", dabs.len()));
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
                        MaskGeometry::Placeholder => {
                            ui.label("This mask type is not implemented yet.");
                        }
                    }
                });
            }

            ui.add_space(6.0);
            ui.separator();
            ui.horizontal(|ui| {
                ui.strong("Local adjustments");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Reset adjustments").clicked() {
                        mask.adjustments.reset();
                        adjustments_changed = true;
                    }
                });
            });
            adjustments_changed |= Self::show_local_mask_adjustments(ui, &mut mask.adjustments);
        }

        app.brush_mode = brush_mode;
        if geometry_changed {
            app.mask_properties_active = true;
            app.mark_mask_geometry_dirty(mask_index);
        }
        if adjustments_changed {
            app.mark_mask_adjustments_dirty();
        }
    }

    fn show_local_mask_adjustments(
        ui: &mut Ui,
        adjustment: &mut crate::pipeline::LocalAdjustments,
    ) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Light")
            .default_open(true)
            .show(ui, |ui| {
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
            });
        egui::CollapsingHeader::new("Color")
            .default_open(true)
            .show(ui, |ui| {
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
            });
        egui::CollapsingHeader::new("Effects")
            .default_open(true)
            .show(ui, |ui| {
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
            });
        changed
    }

    fn show_placeholder(ui: &mut Ui, title: &str, message: &str) {
        ui.add_space(12.0);
        ui.vertical_centered(|ui| {
            ui.heading(title);
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(message)
                    .color(ui.visuals().weak_text_color()),
            );
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

        let source_dimensions = app
            .loaded_raw
            .as_ref()
            .map(|raw| (raw.width, raw.height));
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
                                .range(64..=65_535)
                                .speed(10.0)
                                .suffix(" px"),
                        );
                    });
                }
            }

            if app.export_settings.resize_mode != ExportResizeMode::Original {
                ui.checkbox(&mut app.export_settings.allow_upscale, "Allow upscaling")
                    .on_hover_text("Disabled by default to avoid enlarging beyond the source dimensions.");
            }

            if let Some((width, height)) = source_dimensions {
                let (output_width, output_height) =
                    app.export_settings.output_dimensions(width, height);
                ui.label(format!(
                    "Source: {width}×{height}  →  Export: {output_width}×{output_height}"
                ));
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
        let button = egui::Button::new("Export PNG…").min_size(egui::vec2(ui.available_width(), 30.0));
        if ui.add_enabled(app.can_export(), button).clicked() {
            app.export_png(frame);
        }
        if !app.can_export() {
            ui.label(
                egui::RichText::new("Export becomes available after a RAW image has finished loading.")
                    .small()
                    .color(ui.visuals().weak_text_color()),
            );
        }
    }

    fn show_basic(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Light")
            .default_open(true)
            .show(ui, |ui| {
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
    ) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Tone Curve")
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (tab, label, color) in [
                        (ToneCurveTab::Rgb, "RGB", egui::Color32::WHITE),
                        (ToneCurveTab::Red, "R", egui::Color32::from_rgb(238, 84, 84)),
                        (ToneCurveTab::Green, "G", egui::Color32::from_rgb(92, 210, 116)),
                        (ToneCurveTab::Blue, "B", egui::Color32::from_rgb(88, 150, 245)),
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

    fn show_color(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Color")
            .default_open(true)
            .show(ui, |ui| {
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

    fn show_presence(ui: &mut Ui, exposure: &mut ExposureParams, expert_mode: bool) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Effects")
            .default_open(false)
            .show(ui, |ui| {
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

    fn show_hsl(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        const COLORS: [&str; 8] = [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ];

        let mut changed = false;
        egui::CollapsingHeader::new("Color Mixer")
            .default_open(false)
            .show(ui, |ui| {
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

    fn show_rendering(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Advanced Rendering")
            .default_open(false)
            .show(ui, |ui| {
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

    fn show_raw(ui: &mut Ui, exposure: &mut ExposureParams) -> bool {
        let mut changed = false;
        egui::CollapsingHeader::new("Raw")
            .default_open(false)
            .show(ui, |ui| {
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
