from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
EFRAME = (ROOT / "src/app/eframe_impl.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")


def test_native_preview_textures_are_retired_until_the_next_frame() -> None:
    assert "retired_egui_textures: Vec<egui::TextureId>" in APP
    assert "fn retire_egui_texture" in APP
    assert "fn release_retired_egui_textures" in APP
    assert "std::mem::take(&mut self.retired_egui_textures)" in APP


def test_retired_textures_are_flushed_before_any_current_frame_ui() -> None:
    ui_start = EFRAME.index("fn ui(&mut self")
    flush = EFRAME.index("self.release_retired_egui_textures(frame);", ui_start)
    first_panel = EFRAME.index("egui::Panel::", ui_start)
    central_panel = EFRAME.index("egui::CentralPanel::", ui_start)
    assert flush < first_panel
    assert flush < central_panel


def test_processing_never_frees_native_preview_textures_immediately() -> None:
    assert ".free_texture(&texture_id)" not in PROCESSING
    assert "self.retire_egui_texture(texture_id);" in PROCESSING
