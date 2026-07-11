use eframe::egui::{self, Ui};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SidebarTab {
    #[default]
    General,
    Masks,
    Inpainting,
    Export,
}

impl SidebarTab {
    const ALL: [Self; 4] = [Self::General, Self::Masks, Self::Inpainting, Self::Export];

    fn label(self) -> &'static str {
        match self {
            Self::General => "General",
            Self::Masks => "Masks",
            Self::Inpainting => "Inpainting",
            Self::Export => "Export",
        }
    }
}

pub struct SidebarTabBar;

impl SidebarTabBar {
    pub fn show(ui: &mut Ui, active: &mut SidebarTab) {
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            for tab in SidebarTab::ALL {
                ui.selectable_value(active, tab, tab.label());
            }
        });
    }
}
