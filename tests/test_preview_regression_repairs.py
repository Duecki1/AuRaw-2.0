from __future__ import annotations

import math

MIB = 1024 * 1024


def preview_persistent_bytes(
    width: int,
    height: int,
    mask_edge: int,
    tone_scale: int = 8,
    mask_layers: int = 32,
) -> int:
    """Analytical preview reference for persistent GPU resources."""
    pixels = width * height
    # CFA R16 + color R8 + black R32 + reconstructed R32 + six RGBA16 work
    # textures + RGBA8 output + RGBA16 inpaint.
    full_frame = pixels * (2 + 1 + 4 + 4 + 6 * 8 + 4 + 8)
    tone_width = math.ceil(width / tone_scale)
    tone_height = math.ceil(height / tone_scale)
    tone_guides = 2 * tone_width * tone_height * 4
    mask_atlas = mask_edge * mask_edge * mask_layers * 2
    fixed_buffers = MIB
    return full_frame + tone_guides + mask_atlas + fixed_buffers


def test_android_zoom_working_set_fits_resident_budget() -> None:
    # Max on a 1440px-wide, 3:2 image: the fit proxy is one physical pixel per
    # display pixel and the detail case includes the full 35% support ceiling.
    max_main = preview_persistent_bytes(1446, 964, 1024)
    # Explicit-edge detail pipelines allocate only the active mask layers.
    max_detail = preview_persistent_bytes(1950, 1300, 1024, mask_layers=1)
    navigation = preview_persistent_bytes(384, 256, 256)

    assert max_main + max_detail + navigation < 384 * MIB
