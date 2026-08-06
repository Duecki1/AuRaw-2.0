from __future__ import annotations

import hashlib
import importlib.util
from pathlib import Path
from types import ModuleType

import numpy as np
import yaml

ROOT = Path(__file__).resolve().parents[1]
CORPUS_PATH = ROOT / "regression/corpus.yaml"
GENERATOR_PATH = ROOT / "regression/generate_synthetic_corpus.py"


def load_generator() -> ModuleType:
    spec = importlib.util.spec_from_file_location("generate_synthetic_corpus", GENERATOR_PATH)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def corpus() -> dict:
    return yaml.safe_load(CORPUS_PATH.read_text(encoding="utf-8"))


def test_synthetic_cfa_alias_suite_covers_four_distinct_failure_modes() -> None:
    expected = {
        "alias-weave",
        "alias-zone-plate",
        "alias-foliage",
        "alias-chroma-crossing",
    }
    scenes = corpus()["scenes"]
    assert {scene["cfa"] for scene in scenes} == {"bayer", "xtrans"}
    for scene in scenes:
        names = {roi["name"] for roi in scene["rois"] if roi["kind"] == "frequency"}
        assert names == expected


def test_generated_scene_exercises_neutral_and_chromatic_alias_properties() -> None:
    generator = load_generator()
    scene = generator.build_scene()
    assert scene.shape == (generator.HEIGHT, generator.WIDTH, 3)
    assert np.isfinite(scene).all()

    weave = scene[24:72, 136:188]
    zone = scene[24:72, 188:240]
    foliage = scene[72:120, 136:188]
    crossing = scene[72:120, 188:240]

    np.testing.assert_allclose(zone[..., 0], zone[..., 1], atol=0.0)
    np.testing.assert_allclose(zone[..., 1], zone[..., 2], atol=0.0)
    assert float(np.std(weave[..., 0] - weave[..., 2])) > 0.04
    assert float(np.mean(foliage[..., 1])) > float(np.mean(foliage[..., 0]))
    assert float(np.mean(foliage[..., 1])) > float(np.mean(foliage[..., 2]))
    assert float(np.std(crossing[..., 0] - crossing[..., 2])) > 0.10


def test_mosaics_sample_the_declared_cfa_plane_at_every_pixel() -> None:
    generator = load_generator()
    scene = generator.build_scene()
    for pattern in (generator.BAYER, generator.XTRANS):
        mosaic = generator.mosaic(scene, pattern)
        yy, xx = np.indices(mosaic.shape)
        channels = pattern[yy % pattern.shape[0], xx % pattern.shape[1]]
        sampled = np.take_along_axis(scene, channels[..., None], axis=2)[..., 0]
        expected = np.rint(
            generator.BLACK
            + np.clip(sampled, 0.0, 1.0) * (generator.WHITE - generator.BLACK)
        ).astype("<u2")
        np.testing.assert_array_equal(mosaic, expected)


def test_synthetic_cfa_alias_fixture_hashes_match_manifest() -> None:
    for scene in corpus()["scenes"]:
        path = ROOT / "regression/raw" / scene["raw"]
        assert hashlib.sha256(path.read_bytes()).hexdigest() == scene["sha256"]
