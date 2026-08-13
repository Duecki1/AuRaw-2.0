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

from iqr.cli import (  # noqa: E402
    _require_candidate_metadata,
    _require_independent_backend_provenance,
    _require_matching_provenance,
)
from iqr.io import LinearImage, load_linear_image, save_linear_image  # noqa: E402
from iqr.manifest import Scene, load_manifest, validate_manifest  # noqa: E402
from iqr.metrics import Roi, compare_images, convolve2d, delta_e_ciede2000  # noqa: E402
from iqr.report import evaluate_thresholds  # noqa: E402
from iqr.reference import load_reference_engines, validate_reference_engines  # noqa: E402


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

    def test_loads_rust_uint8_metadata_member(self) -> None:
        metadata = {
            "schema": 1,
            "color_space": "linear-rec2020-d65",
            "transfer": "linear",
        }
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "rust.npz"
            np.savez(
                path,
                rgb=np.zeros((2, 3, 3), dtype=np.float32),
                metadata_json=np.frombuffer(
                    json.dumps(metadata).encode("utf-8"), dtype=np.uint8
                ),
            )
            loaded = load_linear_image(path, color_space="linear-rec2020-d65")
        self.assertEqual(loaded.rgb.shape, (2, 3, 3))
        self.assertEqual(loaded.metadata["schema"], 1)

    def test_tiff_import_preserves_uint16_and_float32_precision(self) -> None:
        import tifffile

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            integer_path = root / "integer.tif"
            integer = np.array([[[0, 1, 65535], [32768, 12345, 54321]]], dtype=np.uint16)
            tifffile.imwrite(integer_path, integer, photometric="rgb")
            loaded_integer = load_linear_image(
                integer_path, color_space="linear-rec2020-d65", transfer="linear"
            )
            np.testing.assert_array_equal(
                loaded_integer.rgb, integer.astype(np.float32) / np.float32(65535.0)
            )

            float_path = root / "float.tiff"
            floating = np.array([[[-0.25, 0.5, 1.25], [2.0, 4.0, 8.0]]], dtype=np.float32)
            tifffile.imwrite(float_path, floating, photometric="rgb")
            loaded_float = load_linear_image(
                float_path, color_space="linear-rec2020-d65", transfer="linear"
            )
            np.testing.assert_array_equal(loaded_float.rgb, floating)

    def test_tiff_import_accepts_planar_separate_rgb(self) -> None:
        import tifffile

        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "planar.tif"
            planar = np.arange(3 * 4 * 5, dtype=np.uint16).reshape(3, 4, 5)
            tifffile.imwrite(path, planar, photometric="rgb", planarconfig="separate")
            loaded = load_linear_image(
                path, color_space="linear-rec2020-d65", transfer="linear"
            )
            expected = np.moveaxis(planar, 0, 2).astype(np.float32) / np.float32(65535.0)
            np.testing.assert_array_equal(loaded.rgb, expected)

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
            "edge_response_p95_rel",
            "edge_response_gain_error",
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

    def test_edge_response_loss_is_detected(self) -> None:
        reference = synthetic_linear_image()
        box = np.full((3, 3), 1.0 / 9.0, dtype=np.float64)
        blurred = np.stack(
            [convolve2d(reference.rgb[..., channel], box) for channel in range(3)],
            axis=-1,
        ).astype(np.float32)
        candidate = LinearImage(blurred, reference.color_space)
        metrics = compare_images(
            reference,
            candidate,
            rois=[Roi("edge", 36, 4, 28, 88)],
            border=2,
        )
        self.assertGreater(metrics["edge_response_p95_rel"], 0.05)
        self.assertGreater(metrics["edge_response_gain_error"], 0.02)

    def test_highlight_retention_metrics_detect_clipping_change(self) -> None:
        reference_rgb = synthetic_linear_image().rgb.copy()
        reference_rgb[64:90, 64:90, 0] = 1.8
        reference_rgb[64:90, 64:90, 1] = 1.2
        reference_rgb[64:90, 64:90, 2] = 0.7
        reference = LinearImage(reference_rgb, "linear-rec2020-d65")
        candidate_rgb = reference_rgb.copy()
        candidate_rgb[64:90, 64:90, :] = np.minimum(
            candidate_rgb[64:90, 64:90, :], 1.0
        )
        candidate = LinearImage(candidate_rgb, reference.color_space)
        metrics = compare_images(
            reference,
            candidate,
            rois=[Roi("highlight", 64, 64, 26, 26)],
            border=2,
        )
        self.assertGreater(metrics["highlight_luma_rmse_rel"], 0.05)
        self.assertGreater(metrics["highlight_peak_rel_error"], 0.05)
        self.assertGreater(metrics["highlight_clipped_fraction_delta"], 0.05)

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


class CandidateProvenanceTests(unittest.TestCase):
    def setUp(self) -> None:
        self.scene = Scene(
            scene_id="provenance",
            raw=Path("fixture.raw"),
            sha256="a" * 64,
            cfa="bayer",
            tags=(),
            license="CC0-1.0",
            source="test",
            redistributable=True,
        )

    def metadata(self, backend: str, implementation: str) -> dict[str, object]:
        return {
            "backend": backend,
            "implementation": implementation,
            "implementation_fingerprint": f"{implementation}@revision",
            "source_revision": "revision",
            "raw_sha256": self.scene.sha256,
            "renderer_sha256": ("b" if backend == "cpu" else "c") * 64,
            "color_space": "linear-rec2020-d65",
            "transfer": "linear",
        }

    def test_rejects_command_label_that_disagrees_with_artifact_backend(self) -> None:
        with self.assertRaisesRegex(ValueError, "backend"):
            _require_candidate_metadata(
                self.metadata("gpu", "auraw-wgpu"),
                expected_backend="cpu",
                scene=self.scene,
                color_space="linear-rec2020-d65",
            )

    def test_rejects_missing_or_unverified_renderer_identity(self) -> None:
        metadata = self.metadata("gpu", "auraw-wgpu")
        metadata["renderer_sha256"] = "unknown"
        with self.assertRaisesRegex(ValueError, "renderer_sha256"):
            _require_candidate_metadata(
                metadata,
                expected_backend="gpu",
                scene=self.scene,
                color_space="linear-rec2020-d65",
            )

    def test_cpu_gpu_pair_requires_independent_implementations(self) -> None:
        cpu = self.metadata("cpu", "same-renderer")
        gpu = self.metadata("gpu", "same-renderer")
        with self.assertRaisesRegex(ValueError, "distinct implementation"):
            _require_independent_backend_provenance(cpu, gpu, self.scene.scene_id)

    def test_cpu_gpu_pair_rejects_same_renderer_executable(self) -> None:
        cpu = self.metadata("cpu", "auraw-cpu-raw-stage-v1")
        gpu = self.metadata("gpu", "auraw-wgpu-raw-stage-v1")
        gpu["renderer_sha256"] = cpu["renderer_sha256"]
        with self.assertRaisesRegex(ValueError, "independently hashed"):
            _require_independent_backend_provenance(cpu, gpu, self.scene.scene_id)

    def test_cpu_gpu_pair_accepts_distinct_provenance_from_same_revision(self) -> None:
        cpu = self.metadata("cpu", "auraw-cpu-raw-stage-v1")
        gpu = self.metadata("gpu", "auraw-wgpu-raw-stage-v1")
        _require_independent_backend_provenance(cpu, gpu, self.scene.scene_id)

    def test_determinism_pair_rejects_changed_renderer_provenance(self) -> None:
        first = self.metadata("gpu", "auraw-wgpu-raw-stage-v1")
        second = dict(first, renderer_sha256="d" * 64)
        with self.assertRaisesRegex(ValueError, "renderer_sha256"):
            _require_matching_provenance(first, second, self.scene.scene_id)


class ReferenceEngineTests(unittest.TestCase):
    def test_checked_in_history_hashes_are_valid(self) -> None:
        config = load_reference_engines(ROOT / "regression/reference-engines.yaml")
        self.assertEqual(validate_reference_engines(config), [])

    def test_history_hash_mismatch_is_rejected(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            history = root / "history.yaml"
            history.write_text("schema: 1\n", encoding="utf-8")
            config_path = root / "engines.yaml"
            config_path.write_text(
                """schema: 1
engines:
  test:
    version: 1.0
    source_revision: abc
    source_sha256: null
    version_command: [test, --version]
    version_output_contains: 1.0
    history: history.yaml
    history_sha256: "0000000000000000000000000000000000000000000000000000000000000000"
""",
                encoding="utf-8",
            )
            config = load_reference_engines(config_path)
            errors = validate_reference_engines(config)
        self.assertTrue(any("processing-history SHA-256 mismatch" in error for error in errors))


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
