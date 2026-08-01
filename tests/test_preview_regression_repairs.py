from __future__ import annotations

import math
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
APP = (ROOT / "src/app.rs").read_text(encoding="utf-8")
PROCESSING = (ROOT / "src/app/processing_export.rs").read_text(encoding="utf-8")
RESOURCES = (ROOT / "src/pipeline/gpu/resources.rs").read_text(encoding="utf-8")

MIB = 1024 * 1024


def preview_persistent_bytes(
    width: int, height: int, mask_edge: int, tone_scale: int = 8
) -> int:
    """Analytical preview reference for resources.rs persistent entries."""
    pixels = width * height
    # CFA R16 + color R8 + black R32 + reconstructed R32 + six RGBA16 work
    # textures + RGBA8 output + RGBA16 inpaint.
    full_frame = pixels * (2 + 1 + 4 + 4 + 6 * 8 + 4 + 8)
    tone_width = math.ceil(width / tone_scale)
    tone_height = math.ceil(height / tone_scale)
    tone_guides = 2 * tone_width * tone_height * 4
    mask_atlas = mask_edge * mask_edge * 32 * 2
    # Profile/params/histogram/stats are small and alignment-dependent. One MiB
    # per pipeline is deliberately conservative for this admission reference.
    fixed_buffers = MIB
    return full_frame + tone_guides + mask_atlas + fixed_buffers


def test_global_gpu_reservation_counts_resident_not_repeated_transient_peaks() -> None:
    acquire = RESOURCES[
        RESOURCES.index("impl GpuBudgetReservation") : RESOURCES.index(
            "impl Drop for GpuBudgetReservation"
        )
    ]
    assert "validate_gpu_resource_plan(plan, limit)?" in acquire
    assert "let bytes = plan.persistent_gpu_bytes;" in acquire
    assert "reserve_gpu_bytes(&RESERVED_GPU_BYTES, limit, plan.admitted_gpu_bytes)" not in acquire


def test_android_zoom_working_set_fits_resident_budget() -> None:
    assert "Self::Low => 0.50" in APP
    assert "Self::Medium => 0.67" in APP
    assert "Self::High => 0.84" in APP
    assert "Self::Max => 1.00" in APP
    assert "cfg!(target_os = \"android\")" not in APP[
        APP.index("impl PreviewQuality"):APP.index("pub(crate) struct PreviewDetail")
    ]
    assert 'if cfg!(target_os = "android") { 384 } else { 1024 }' in PROCESSING

    # Max on a 1440px-wide, 3:2 image: the fit proxy is one physical pixel per
    # display pixel and the detail case includes the full 35% support ceiling.
    max_main = preview_persistent_bytes(1446, 964, 1024)
    max_detail = preview_persistent_bytes(1950, 1300, 384)
    navigation = preview_persistent_bytes(384, 256, 256)
    assert max_main + max_detail + navigation < 384 * MIB


def test_zoom_detail_uses_dedicated_mask_atlas() -> None:
    detail_constructor = PROCESSING[
        PROCESSING.index("let Some(program_template)") : PROCESSING.index(
            "pipeline.dispatch_stage(", PROCESSING.index("let Some(program_template)")
        )
    ]
    assert "new_headless_reusing_programs_with_mask_edge" in detail_constructor
    assert "detail_mask_edge()" in detail_constructor


def test_optional_preview_upload_failure_does_not_block_main_inpainting() -> None:
    sync = PROCESSING[
        PROCESSING.index("pub(crate) fn sync_original_preview") : PROCESSING.index(
            "pub(crate) fn preview_base_pipeline"
        )
    ]
    assert "main preview inpaint upload failed" in sync
    assert "discarding navigation preview after inpaint upload failure" in sync
    assert "discarding zoom detail after inpaint upload failure" in sync
    assert "self.preview_navigation.take()" in sync
    assert "self.preview_detail.take()" in sync
    assert "collect_pipeline_update_results(\"install inpaint layer\"" not in sync
    assert "self.original_preview_rendered_state = Some(requested_state);" in sync
