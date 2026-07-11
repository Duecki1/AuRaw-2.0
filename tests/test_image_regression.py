from __future__ import annotations

import hashlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "regression"))

from iqr.io import LinearImage, load_linear_image, save_linear_image  # noqa: E402
from iqr.manifest import load_manifest, validate_manifest  # noqa: E402
from iqr.metrics import Roi, compare_images, delta_e_ciede2000  # noqa: E402
from iqr.report import evaluate_thresholds  # noqa: E402


class LinearIntermediateTests(unittest.TestCase):
    def test_round_trip_preserves_pixels_metadata_and_mask(self) -> None:
        rng = np.random.default_rng(7)
        rgb = rng.random((12, 16, 3), dtype=np.float32)
        mask = np.ones((12, 16), dtype=bool)
        mask[:2, :] = False
        image = LinearImage(rgb, "linear-rec2020-d65", {"scene": "roundtrip"}, mask)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "image.npz"
            save_linear_image(path, image)
            loaded = load_linear_image(path, color_space="linear-rec2020-d65")
        self.assertTrue(np.array_equal(loaded.rgb, rgb))
        self.assertTrue(np.array_equal(loaded.valid_mask, mask))
        self.assertEqual(loaded.metadata["scene"], "roundtrip")
        self.assertEqual(loaded.metadata["transfer"], "linear")

    def test_rejects_camera_rgb_for_delta_e(self) -> None:
        image = LinearImage(np.zeros((40, 40, 3), dtype=np.float32), "camera-rgb")
        with self.assertRaisesRegex(ValueError, "camera-rgb"):
            compare_images(image, image, border=1)


class MetricTests(unittest.TestCase):
    def test_ciede2000_matches_published_reference_pair(self) -> None:
        lab1 = np.asarray([[[50.0, 2.6772, -79.7751]]])
        lab2 = np.asarray([[[50.0, 0.0, -82.7485]]])
        delta = float(delta_e_ciede2000(lab1, lab2)[0, 0])
        self.assertAlmostEqual(delta, 2.0425, places=4)

    def test_identical_images_have_zero_error(self) -> None:
        image = synthetic_linear_image()
        metrics = compare_images(
            image,
            image,
            rois=[Roi("edge", 20, 10, 60, 76), Roi("flat", 2, 2, 16, 80)],
            border=2,
        )
        for name in (
            "rmse",
            "mae",
            "max_abs",
            "delta_e00_mean",
            "delta_e00_p95",
            "edge_rmse_rel",
            "zippering_p95",
            "false_color_p95",
            "noise_sigma_rel",
            "noise_bias_max",
        ):
            self.assertLessEqual(metrics[name], 1e-10, name)

    def test_false_color_and_zippering_perturbation_is_detected(self) -> None:
        reference = synthetic_linear_image()
        candidate_rgb = reference.rgb.copy()
        yy, xx = np.indices(candidate_rgb.shape[:2])
        band = (xx >= 45) & (xx <= 52)
        alternating = np.where((yy + xx) % 2 == 0, 0.035, -0.035).astype(np.float32)
        candidate_rgb[..., 0][band] += alternating[band]
        candidate_rgb[..., 2][band] -= alternating[band]
        candidate = LinearImage(candidate_rgb, reference.color_space)
        metrics = compare_images(
            reference,
            candidate,
            rois=[Roi("edge", 36, 4, 28, 88), Roi("neutral", 36, 4, 28, 88)],
            border=2,
        )
        self.assertGreater(metrics["zippering_p95"], 0.02)
        self.assertGreater(metrics["false_color_p95"], 1.0)
        failures = evaluate_thresholds(
            metrics, {"zippering_p95": 0.01, "false_color_p95": 0.5}
        )
        self.assertEqual(len(failures), 2)

    def test_noise_change_is_detected_in_flat_roi(self) -> None:
        reference = synthetic_linear_image(noise=0.002)
        rng = np.random.default_rng(11)
        candidate_rgb = reference.rgb.copy()
        candidate_rgb[:, :24, :] += rng.normal(0.0, 0.008, (96, 24, 3)).astype(np.float32)
        candidate = LinearImage(candidate_rgb, reference.color_space)
        metrics = compare_images(
            reference,
            candidate,
            rois=[Roi("flat", 2, 2, 20, 92)],
            border=2,
        )
        self.assertGreater(metrics["noise_sigma_rel"], 1.0)


class ManifestTests(unittest.TestCase):
    def test_manifest_checks_hashes_and_required_coverage(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            raw = root / "sample.raw"
            raw.write_bytes(b"auraw-regression")
            digest = hashlib.sha256(raw.read_bytes()).hexdigest()
            manifest_path = root / "corpus.yaml"
            manifest_path.write_text(
                f"""schema: 1
color_space: linear-rec2020-d65
raw_root: .
scenes:
  - id: bayer-all
    raw: sample.raw
    sha256: {digest}
    cfa: bayer
    tags: [high-iso, underexposed, saturated-highlight, difficult-frequency]
    source: test fixture
    license: CC0-1.0
    redistributable: true
  - id: xtrans-all
    raw: sample.raw
    sha256: {digest}
    cfa: xtrans
    tags: []
    source: test fixture
    license: CC0-1.0
    redistributable: true
""",
                encoding="utf-8",
            )
            manifest = load_manifest(manifest_path)
            self.assertEqual(validate_manifest(manifest, verify_files=True), [])
            raw.write_bytes(b"changed")
            errors = validate_manifest(manifest, verify_files=True)
            self.assertTrue(any("SHA-256 mismatch" in error for error in errors))


def synthetic_linear_image(noise: float = 0.0) -> LinearImage:
    height = width = 96
    yy, xx = np.indices((height, width))
    base = np.where(xx < 48, 0.18, 0.68).astype(np.float32)
    # Add a low-amplitude diagonal frequency target and a neutral flat strip.
    base += (0.015 * np.sin((xx + yy) * np.pi / 3.0)).astype(np.float32)
    rgb = np.stack((base, base, base), axis=-1)
    rgb[:, :24, :] = 0.22
    if noise:
        rng = np.random.default_rng(3)
        rgb += rng.normal(0.0, noise, rgb.shape).astype(np.float32)
    return LinearImage(rgb, "linear-rec2020-d65", {"fixture": True})


if __name__ == "__main__":
    unittest.main()
