"""Analytical float32/float16 curve-continuity references."""
from __future__ import annotations

import math
import numpy as np
LUMA = (0.2627002, 0.6779981, 0.0593017)
SCENE_MIDDLE_GREY = 0.1845
SCENE_CURVE_DECODE_MAX = 32768.0
SCENE_CURVE_WORK_MAX = 60000.0
SCENE_CURVE_ZERO_SLOPE_MAX = 1048576.0
SCENE_CURVE_SHOULDER_ENCODE_START = 0.9999915361404419
HALF_FLOAT_MAX = 65504.0


def dot(rgb: tuple[float, float, float]) -> float:
    return sum(channel * weight for channel, weight in zip(rgb, LUMA))


def _constants_f32() -> tuple[np.float32, ...]:
    middle = np.float32(SCENE_MIDDLE_GREY)
    decode_max = np.float32(SCENE_CURVE_DECODE_MAX)
    work_max = np.float32(SCENE_CURVE_WORK_MAX)
    slope_max = np.float32(SCENE_CURVE_ZERO_SLOPE_MAX)
    shoulder_x = np.float32(SCENE_CURVE_SHOULDER_ENCODE_START)
    shoulder_width = np.float32(np.float32(1.0) - shoulder_x)
    shoulder_y = np.float32(middle * shoulder_x / shoulder_width)
    shoulder_tangent = np.float32(middle / shoulder_width)
    return (
        middle,
        decode_max,
        work_max,
        slope_max,
        shoulder_x,
        shoulder_width,
        shoulder_y,
        shoulder_tangent,
    )


def _shoulder_decode_f32(t: np.float32) -> np.float32:
    _, decode_max, _, _, _, _, shoulder_y, shoulder_tangent = _constants_f32()
    bounded = np.minimum(np.maximum(np.float32(t), np.float32(0.0)), np.float32(1.0))
    t2 = np.float32(bounded * bounded)
    t3 = np.float32(t2 * bounded)
    return np.float32(
        np.float32(np.float32(2.0) * t3 - np.float32(3.0) * t2 + np.float32(1.0))
        * shoulder_y
        + np.float32(t3 - np.float32(2.0) * t2 + bounded) * shoulder_tangent
        + np.float32(-np.float32(2.0) * t3 + np.float32(3.0) * t2) * decode_max
    )


def _shoulder_derivative_f32(t: np.float32) -> np.float32:
    _, decode_max, _, _, _, _, shoulder_y, shoulder_tangent = _constants_f32()
    bounded = np.minimum(np.maximum(np.float32(t), np.float32(0.0)), np.float32(1.0))
    t2 = np.float32(bounded * bounded)
    return np.float32(
        np.float32(np.float32(6.0) * t2 - np.float32(6.0) * bounded) * shoulder_y
        + np.float32(
            np.float32(3.0) * t2 - np.float32(4.0) * bounded + np.float32(1.0)
        )
        * shoulder_tangent
        + np.float32(-np.float32(6.0) * t2 + np.float32(6.0) * bounded)
        * decode_max
    )


def scene_curve_decode_f32(value: np.float32 | float) -> np.float32:
    middle, decode_max, _, _, shoulder_x, shoulder_width, _, _ = _constants_f32()
    bounded = np.minimum(np.maximum(np.float32(value), np.float32(0.0)), np.float32(1.0))
    if bounded <= shoulder_x:
        denominator = np.maximum(np.float32(1.0) - bounded, np.float32(1e-6))
        return np.float32(middle * bounded / denominator)
    t = np.float32((bounded - shoulder_x) / shoulder_width)
    return np.minimum(
        np.maximum(_shoulder_decode_f32(t), np.float32(0.0)), decode_max
    )


def scene_curve_encode_f32(value: np.float32 | float) -> np.float32:
    middle, decode_max, _, _, shoulder_x, shoulder_width, shoulder_y, _ = _constants_f32()
    positive = np.minimum(np.maximum(np.float32(value), np.float32(0.0)), decode_max)
    if positive <= shoulder_y:
        rational = np.float32(positive / np.float32(positive + middle))
        return np.minimum(rational, shoulder_x)

    low = np.float32(0.0)
    high = np.float32(1.0)
    for _ in range(8):
        midpoint = np.float32(np.float32(0.5) * np.float32(low + high))
        if _shoulder_decode_f32(midpoint) < positive:
            low = midpoint
        else:
            high = midpoint
    low_encoded = np.float32(shoulder_x + shoulder_width * low)
    high_encoded = np.float32(shoulder_x + shoulder_width * high)
    low_error = np.abs(np.float32(scene_curve_decode_f32(low_encoded) - positive))
    high_error = np.abs(np.float32(scene_curve_decode_f32(high_encoded) - positive))
    return high_encoded if high_error < low_error else low_encoded


def scene_curve_decode_slope_scale_f32(encoded_endpoint: float) -> np.float32:
    middle, _, _, _, shoulder_x, shoulder_width, _, _ = _constants_f32()
    endpoint = np.float32(encoded_endpoint)
    bounded = np.minimum(np.maximum(endpoint, np.float32(0.0)), np.float32(1.0))
    if bounded != endpoint:
        return np.float32(0.0)
    if bounded <= shoulder_x:
        denominator = np.maximum(np.float32(1.0) - bounded, np.float32(1e-6))
        return np.float32(np.float32(1.0) / np.float32(denominator * denominator))
    t = np.float32((bounded - shoulder_x) / shoulder_width)
    decoded_derivative = np.float32(_shoulder_derivative_f32(t) / shoulder_width)
    return np.maximum(np.float32(decoded_derivative / middle), np.float32(0.0))


def limited_endpoint_tangent_f32(encoded_endpoint: float, encoded_slope: float) -> np.float32:
    _, _, _, slope_max, _, _, _, _ = _constants_f32()
    scale = scene_curve_decode_slope_scale_f32(encoded_endpoint)
    if scale <= np.float32(1e-12):
        return np.float32(0.0)
    encoded_limit = np.float32(slope_max / scale)
    return np.minimum(
        np.maximum(np.float32(encoded_slope), -encoded_limit), encoded_limit
    )


def decoded_scene_curve_zero_slope_f32(encoded_black: float, encoded_slope: float) -> np.float32:
    _, _, _, slope_max, _, _, _, _ = _constants_f32()
    scale = scene_curve_decode_slope_scale_f32(encoded_black)
    limited = limited_endpoint_tangent_f32(encoded_black, encoded_slope)
    return np.minimum(np.maximum(np.float32(limited * scale), -slope_max), slope_max)


def clamp_scene_curve_value(value: float) -> float:
    return min(max(value, -SCENE_CURVE_WORK_MAX), SCENE_CURVE_WORK_MAX)


def limit_scene_curve_rgb_ratio_preserving(
    value: tuple[float, float, float],
) -> tuple[float, float, float]:
    peak = max(abs(channel) for channel in value)
    scale = min(1.0, SCENE_CURVE_WORK_MAX / max(peak, 1e-12))
    return tuple(
        min(max(channel * scale, -SCENE_CURVE_WORK_MAX), SCENE_CURVE_WORK_MAX)
        for channel in value
    )


def remap_with_black_offset(
    rgb: tuple[float, float, float],
    adjusted_luminance: float,
    black_luminance: float,
    zero_slope: float,
) -> tuple[float, float, float]:
    luminance = dot(rgb)
    black = max(black_luminance, 0.0)
    if luminance <= 0.0:
        return limit_scene_curve_rgb_ratio_preserving(
            tuple(black + channel * zero_slope for channel in rgb)
        )
    mapped = max(adjusted_luminance, black)
    chromatic_luminance = mapped - black
    return limit_scene_curve_rgb_ratio_preserving(
        tuple(black + channel * chromatic_luminance / luminance for channel in rgb)
    )


def channel_extension(value: float, black: float, zero_slope: float) -> float:
    if value < 0.0:
        return black + value * zero_slope
    return black + value * zero_slope + 0.25 * value * value


def max_channel_error(
    left: tuple[float, float, float], right: tuple[float, float, float]
) -> float:
    return max(abs(a - b) for a, b in zip(left, right))


def final_encoded_values(count: int) -> list[np.float32]:
    value = np.float32(1.0)
    values: list[np.float32] = []
    for _ in range(count):
        value = np.nextafter(value, np.float32(0.0), dtype=np.float32)
        values.append(value)
    return values



def first_segment_output_f32(
    scene_value: np.float32,
    endpoint: np.float32,
    next_point: np.float32,
    width: np.float32 = np.float32(0.005),
) -> np.float32:
    raw_tangent = np.float32((next_point - endpoint) / width)
    tangent0 = limited_endpoint_tangent_f32(float(endpoint), float(raw_tangent))
    encoded_input = scene_curve_encode_f32(scene_value)
    t = np.minimum(np.maximum(np.float32(encoded_input / width), np.float32(0.0)), np.float32(1.0))
    t2 = np.float32(t * t)
    t3 = np.float32(t2 * t)
    m0 = np.float32(tangent0 * width)
    # A following segment with the opposite or flat trend gives a zero interior
    # tangent under the shader's monotonic Hermite rule.
    m1 = np.float32(0.0)
    encoded_output = np.float32(
        np.float32(np.float32(2.0) * t3 - np.float32(3.0) * t2 + np.float32(1.0)) * endpoint
        + np.float32(t3 - np.float32(2.0) * t2 + t) * m0
        + np.float32(-np.float32(2.0) * t3 + np.float32(3.0) * t2) * next_point
        + np.float32(t3 - t2) * m1
    )
    encoded_output = np.minimum(
        np.maximum(encoded_output, np.minimum(endpoint, next_point)),
        np.maximum(endpoint, next_point),
    )
    return scene_curve_decode_f32(encoded_output)

def test_soft_ceiling_is_finite_monotonic_and_half_float_safe() -> None:
    encoded = list(reversed(final_encoded_values(128))) + [np.float32(1.0)]
    decoded = [scene_curve_decode_f32(value) for value in encoded]
    assert all(np.isfinite(value) for value in decoded)
    assert all(np.float32(0.0) <= value <= np.float32(SCENE_CURVE_DECODE_MAX) for value in decoded)
    assert all(left <= right for left, right in zip(decoded, decoded[1:]))
    assert float(decoded[-1]) == SCENE_CURVE_DECODE_MAX < HALF_FLOAT_MAX

    adjacent = [float(right - left) for left, right in zip(decoded, decoded[1:])]
    assert max(adjacent) < 160.0
    stored = [np.float16(value) for value in decoded]
    assert all(np.isfinite(value) for value in stored)
    stored_steps = [abs(float(right) - float(left)) for left, right in zip(stored, stored[1:])]
    assert max(stored_steps) <= 160.0


def test_shoulder_join_matches_value_and_first_derivative_in_f32() -> None:
    middle, _, _, _, shoulder_x, shoulder_width, shoulder_y, shoulder_tangent = _constants_f32()
    rational_value = np.float32(middle * shoulder_x / shoulder_width)
    rational_derivative = np.float32(middle / np.float32(shoulder_width * shoulder_width))
    shoulder_value = _shoulder_decode_f32(np.float32(0.0))
    shoulder_derivative = np.float32(_shoulder_derivative_f32(np.float32(0.0)) / shoulder_width)
    assert shoulder_value == shoulder_y == rational_value
    assert shoulder_tangent == np.float32(middle / shoulder_width)
    assert math.isclose(float(shoulder_derivative), float(rational_derivative), rel_tol=2e-6)
    assert _shoulder_derivative_f32(np.float32(1.0)) == np.float32(0.0)


def test_encode_decode_pair_preserves_identity_with_expected_f32_quantization() -> None:
    samples = (0.0, 0.1845, 1.0, 100.0, 8192.0, 20000.0, 24000.0, 30000.0, 32768.0)
    for value in samples:
        encoded = scene_curve_encode_f32(value)
        decoded = scene_curve_decode_f32(encoded)
        assert np.isfinite(encoded) and np.isfinite(decoded)
        assert np.float32(0.0) <= encoded <= np.float32(1.0)
        tolerance = 2e-3 if value < 1000.0 else 160.0
        assert abs(float(decoded) - value) <= tolerance


def _adjacent_f32_values(center: np.float32, each_side: int) -> list[np.float32]:
    lower: list[np.float32] = []
    value = np.float32(center)
    for _ in range(each_side):
        value = np.nextafter(value, np.float32(-np.inf), dtype=np.float32)
        lower.append(value)
    upper: list[np.float32] = []
    value = np.float32(center)
    for _ in range(each_side):
        value = np.nextafter(value, np.float32(np.inf), dtype=np.float32)
        upper.append(value)
    return list(reversed(lower)) + [np.float32(center)] + upper


def test_encode_decode_composition_is_monotonic_across_shoulder_join() -> None:
    _, _, _, _, shoulder_x, _, shoulder_y, _ = _constants_f32()
    values = _adjacent_f32_values(shoulder_y, 256)
    encoded = [scene_curve_encode_f32(value) for value in values]
    decoded = [scene_curve_decode_f32(value) for value in encoded]

    assert all(np.isfinite(value) for value in encoded)
    assert all(np.isfinite(value) for value in decoded)
    assert all(left <= right for left, right in zip(encoded, encoded[1:]))
    assert all(left <= right for left, right in zip(decoded, decoded[1:]))
    assert encoded[256] <= shoulder_x

    adjacent = [float(right - left) for left, right in zip(decoded, decoded[1:])]
    shoulder_step = float(
        scene_curve_decode_f32(np.nextafter(shoulder_x, np.float32(1.0), dtype=np.float32))
        - scene_curve_decode_f32(shoulder_x)
    )
    assert min(adjacent) >= 0.0
    assert max(adjacent) <= max(shoulder_step, 160.0)

    stored = [np.float16(value) for value in decoded]
    assert all(np.isfinite(value) for value in stored)
    assert all(left <= right for left, right in zip(stored, stored[1:]))


def test_first_post_join_encoder_transition_is_monotonic_and_one_step_only() -> None:
    _, _, _, _, shoulder_x, _, shoulder_y, _ = _constants_f32()
    before_value = np.float32(shoulder_y)
    before_encoded = scene_curve_encode_f32(before_value)
    assert before_encoded == shoulder_x

    after_value = before_value
    after_encoded = before_encoded
    for _ in range(100_000):
        candidate = np.nextafter(after_value, np.float32(np.inf), dtype=np.float32)
        candidate_encoded = scene_curve_encode_f32(candidate)
        if candidate_encoded > before_encoded:
            before_value = after_value
            after_value = candidate
            after_encoded = candidate_encoded
            break
        after_value = candidate
    else:
        raise AssertionError("shoulder inverse did not advance within 100,000 f32 values")

    before_decoded = scene_curve_decode_f32(before_encoded)
    after_decoded = scene_curve_decode_f32(after_encoded)
    next_encoded = np.nextafter(shoulder_x, np.float32(1.0), dtype=np.float32)
    unavoidable_step = np.float32(
        scene_curve_decode_f32(next_encoded) - scene_curve_decode_f32(shoulder_x)
    )

    assert after_value > before_value
    assert after_encoded == next_encoded
    assert after_decoded >= before_decoded
    assert np.float32(after_decoded - before_decoded) <= unavoidable_step

    before_half = np.float16(before_decoded)
    after_half = np.float16(after_decoded)
    assert np.isfinite(before_half) and np.isfinite(after_half)
    assert after_half >= before_half
    assert float(after_half) - float(before_half) <= 160.0




def test_final_128_endpoints_have_bounded_scene_domain_slopes() -> None:
    raw_tangents = (-200.0, -1.0, -0.01, 0.01, 1.0, 200.0)
    for endpoint in final_encoded_values(128):
        for raw_tangent in raw_tangents:
            limited = limited_endpoint_tangent_f32(float(endpoint), raw_tangent)
            scene_slope = decoded_scene_curve_zero_slope_f32(float(endpoint), raw_tangent)
            assert np.isfinite(limited) and np.isfinite(scene_slope)
            assert abs(float(scene_slope)) <= SCENE_CURVE_ZERO_SLOPE_MAX
            if raw_tangent != 0.0 and scene_slope != 0.0:
                assert math.copysign(1.0, float(scene_slope)) == math.copysign(1.0, raw_tangent)


def test_first_negative_half_steps_are_bounded_after_float16_storage() -> None:
    half_step = np.float32(np.nextafter(np.float16(0.0), np.float16(1.0), dtype=np.float16))
    raw_tangents = (-200.0, -1.0, -0.01, 0.01, 1.0, 200.0)
    for endpoint in final_encoded_values(128):
        black = scene_curve_decode_f32(endpoint)
        stored_black = np.float16(black)
        for raw_tangent in raw_tangents:
            scene_slope = decoded_scene_curve_zero_slope_f32(float(endpoint), raw_tangent)
            previous_stored = stored_black
            for multiple in range(1, 9):
                scene_input = np.float32(-half_step * np.float32(multiple))
                output = np.float32(black + np.float32(scene_input * scene_slope))
                stored = np.float16(output)
                assert np.isfinite(output) and np.isfinite(stored)
                assert abs(float(output) - float(black)) <= 0.51
                assert abs(float(stored) - float(stored_black)) <= 32.0
                assert abs(float(stored) - float(previous_stored)) <= 32.0
                previous_stored = stored



def test_limited_tangent_bounds_actual_first_segment_on_both_sides_of_zero() -> None:
    half_step = np.float32(np.nextafter(np.float16(0.0), np.float16(1.0), dtype=np.float16))
    endpoint = final_encoded_values(1)[0]
    black = scene_curve_decode_f32(endpoint)
    raw_tangent = np.float32((np.float32(0.0) - endpoint) / np.float32(0.005))
    scene_slope = decoded_scene_curve_zero_slope_f32(float(endpoint), float(raw_tangent))
    for multiple in range(1, 9):
        magnitude = np.float32(half_step * np.float32(multiple))
        negative = np.float32(black - np.float32(magnitude * scene_slope))
        positive = first_segment_output_f32(magnitude, endpoint, np.float32(0.0))
        assert np.isfinite(negative) and np.isfinite(positive)
        assert abs(float(negative) - float(black)) <= 0.51
        assert abs(float(positive) - float(black)) <= 128.0
        assert abs(float(np.float16(negative)) - float(np.float16(black))) <= 32.0
        assert abs(float(np.float16(positive)) - float(np.float16(black))) <= 128.0

def test_typical_negative_value_has_documented_maximum_amplification() -> None:
    for endpoint in final_encoded_values(128):
        black = scene_curve_decode_f32(endpoint)
        for raw_tangent in (-200.0, -1.0, 1.0, 200.0):
            scene_slope = decoded_scene_curve_zero_slope_f32(float(endpoint), raw_tangent)
            output = np.float32(black + np.float32(np.float32(-1e-4) * scene_slope))
            assert np.isfinite(output)
            assert abs(float(output) - float(black)) <= 105.0
            assert abs(float(np.float16(output)) - float(np.float16(black))) <= 128.0


def test_endpoint_tangent_limit_applies_same_derivative_on_both_sides_of_zero() -> None:
    endpoint = float(final_encoded_values(1)[0])
    raw_tangent = -200.0
    limited = float(limited_endpoint_tangent_f32(endpoint, raw_tangent))
    slope_scale = float(scene_curve_decode_slope_scale_f32(endpoint))
    expected_scene_slope = limited * slope_scale
    actual_scene_slope = float(decoded_scene_curve_zero_slope_f32(endpoint, raw_tangent))
    assert math.isclose(actual_scene_slope, expected_scene_slope, rel_tol=1e-6)
    assert math.isclose(abs(actual_scene_slope), SCENE_CURVE_ZERO_SLOPE_MAX, rel_tol=1e-6)


def test_master_headroom_limiter_preserves_extreme_rgb_ratios() -> None:
    candidate = (75817.0, 18954.0, 0.0)
    limited = limit_scene_curve_rgb_ratio_preserving(candidate)
    assert max(abs(channel) for channel in limited) == SCENE_CURVE_WORK_MAX
    assert math.isclose(limited[0] / limited[1], candidate[0] / candidate[1], rel_tol=1e-12)
    assert limited[2] == 0.0

    independently_clamped = tuple(
        min(max(channel, -SCENE_CURVE_WORK_MAX), SCENE_CURVE_WORK_MAX)
        for channel in candidate
    )
    assert not math.isclose(
        independently_clamped[0] / independently_clamped[1],
        candidate[0] / candidate[1],
        rel_tol=1e-3,
    )


def test_master_curve_reference_is_continuous_through_representable_near_black() -> None:
    black = 0.0160435
    slope = 0.60
    samples: list[tuple[float, tuple[float, float, float]]] = []
    for y in (0.0, 5e-8, 9.99e-8, 1e-7, 1.001e-7, 5e-7):
        rgb = (y / LUMA[0], 0.0, 0.0) if y > 0.0 else (0.0, 0.0, 0.0)
        mapped = black + slope * y
        samples.append((y, remap_with_black_offset(rgb, mapped, black, slope)))

    for (_, previous), (_, current) in zip(samples, samples[1:]):
        assert max_channel_error(previous, current) < 2e-6
    assert all(math.isclose(dot(output), black + slope * y, abs_tol=1e-12) for y, output in samples)


def test_master_curve_reference_is_continuous_across_opponent_zero_luminance() -> None:
    black = 0.0160435
    slope = 0.40
    opponent = (1.0, -LUMA[0] / LUMA[1], 0.0)
    expected_limit = tuple(black + slope * channel for channel in opponent)

    previous_error = float("inf")
    for epsilon in (1e-5, 1e-7, 1e-9):
        below_rgb = tuple(channel - epsilon for channel in opponent)
        above_rgb = tuple(channel + epsilon for channel in opponent)
        below = remap_with_black_offset(below_rgb, black, black, slope)
        above_y = dot(above_rgb)
        assert dot(below_rgb) < 0.0 < above_y
        above = remap_with_black_offset(above_rgb, black + slope * above_y, black, slope)
        error = max(max_channel_error(below, expected_limit), max_channel_error(above, expected_limit))
        assert error < previous_error
        previous_error = error
    assert previous_error < 1e-8


def test_channel_curve_reference_has_no_negative_zero_positive_jump() -> None:
    black = 0.0160435
    slope = 0.85
    epsilon = 1e-12
    below = channel_extension(-epsilon, black, slope)
    at_zero = channel_extension(0.0, black, slope)
    above = channel_extension(epsilon, black, slope)
    assert math.isclose(at_zero - below, slope * epsilon, rel_tol=0.0, abs_tol=1e-15)
    assert math.isclose(above - at_zero, slope * epsilon, rel_tol=0.0, abs_tol=1e-15)


def test_descending_master_endpoint_uses_effective_black_floor_slope() -> None:
    black = 0.25
    effective_master_slope = 0.0
    opponent = (1.0, -LUMA[0] / LUMA[1], 0.0)
    below = remap_with_black_offset(
        tuple(channel - 1e-9 for channel in opponent), black, black, effective_master_slope
    )
    above = remap_with_black_offset(
        tuple(channel + 1e-9 for channel in opponent), black, black, effective_master_slope
    )
    expected = (black, black, black)
    assert max_channel_error(below, expected) < 1e-12
    assert max_channel_error(above, expected) < 1e-12




