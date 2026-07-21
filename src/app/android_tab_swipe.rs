use super::{AppTab, AurawApp, SidebarTab};
use eframe::egui::{self, Pos2, Rect};

const HORIZONTAL_INTENT_POINTS: f32 = 24.0;
const HORIZONTAL_DOMINANCE: f32 = 1.30;
const MIN_SWIPE_POINTS: f32 = 72.0;
const MAX_SWIPE_POINTS: f32 = 160.0;
const VIEWPORT_SWIPE_FRACTION: f32 = 0.18;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AndroidTabSwipe {
    active: bool,
    start: Pos2,
    current: Pos2,
    starting_tab: AppTab,
    blocked: bool,
    horizontal_intent: bool,
    preview_zoom: f32,
    preview_center: [f32; 2],
}

impl Default for AndroidTabSwipe {
    fn default() -> Self {
        Self {
            active: false,
            start: Pos2::ZERO,
            current: Pos2::ZERO,
            starting_tab: AppTab::Library,
            blocked: false,
            horizontal_intent: false,
            preview_zoom: 1.0,
            preview_center: [0.5, 0.5],
        }
    }
}

impl AndroidTabSwipe {
    fn reset(&mut self) {
        *self = Self::default();
    }
}

impl AurawApp {
    pub(crate) fn prepare_android_tab_swipe_frame(&mut self) {
        self.tab_swipe_surface_id = None;
    }

    pub(crate) fn finish_android_tab_swipe_frame(
        &mut self,
        ctx: &egui::Context,
        content_rect: Rect,
    ) {
        let dragged_id = ctx.dragged_id();
        let popup_open = ctx.any_popup_open();
        let (pressed, down, released, pointer_pos, any_touches, multi_touch) = ctx.input(|input| {
            (
                input.pointer.primary_pressed(),
                input.pointer.primary_down(),
                input.pointer.primary_released(),
                input.pointer.interact_pos(),
                input.any_touches(),
                input.multi_touch().is_some(),
            )
        });

        if pressed && any_touches {
            if let Some(position) = pointer_pos {
                self.android_tab_swipe = AndroidTabSwipe {
                    active: true,
                    start: position,
                    current: position,
                    starting_tab: self.active_tab,
                    blocked: !content_rect.contains(position),
                    horizontal_intent: false,
                    preview_zoom: self.preview_zoom,
                    preview_center: self.preview_center,
                };
            }
        }

        if !self.android_tab_swipe.active {
            return;
        }

        if let Some(position) = pointer_pos {
            self.android_tab_swipe.current = position;
        }

        let delta = self.android_tab_swipe.current - self.android_tab_swipe.start;
        if !self.android_tab_swipe.horizontal_intent {
            if delta.x.abs() >= HORIZONTAL_INTENT_POINTS
                && delta.x.abs() >= delta.y.abs() * HORIZONTAL_DOMINANCE
            {
                self.android_tab_swipe.horizontal_intent = true;
            } else if delta.y.abs() >= HORIZONTAL_INTENT_POINTS && delta.y.abs() > delta.x.abs() {
                self.android_tab_swipe.blocked = true;
            }
        }

        let editing_mask = self.active_tab == AppTab::Develop
            && (self.mask_drag.is_some()
                || self.preview_touch_navigation_active
                || self
                    .android_original_hold
                    .is_some_and(|hold| hold.showing_original)
                || (self.sidebar_tab == SidebarTab::Masks && self.active_mask_tool.is_some()));
        // At any zoom above fit, horizontal one-finger drags belong to preview
        // panning and must never be promoted into page navigation.
        let zoomed_preview = zoomed_preview_blocks_tab_swipe(
            self.active_tab,
            self.preview_zoom,
            self.tab_swipe_surface_id.is_some(),
        );
        let captured_by_control =
            dragged_id.is_some_and(|id| Some(id) != self.tab_swipe_surface_id);
        if multi_touch
            || popup_open
            || crate::ui::components::adjustment_slider::slider_scroll_locked(ctx)
            || editing_mask
            || zoomed_preview
            || captured_by_control
            || self.active_tab != self.android_tab_swipe.starting_tab
        {
            self.android_tab_swipe.blocked = true;
        }

        if released || (!down && !any_touches) {
            let swipe = self.android_tab_swipe;
            self.android_tab_swipe.reset();

            let threshold = (content_rect.width() * VIEWPORT_SWIPE_FRACTION)
                .clamp(MIN_SWIPE_POINTS, MAX_SWIPE_POINTS);
            if swipe.blocked
                || !swipe.horizontal_intent
                || delta.x.abs() < threshold
                || delta.x.abs() < delta.y.abs() * HORIZONTAL_DOMINANCE
            {
                return;
            }

            let destination = if delta.x < 0.0 {
                next_tab(swipe.starting_tab)
            } else {
                previous_tab(swipe.starting_tab)
            };
            let Some(destination) = destination else {
                return;
            };

            // A recognized page swipe should not leave a one-finger preview pan
            // behind when the user later returns to Develop.
            if swipe.starting_tab == AppTab::Develop {
                self.preview_zoom = swipe.preview_zoom;
                self.preview_center = swipe.preview_center;
            }
            self.activate_tab(destination);
            ctx.request_repaint();
        }
    }
}

fn zoomed_preview_blocks_tab_swipe(
    active_tab: AppTab,
    preview_zoom: f32,
    has_preview_swipe_surface: bool,
) -> bool {
    active_tab == AppTab::Develop && has_preview_swipe_surface && preview_zoom > 1.0
}

fn next_tab(tab: AppTab) -> Option<AppTab> {
    match tab {
        AppTab::Library => Some(AppTab::Develop),
        AppTab::Develop => Some(AppTab::Settings),
        AppTab::Settings => None,
    }
}

fn previous_tab(tab: AppTab) -> Option<AppTab> {
    match tab {
        AppTab::Library => None,
        AppTab::Develop => Some(AppTab::Library),
        AppTab::Settings => Some(AppTab::Develop),
    }
}

#[cfg(test)]
mod tests {
    use super::{next_tab, previous_tab, zoomed_preview_blocks_tab_swipe, AppTab};

    #[test]
    fn tab_order_matches_android_page_order() {
        assert_eq!(next_tab(AppTab::Library), Some(AppTab::Develop));
        assert_eq!(next_tab(AppTab::Develop), Some(AppTab::Settings));
        assert_eq!(next_tab(AppTab::Settings), None);
        assert_eq!(previous_tab(AppTab::Settings), Some(AppTab::Develop));
        assert_eq!(previous_tab(AppTab::Develop), Some(AppTab::Library));
        assert_eq!(previous_tab(AppTab::Library), None);
    }

    #[test]
    fn any_zoom_above_fit_blocks_preview_tab_swipes() {
        assert!(!zoomed_preview_blocks_tab_swipe(AppTab::Develop, 1.0, true,));
        assert!(zoomed_preview_blocks_tab_swipe(
            AppTab::Develop,
            1.000_001,
            true,
        ));
        assert!(!zoomed_preview_blocks_tab_swipe(AppTab::Library, 2.0, true,));
        assert!(!zoomed_preview_blocks_tab_swipe(
            AppTab::Develop,
            2.0,
            false,
        ));
    }
}
