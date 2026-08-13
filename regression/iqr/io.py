from __future__ import annotations

from dataclasses import dataclass, field
import json
from pathlib import Path
from typing import Any

import numpy as np


SUPPORTED_COLOR_SPACES = {"linear-srgb-d65", "linear-rec2020-d65", "camera-rgb"}


@dataclass(frozen=True)
class LinearImage:
    rgb: np.ndarray
    color_space: str
    metadata: dict[str, Any] = field(default_factory=dict)
    valid_mask: np.ndarray | None = None

    def __post_init__(self) -> None:
        rgb = np.asarray(self.rgb)
        if rgb.ndim != 3 or rgb.shape[2] != 3:
            raise ValueError(f"rgb must have shape HxWx3, got {rgb.shape}")
        if not np.issubdtype(rgb.dtype, np.floating):
            raise ValueError(f"rgb must be floating point, got {rgb.dtype}")
        if not np.all(np.isfinite(rgb)):
            raise ValueError("rgb contains NaN or infinity")
        if self.color_space not in SUPPORTED_COLOR_SPACES:
            raise ValueError(
                f"unsupported color space {self.color_space!r}; expected one of "
                f"{sorted(SUPPORTED_COLOR_SPACES)}"
            )
        if self.valid_mask is not None:
            mask = np.asarray(self.valid_mask)
            if mask.shape != rgb.shape[:2]:
                raise ValueError(
                    f"valid_mask must match image dimensions {rgb.shape[:2]}, got {mask.shape}"
                )


def save_linear_image(path: Path | str, image: LinearImage) -> None:
    """Write a deterministic, self-describing float32 NPZ intermediate."""
    target = Path(path)
    target.parent.mkdir(parents=True, exist_ok=True)
    metadata = dict(image.metadata)
    metadata.update(
        {
            "schema": 1,
            "color_space": image.color_space,
            "transfer": "linear",
            "layout": "HWC",
            "channels": ["R", "G", "B"],
            "dtype": "float32",
        }
    )
    payload: dict[str, np.ndarray] = {
        "rgb": np.asarray(image.rgb, dtype="<f4"),
        "metadata_json": np.asarray(
            json.dumps(metadata, sort_keys=True, separators=(",", ":")), dtype=np.str_
        ),
    }
    if image.valid_mask is not None:
        payload["valid_mask"] = np.asarray(image.valid_mask, dtype=np.uint8)
    # ZIP_STORED avoids compressor-version drift. NPY members are deterministic.
    np.savez(target, **payload)


def load_linear_image(
    path: Path | str,
    *,
    color_space: str | None = None,
    transfer: str = "linear",
) -> LinearImage:
    source = Path(path)
    suffix = source.suffix.lower()
    if suffix == ".npz":
        with np.load(source, allow_pickle=False) as data:
            if "rgb" not in data or "metadata_json" not in data:
                raise ValueError(f"{source} is not an AuRaw linear intermediate")
            rgb = np.asarray(data["rgb"], dtype=np.float32)
            raw_metadata = data["metadata_json"]
            if raw_metadata.dtype == np.uint8:
                metadata_text = np.asarray(raw_metadata, dtype=np.uint8).tobytes().decode("utf-8")
            else:
                metadata_text = str(raw_metadata.item())
            metadata = json.loads(metadata_text)
            mask = (
                np.asarray(data["valid_mask"], dtype=bool)
                if "valid_mask" in data
                else None
            )
        stored_space = str(metadata.get("color_space", ""))
        if color_space is not None and stored_space != color_space:
            raise ValueError(
                f"{source} color space is {stored_space!r}, expected {color_space!r}"
            )
        return LinearImage(rgb, stored_space, metadata, mask)

    if color_space is None:
        raise ValueError("--color-space is required when importing non-NPZ images")

    if suffix == ".npy":
        rgb = np.load(source, allow_pickle=False)
    elif suffix in {".tif", ".tiff"}:
        import tifffile

        # Uncompressed TIFFs can be mapped without an eager full-file copy.
        # Compressed/tiled images fall back to tifffile's native decoder while
        # retaining uint16/float32 samples without an 8-bit intermediate.
        try:
            rgb = tifffile.memmap(source)
        except (ValueError, OSError):
            rgb = tifffile.imread(source)
    elif suffix in {".png", ".jpg", ".jpeg"}:
        from PIL import Image

        rgb = np.asarray(Image.open(source).convert("RGB"))
    else:
        raise ValueError(f"unsupported image extension: {suffix}")

    rgb = _as_rgb_float(rgb)
    if transfer == "srgb":
        rgb = srgb_to_linear(rgb)
    elif transfer != "linear":
        raise ValueError("transfer must be 'linear' or 'srgb'")
    return LinearImage(
        rgb,
        color_space,
        {
            "schema": 1,
            "source_file": source.name,
            "source_transfer": transfer,
        },
    )


def _as_rgb_float(array: np.ndarray) -> np.ndarray:
    value = np.asarray(array)
    if value.ndim == 3 and value.shape[0] in {3, 4} and value.shape[2] not in {3, 4}:
        value = np.moveaxis(value, 0, 2)
    if value.ndim == 2:
        value = np.repeat(value[..., None], 3, axis=2)
    if value.ndim != 3 or value.shape[2] not in {3, 4}:
        raise ValueError(f"expected HxWx3 or HxWx4 image, got {value.shape}")
    value = value[..., :3]
    if np.issubdtype(value.dtype, np.integer):
        max_value = float(np.iinfo(value.dtype).max)
        value = value.astype(np.float32) / max_value
    else:
        value = value.astype(np.float32)
    if not np.all(np.isfinite(value)):
        raise ValueError("image contains NaN or infinity")
    return value


def srgb_to_linear(value: np.ndarray) -> np.ndarray:
    value = np.asarray(value, dtype=np.float32)
    return np.where(
        value <= 0.04045,
        value / 12.92,
        ((value + 0.055) / 1.055) ** 2.4,
    ).astype(np.float32)
