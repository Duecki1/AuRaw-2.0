from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_android_gpu_preview_prewarm_resolves_cfa_kind_explicitly() -> None:
    lifecycle = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
    pipeline = (ROOT / "src/pipeline/mod.rs").read_text(encoding="utf-8")

    assert '#[cfg(target_os = "android")]\nfn spawn_gpu_preview_prewarm' in lifecycle
    assert "CameraProfileCandidate, CameraProfileMode, CfaKind," in pipeline
    assert "crate::pipeline::CfaKind::Bayer" in lifecycle
    assert re.search(r"(?m)^\s*CfaKind::Bayer,\s*$", lifecycle) is None
