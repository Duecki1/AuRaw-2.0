from __future__ import annotations

import hashlib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
CORPUS = (ROOT / "regression/corpus.yaml").read_text(encoding="utf-8")
GENERATOR = (ROOT / "regression/generate_synthetic_corpus.py").read_text(encoding="utf-8")








def test_synthetic_cfa_alias_suite_covers_four_distinct_failure_modes() -> None:
    for name in (
        "alias-weave",
        "alias-zone-plate",
        "alias-foliage",
        "alias-chroma-crossing",
    ):
        assert CORPUS.count(f"name: {name}") == 2

    for marker in (
        "woven fabric",
        "radial zone plate",
        "foliage-like",
        "chromatic stripe crossings",
    ):
        assert marker in GENERATOR


def test_synthetic_cfa_alias_fixture_hashes_match_manifest() -> None:
    fixtures = {
        "synthetic-bayer.dng": "74b940c8020ea0572d553f10ae1fb8fa3858965a1feabcf600bd74ee068340ff",
        "synthetic-xtrans.dng": "a1d6b7d8dd3590fec4a1c64afd635603b015c23be65144b0959835dbe352b2c4",
    }
    for name, expected in fixtures.items():
        path = ROOT / "regression/raw" / name
        assert hashlib.sha256(path.read_bytes()).hexdigest() == expected
        assert expected in CORPUS
