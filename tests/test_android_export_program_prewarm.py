from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
LIFECYCLE = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
EXPORT = (ROOT / "src/pipeline/export.rs").read_text(encoding="utf-8")
GPU = (ROOT / "src/pipeline/gpu.rs").read_text(encoding="utf-8")


def test_android_startup_prewarms_and_persists_high_quality_export_programs() -> None:
    assert "GpuProgramPrewarm" in APP
    assert "prewarm_export_program_template_with_cache" in LIFECYCLE
    assert LIFECYCLE.index("prewarm_export_program_template_with_cache") < LIFECYCLE.index(
        "cache.persist()"
    )
    assert "ProcessingQuality::High" in GPU
    assert ".map(Self::into_program_template)" in GPU


def test_tiled_export_reuses_startup_precompiled_program_handles() -> None:
    assert "await_export_program_template" in EXPORT
    assert "new_headless_reusing_program_template_with_mask_edge" in EXPORT
    assert "template.pipelines[program_index].clone()" in GPU
    assert "Full-quality export reused startup-precompiled GPU programs" in EXPORT
