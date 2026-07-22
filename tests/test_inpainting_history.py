from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
HISTORY = (ROOT / "src/app/edit_history.rs").read_text(encoding="utf-8")
INPAINT = (ROOT / "src/app/inpainting.rs").read_text(encoding="utf-8")


def test_inpainting_is_part_of_unified_undo_redo_snapshots() -> None:
    assert "inpainting: Arc<Vec<InpaintStroke>>" in HISTORY
    assert "fn undo_with_inpainting(" in HISTORY
    assert "fn redo_with_inpainting(" in HISTORY
    assert "let inpainting_changed = !Arc::ptr_eq(&target.inpainting, &self.current.inpainting);" in HISTORY
    assert "self.inpaint_strokes = snapshot.materialize_inpainting();" in HISTORY
    assert "self.rebuild_inpaint_layer();" in HISTORY


def test_inpainting_add_delete_and_clear_signal_edit_history() -> None:
    assert INPAINT.count("self.note_inpainting_edit_changed();") >= 3
    assert "self.inpaint_strokes.push(stroke);" in INPAINT
    assert "self.inpaint_strokes.remove(index);" in INPAINT
    assert "self.inpaint_strokes.clear();" in INPAINT


def test_history_observer_tracks_inpainting_without_cloning_unrelated_snapshots() -> None:
    assert "inpainting_contents_match" in HISTORY
    assert "Arc::clone(&self.inpainting)" in HISTORY
    assert "self.current.inpainting.as_slice() == inpainting" in HISTORY
