from __future__ import annotations

import importlib.util
import py_compile
import sys
from pathlib import Path
from types import ModuleType

import numpy as np

ROOT = Path(__file__).resolve().parents[1]
COMPARATOR = ROOT / "scripts/compare_lightroom_adjustments.py"


def load_comparator() -> ModuleType:
    spec = importlib.util.spec_from_file_location("compare_lightroom_adjustments", COMPARATOR)
    assert spec and spec.loader
    module = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = module
    spec.loader.exec_module(module)
    return module


def test_lightroom_comparator_compiles() -> None:
    py_compile.compile(str(COMPARATOR), doraise=True)


def test_relative_luma_response_is_independent_of_each_renderer_baseline() -> None:
    comparator = load_comparator()
    auraw_base = np.array([[[0.10, 0.20, 0.30], [0.30, 0.20, 0.10]]], dtype=np.float32)
    lightroom_base = np.array([[[0.35, 0.12, 0.08], [0.08, 0.18, 0.42]]], dtype=np.float32)

    auraw_delta = comparator.luma_delta_ev(
        auraw_base, auraw_base * 2.0, comparator.SRGB_LUMA
    )
    lightroom_delta = comparator.luma_delta_ev(
        lightroom_base, lightroom_base * 2.0, comparator.ADOBE_RGB_LUMA
    )

    np.testing.assert_allclose(auraw_delta, 1.0, atol=1e-6)
    np.testing.assert_allclose(lightroom_delta, 1.0, atol=1e-6)
    np.testing.assert_allclose(
        comparator.baseline_luma_quantiles(auraw_base, comparator.SRGB_LUMA),
        np.quantile(auraw_base @ comparator.SRGB_LUMA, [0.05, 0.50, 0.95]),
    )
    np.testing.assert_allclose(
        comparator.baseline_luma_quantiles(lightroom_base, comparator.ADOBE_RGB_LUMA),
        np.quantile(lightroom_base @ comparator.ADOBE_RGB_LUMA, [0.05, 0.50, 0.95]),
    )


def test_decoder_preserves_native_uint16_values(monkeypatch, tmp_path: Path) -> None:
    comparator = load_comparator()
    source = tmp_path / "fixture.tif"
    source.write_bytes(b"placeholder")
    samples = np.array([0, 1, 32768, 65535, 12345, 54321], dtype="<u2")
    observed: dict[str, object] = {}

    class Result:
        stdout = samples.tobytes()
        stderr = b""

    def fake_run(command, **kwargs):
        observed["command"] = command
        observed["kwargs"] = kwargs
        return Result()

    monkeypatch.setattr(comparator, "image_region_size", lambda path, crop: (2, 1))
    monkeypatch.setattr(comparator.subprocess, "run", fake_run)
    decoded = comparator.encoded_rgb16(source, crop=None, sample_step=1)

    np.testing.assert_allclose(decoded.reshape(-1), samples.astype(np.float32) / 65535.0)
    command = observed["command"]
    assert command[-7:] == ["-alpha", "off", "-depth", "16", "-endian", "LSB", "rgb:-"]


def test_color_space_transfers_and_endpoint_metadata_are_functional(monkeypatch) -> None:
    comparator = load_comparator()
    encoded = np.array([[[0.02, 0.18, 0.75]]], dtype=np.float32)
    monkeypatch.setattr(comparator, "encoded_rgb16", lambda *args, **kwargs: encoded.copy())

    srgb = comparator.linear_rgb(Path("unused"), crop=None, sample_step=1, color_space="srgb")
    adobe = comparator.linear_rgb(
        Path("unused"), crop=None, sample_step=1, color_space="adobe-rgb"
    )
    expected_srgb = np.where(
        encoded <= 0.04045,
        encoded / 12.92,
        ((encoded + 0.055) / 1.055) ** 2.4,
    )
    np.testing.assert_allclose(srgb, expected_srgb, rtol=1e-6)
    np.testing.assert_allclose(adobe, encoded**comparator.ADOBE_RGB_GAMMA, rtol=1e-6)

    endpoint = next(item for item in comparator.ENDPOINTS if item.name == "Vibrance -100")
    assert endpoint.lightroom_file == "Vibration -100.tif"
    assert comparator.parse_crop("1,2,3,4") == (1, 2, 3, 4)
