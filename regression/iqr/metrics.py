from __future__ import annotations

from dataclasses import dataclass
from typing import Iterable

import numpy as np

from .io import LinearImage


_RGB_TO_XYZ = {
    "linear-srgb-d65": np.asarray(
        [
            [0.4124564, 0.3575761, 0.1804375],
            [0.2126729, 0.7151522, 0.0721750],
            [0.0193339, 0.1191920, 0.9503041],
        ],
        dtype=np.float64,
    ),
    "linear-rec2020-d65": np.asarray(
        [
            [0.63695805, 0.14461690, 0.16888098],
            [0.26270021, 0.67799807, 0.05930172],
            [0.00000000, 0.02807269, 1.06098506],
        ],
        dtype=np.float64,
    ),
}
_D65 = np.asarray([0.95047, 1.0, 1.08883], dtype=np.float64)
_SCHARR_X = np.asarray(
    [[-3.0, 0.0, 3.0], [-10.0, 0.0, 10.0], [-3.0, 0.0, 3.0]],
    dtype=np.float64,
) / 32.0
_SCHARR_Y = _SCHARR_X.T
_LAPLACIAN = np.asarray(
    [[0.0, 1.0, 0.0], [1.0, -4.0, 1.0], [0.0, 1.0, 0.0]],
    dtype=np.float64,
)
_BOX3 = np.full((3, 3), 1.0 / 9.0, dtype=np.float64)


@dataclass(frozen=True)
class Roi:
    kind: str
    x: int
    y: int
    width: int
    height: int
    name: str = ""

    @classmethod
    def from_mapping(cls, value: dict[str, object]) -> "Roi":
        rect = value.get("rect")
        if not isinstance(rect, list) or len(rect) != 4:
            raise ValueError(f"ROI rect must be [x, y, width, height], got {rect!r}")
        return cls(
            kind=str(value.get("kind", "general")),
            x=int(rect[0]),
            y=int(rect[1]),
            width=int(rect[2]),
            height=int(rect[3]),
            name=str(value.get("name", "")),
        )

    def mask(self, shape: tuple[int, int]) -> np.ndarray:
        height, width = shape
        if self.width <= 0 or self.height <= 0:
            raise ValueError(f"ROI {self.name or self.kind!r} has non-positive size")
        if self.x < 0 or self.y < 0 or self.x + self.width > width or self.y + self.height > height:
            raise ValueError(
                f"ROI {self.name or self.kind!r}={self.x,self.y,self.width,self.height} "
                f"is outside image {width}x{height}"
            )
        result = np.zeros(shape, dtype=bool)
        result[self.y : self.y + self.height, self.x : self.x + self.width] = True
        return result


def compare_images(
    reference: LinearImage,
    candidate: LinearImage,
    *,
    rois: Iterable[Roi] = (),
    border: int = 18,
) -> dict[str, float]:
    if reference.rgb.shape != candidate.rgb.shape:
        raise ValueError(
            f"shape mismatch: reference {reference.rgb.shape}, candidate {candidate.rgb.shape}"
        )
    if reference.color_space != candidate.color_space:
        raise ValueError(
            f"color-space mismatch: reference {reference.color_space}, candidate {candidate.color_space}"
        )
    if reference.color_space == "camera-rgb":
        raise ValueError(
            "camera-rgb cannot be used for Delta E; normalize both images into "
            "linear-srgb-d65 or linear-rec2020-d65"
        )

    ref = np.asarray(reference.rgb, dtype=np.float64)
    cand = np.asarray(candidate.rgb, dtype=np.float64)
    shape = ref.shape[:2]
    valid = np.ones(shape, dtype=bool)
    if border > 0:
        if shape[0] <= border * 2 or shape[1] <= border * 2:
            raise ValueError(f"border {border} is too large for image {shape[1]}x{shape[0]}")
        valid[:border, :] = False
        valid[-border:, :] = False
        valid[:, :border] = False
        valid[:, -border:] = False
    if reference.valid_mask is not None:
        valid &= np.asarray(reference.valid_mask, dtype=bool)
    if candidate.valid_mask is not None:
        valid &= np.asarray(candidate.valid_mask, dtype=bool)
    if not np.any(valid):
        raise ValueError("no valid pixels remain after border/mask filtering")

    roi_list = list(rois)
    general_mask = valid.copy()
    edge_roi_mask = _roi_union(roi_list, shape, {"edge", "frequency"})
    flat_roi_mask = _roi_union(roi_list, shape, {"flat", "noise"})
    neutral_roi_mask = _roi_union(roi_list, shape, {"neutral", "edge", "frequency"})
    highlight_roi_mask = _roi_union(roi_list, shape, {"highlight"})
    if edge_roi_mask is not None:
        edge_roi_mask &= valid
    if flat_roi_mask is not None:
        flat_roi_mask &= valid
    if neutral_roi_mask is not None:
        neutral_roi_mask &= valid
    if highlight_roi_mask is not None:
        highlight_roi_mask &= valid

    residual = cand - ref
    abs_residual = np.abs(residual)
    rmse = float(np.sqrt(np.mean(np.square(residual[valid]))))
    peak = max(float(np.percentile(np.abs(ref[valid]), 99.9)), 1.0)
    psnr = float(20.0 * np.log10(peak / max(rmse, 1e-12)))

    ref_lab = rgb_to_lab(ref, reference.color_space)
    cand_lab = rgb_to_lab(cand, candidate.color_space)
    delta_e = delta_e_ciede2000(ref_lab, cand_lab)

    ref_y = luminance(ref, reference.color_space)
    cand_y = luminance(cand, candidate.color_space)
    ref_gx = convolve2d(ref_y, _SCHARR_X)
    ref_gy = convolve2d(ref_y, _SCHARR_Y)
    cand_gx = convolve2d(cand_y, _SCHARR_X)
    cand_gy = convolve2d(cand_y, _SCHARR_Y)
    ref_edge = np.hypot(ref_gx, ref_gy)
    cand_edge = np.hypot(cand_gx, cand_gy)
    edge_threshold = float(np.percentile(ref_edge[valid], 75.0))
    edge_mask = valid & (ref_edge >= max(edge_threshold, 1e-6))
    if edge_roi_mask is not None and np.any(edge_roi_mask):
        edge_mask &= edge_roi_mask
    if not np.any(edge_mask):
        edge_mask = valid
    edge_scale = max(float(np.sqrt(np.mean(np.square(ref_edge[edge_mask])))), 1e-8)
    edge_rmse_rel = float(
        np.sqrt(np.mean(np.square(cand_edge[edge_mask] - ref_edge[edge_mask]))) / edge_scale
    )
    dot = ref_gx * cand_gx + ref_gy * cand_gy
    denom = np.maximum(ref_edge * cand_edge, 1e-12)
    angle = np.degrees(np.arccos(np.clip(dot / denom, -1.0, 1.0)))

    ref_chroma = np.stack((ref[..., 0] - ref[..., 1], ref[..., 2] - ref[..., 1]), axis=-1)
    cand_chroma = np.stack((cand[..., 0] - cand[..., 1], cand[..., 2] - cand[..., 1]), axis=-1)
    chroma_residual = cand_chroma - ref_chroma
    zipper_response = np.hypot(
        convolve2d(chroma_residual[..., 0], _LAPLACIAN),
        convolve2d(chroma_residual[..., 1], _LAPLACIAN),
    )
    zipper_scale = max(float(np.percentile(ref_edge[edge_mask], 95.0)), 1e-6)

    chroma_delta_lab = np.hypot(cand_lab[..., 1] - ref_lab[..., 1], cand_lab[..., 2] - ref_lab[..., 2])
    ref_chroma_lab = np.hypot(ref_lab[..., 1], ref_lab[..., 2])
    neutral_mask = valid & (ref_chroma_lab <= 12.0)
    if neutral_roi_mask is not None and np.any(neutral_roi_mask):
        neutral_mask &= neutral_roi_mask
    neutral_edge_mask = neutral_mask & edge_mask
    if not np.any(neutral_edge_mask):
        neutral_edge_mask = neutral_mask if np.any(neutral_mask) else edge_mask

    noise_mask = flat_roi_mask if flat_roi_mask is not None and np.any(flat_roi_mask) else _auto_flat_mask(ref_y, valid)
    ref_sigma = _noise_sigma(ref, noise_mask)
    cand_sigma = _noise_sigma(cand, noise_mask)
    noise_sigma_rel = float(np.max(np.abs(cand_sigma - ref_sigma) / np.maximum(ref_sigma, 1e-6)))
    noise_bias = float(np.max(np.abs(np.median(residual[noise_mask], axis=0))))

    metrics = {
        "rmse": rmse,
        "mae": float(np.mean(abs_residual[valid])),
        "max_abs": float(np.max(abs_residual[valid])),
        "psnr_db": psnr,
        "delta_e00_mean": float(np.mean(delta_e[general_mask])),
        "delta_e00_p95": float(np.percentile(delta_e[general_mask], 95.0)),
        "delta_e00_max": float(np.max(delta_e[general_mask])),
        "edge_rmse_rel": edge_rmse_rel,
        "edge_angle_p95_deg": float(np.percentile(angle[edge_mask], 95.0)),
        "zippering_p95": float(np.percentile(zipper_response[edge_mask] / zipper_scale, 95.0)),
        "false_color_p95": float(np.percentile(chroma_delta_lab[neutral_edge_mask], 95.0)),
        "noise_sigma_rel": noise_sigma_rel,
        "noise_bias_max": noise_bias,
        "valid_pixel_fraction": float(np.mean(valid)),
    }
    for channel, name in enumerate(("r", "g", "b")):
        metrics[f"noise_sigma_ref_{name}"] = float(ref_sigma[channel])
        metrics[f"noise_sigma_candidate_{name}"] = float(cand_sigma[channel])
    if highlight_roi_mask is not None and np.any(highlight_roi_mask):
        metrics["highlight_delta_e00_p95"] = float(
            np.percentile(delta_e[highlight_roi_mask], 95.0)
        )
        metrics["highlight_max_abs"] = float(np.max(abs_residual[highlight_roi_mask]))
    return metrics


def luminance(rgb: np.ndarray, color_space: str) -> np.ndarray:
    matrix = _RGB_TO_XYZ[color_space]
    return np.tensordot(rgb, matrix[1], axes=([-1], [0]))


def rgb_to_lab(rgb: np.ndarray, color_space: str) -> np.ndarray:
    matrix = _RGB_TO_XYZ[color_space]
    xyz = np.tensordot(rgb, matrix.T, axes=([-1], [0]))
    normalized = xyz / _D65
    delta = 6.0 / 29.0
    threshold = delta**3
    f = np.where(
        normalized > threshold,
        np.cbrt(normalized),
        normalized / (3.0 * delta**2) + 4.0 / 29.0,
    )
    l = 116.0 * f[..., 1] - 16.0
    a = 500.0 * (f[..., 0] - f[..., 1])
    b = 200.0 * (f[..., 1] - f[..., 2])
    return np.stack((l, a, b), axis=-1)


def delta_e_ciede2000(lab1: np.ndarray, lab2: np.ndarray) -> np.ndarray:
    # Vectorized Sharma et al. CIEDE2000 implementation, kL=kC=kH=1.
    l1, a1, b1 = np.moveaxis(np.asarray(lab1, dtype=np.float64), -1, 0)
    l2, a2, b2 = np.moveaxis(np.asarray(lab2, dtype=np.float64), -1, 0)
    c1 = np.hypot(a1, b1)
    c2 = np.hypot(a2, b2)
    c_bar = (c1 + c2) / 2.0
    c7 = c_bar**7
    g = 0.5 * (1.0 - np.sqrt(c7 / (c7 + 25.0**7)))
    a1p = (1.0 + g) * a1
    a2p = (1.0 + g) * a2
    c1p = np.hypot(a1p, b1)
    c2p = np.hypot(a2p, b2)
    h1p = np.mod(np.degrees(np.arctan2(b1, a1p)), 360.0)
    h2p = np.mod(np.degrees(np.arctan2(b2, a2p)), 360.0)
    h1p = np.where(c1p == 0.0, 0.0, h1p)
    h2p = np.where(c2p == 0.0, 0.0, h2p)

    dl = l2 - l1
    dc = c2p - c1p
    dh_raw = h2p - h1p
    dh = np.where(
        c1p * c2p == 0.0,
        0.0,
        np.where(dh_raw > 180.0, dh_raw - 360.0, np.where(dh_raw < -180.0, dh_raw + 360.0, dh_raw)),
    )
    d_h = 2.0 * np.sqrt(c1p * c2p) * np.sin(np.radians(dh / 2.0))

    l_bar = (l1 + l2) / 2.0
    cp_bar = (c1p + c2p) / 2.0
    hp_sum = h1p + h2p
    hp_diff = np.abs(h1p - h2p)
    hp_bar = np.where(
        c1p * c2p == 0.0,
        hp_sum,
        np.where(
            hp_diff <= 180.0,
            hp_sum / 2.0,
            np.where(hp_sum < 360.0, (hp_sum + 360.0) / 2.0, (hp_sum - 360.0) / 2.0),
        ),
    )

    t = (
        1.0
        - 0.17 * np.cos(np.radians(hp_bar - 30.0))
        + 0.24 * np.cos(np.radians(2.0 * hp_bar))
        + 0.32 * np.cos(np.radians(3.0 * hp_bar + 6.0))
        - 0.20 * np.cos(np.radians(4.0 * hp_bar - 63.0))
    )
    sl = 1.0 + 0.015 * (l_bar - 50.0) ** 2 / np.sqrt(20.0 + (l_bar - 50.0) ** 2)
    sc = 1.0 + 0.045 * cp_bar
    sh = 1.0 + 0.015 * cp_bar * t
    delta_theta = 30.0 * np.exp(-((hp_bar - 275.0) / 25.0) ** 2)
    rc = 2.0 * np.sqrt(cp_bar**7 / (cp_bar**7 + 25.0**7))
    rt = -rc * np.sin(np.radians(2.0 * delta_theta))
    return np.sqrt(
        (dl / sl) ** 2
        + (dc / sc) ** 2
        + (d_h / sh) ** 2
        + rt * (dc / sc) * (d_h / sh)
    )


def convolve2d(image: np.ndarray, kernel: np.ndarray) -> np.ndarray:
    source = np.asarray(image, dtype=np.float64)
    filt = np.asarray(kernel, dtype=np.float64)
    if filt.ndim != 2 or filt.shape[0] % 2 == 0 or filt.shape[1] % 2 == 0:
        raise ValueError("kernel must have odd dimensions")
    py, px = filt.shape[0] // 2, filt.shape[1] // 2
    padded = np.pad(source, ((py, py), (px, px)), mode="reflect")
    result = np.zeros_like(source, dtype=np.float64)
    for y in range(filt.shape[0]):
        for x in range(filt.shape[1]):
            result += filt[y, x] * padded[y : y + source.shape[0], x : x + source.shape[1]]
    return result


def _roi_union(rois: list[Roi], shape: tuple[int, int], kinds: set[str]) -> np.ndarray | None:
    selected = [roi for roi in rois if roi.kind in kinds]
    if not selected:
        return None
    result = np.zeros(shape, dtype=bool)
    for roi in selected:
        result |= roi.mask(shape)
    return result


def _auto_flat_mask(luma: np.ndarray, valid: np.ndarray) -> np.ndarray:
    gx = convolve2d(luma, _SCHARR_X)
    gy = convolve2d(luma, _SCHARR_Y)
    gradient = np.hypot(gx, gy)
    threshold = float(np.percentile(gradient[valid], 20.0))
    mask = valid & (gradient <= threshold)
    if np.count_nonzero(mask) < 64:
        return valid
    return mask


def _noise_sigma(rgb: np.ndarray, mask: np.ndarray) -> np.ndarray:
    values = []
    for channel in range(3):
        highpass = rgb[..., channel] - convolve2d(rgb[..., channel], _BOX3)
        samples = highpass[mask]
        median = np.median(samples)
        sigma = 1.4826 * np.median(np.abs(samples - median))
        values.append(float(sigma))
    return np.asarray(values, dtype=np.float64)
