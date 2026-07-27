from __future__ import annotations

import re
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def test_android_gpu_preview_prewarm_resolves_cfa_kind_explicitly() -> None:
    lifecycle = (ROOT / "src/app/lifecycle.rs").read_text(encoding="utf-8")
    pipeline = (ROOT / "src/pipeline/mod.rs").read_text(encoding="utf-8")

    assert '#[cfg(target_os = "android")]\nfn spawn_gpu_preview_prewarm' in lifecycle
    raw_exports = pipeline[
        pipeline.index("pub use raw_loader::{") : pipeline.index("pub use sigmoid")
    ]
    assert "CameraProfileCandidate" in raw_exports
    assert "CameraProfileMode" in raw_exports
    assert "CfaKind" in raw_exports
    assert "crate::pipeline::CfaKind::Bayer" in lifecycle
    assert re.search(r"(?m)^\s*CfaKind::Bayer,\s*$", lifecycle) is None
