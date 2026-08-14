use super::*;

pub(super) fn mask_component_badge(component_index: usize, combine: MaskCombineMode) -> &'static str {
    if component_index == 0 {
        "BASE"
    } else {
        match combine {
            MaskCombineMode::Add => egui_phosphor::regular::PLUS,
            MaskCombineMode::Subtract => egui_phosphor::regular::MINUS,
            MaskCombineMode::Intersect => egui_phosphor::regular::INTERSECT,
        }
    }
}

pub(super) fn mask_creation_icon() -> &'static str {
    egui_phosphor::regular::PLUS
}

fn mask_strip_scroll_source() -> egui::scroll_area::ScrollSource {
    if cfg!(target_os = "android") {
        // Force content-drag scrolling for touch and stylus input. Card widgets
        // intentionally use click-only sense on Android so they cannot steal it.
        egui::scroll_area::ScrollSource::ALL
    } else {
        egui::scroll_area::ScrollSource::default()
    }
}

#[derive(Clone, Debug)]
enum MaskRenameTarget {
    Group(usize),
    Component {
        mask_index: usize,
        component_index: usize,
    },
}

#[derive(Clone, Debug)]
struct MaskRenameDialog {
    target: MaskRenameTarget,
    name: String,
    request_focus: bool,
}

#[derive(Clone)]
struct SubmaskDragState {
    source_mask: usize,
    source_component: usize,
    source_texture: Option<egui::TextureHandle>,
    source_name: String,
    source_badge: String,
    source_enabled: bool,
    hover_group: Option<(usize, std::time::Instant)>,
    drop_target: Option<(usize, usize)>,
    target_loss_started: Option<std::time::Instant>,
}

mod adjustments;
mod details;
mod menus;
mod properties;
mod strip;
mod thumbnails;
