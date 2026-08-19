use super::*;

impl InpaintState {
    pub(crate) fn reset_for_document(&mut self) {
        self.source_anchor = None;
        self.source_pick_active = false;
        self.active_dab_count = 0;
        self.strokes.clear();
        self.hovered_stroke = None;
        self.selected_stroke = None;
    }
}

impl AurawApp {
    pub(crate) fn reset_inpainting_state(&mut self) {
        self.inpaint.reset_for_document();
    }

    pub(crate) fn clear_inpainting_tool(&mut self, kind: UiInpaintTool) {
        self.inpaint.active_dab_count = 0;
        self.inpaint.strokes.retain(|stroke| stroke.kind != kind);
        self.inpaint.hovered_stroke = None;
        self.inpaint.selected_stroke = None;
        self.egui_ctx.request_repaint();
    }

    pub(crate) fn delete_inpaint_stroke(&mut self, index: usize) {
        if index >= self.inpaint.strokes.len() {
            return;
        }
        self.inpaint.strokes.remove(index);
        self.inpaint.hovered_stroke = None;
        self.inpaint.selected_stroke = None;
        self.egui_ctx.request_repaint();
    }
}
