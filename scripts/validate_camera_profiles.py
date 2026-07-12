#!/usr/bin/env python3
"""Static and numerical checks for the DNG/DCP/ICC camera-profile pipeline.

This complements `cargo test` on hosts where the Rust/LibRaw toolchain is not
installed. It verifies cross-file GPU ABI invariants, transform ordering, DCP
container/tag support, and the fixed colour-space matrices used by WGSL.
"""
from __future__ import annotations

import math
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]


def read(relative: str) -> str:
    return (ROOT / relative).read_text(encoding="utf-8")


class Checks:
    def __init__(self) -> None:
        self.results: list[tuple[bool, str]] = []

    def check(self, condition: bool, message: str) -> None:
        self.results.append((bool(condition), message))

    def require_in_order(self, text: str, needles: list[str], message: str) -> None:
        cursor = -1
        ok = True
        for needle in needles:
            cursor = text.find(needle, cursor + 1)
            if cursor < 0:
                ok = False
                break
        self.check(ok, message)

    def finish(self) -> int:
        passed = sum(ok for ok, _ in self.results)
        for ok, message in self.results:
            print(f"[{'PASS' if ok else 'FAIL'}] {message}")
        print(f"\n{passed}/{len(self.results)} camera-profile checks passed")
        return 0 if passed == len(self.results) else 1


def strip_comments_and_strings(source: str) -> str:
    """Remove comments/string bodies while preserving delimiters and newlines."""
    out: list[str] = []
    i = 0
    state = "code"
    block_depth = 0
    while i < len(source):
        ch = source[i]
        nxt = source[i + 1] if i + 1 < len(source) else ""
        if state == "code":
            if ch == "/" and nxt == "/":
                state = "line"
                out.extend("  ")
                i += 2
                continue
            if ch == "/" and nxt == "*":
                state = "block"
                block_depth = 1
                out.extend("  ")
                i += 2
                continue
            if ch == '"':
                state = "string"
                out.append(" ")
                i += 1
                continue
            if ch == "'":
                # Rust lifetimes are not quoted strings. Treat as a character
                # literal only when a closing quote is nearby.
                closing = source.find("'", i + 1, min(i + 8, len(source)))
                if closing != -1:
                    state = "char"
                    out.append(" ")
                    i += 1
                    continue
            out.append(ch)
            i += 1
        elif state == "line":
            if ch == "\n":
                state = "code"
                out.append("\n")
            else:
                out.append(" ")
            i += 1
        elif state == "block":
            if ch == "/" and nxt == "*":
                block_depth += 1
                out.extend("  ")
                i += 2
            elif ch == "*" and nxt == "/":
                block_depth -= 1
                out.extend("  ")
                i += 2
                if block_depth == 0:
                    state = "code"
            else:
                out.append("\n" if ch == "\n" else " ")
                i += 1
        elif state in {"string", "char"}:
            if ch == "\\":
                out.extend("  ")
                i += 2
            elif (state == "string" and ch == '"') or (state == "char" and ch == "'"):
                state = "code"
                out.append(" ")
                i += 1
            else:
                out.append("\n" if ch == "\n" else " ")
                i += 1
    return "".join(out)


WGSL_RESERVED_WORDS = frozenset(
    """
    NULL Self abstract active alignas alignof as asm asm_fragment async attribute auto await
    become cast catch class co_await co_return co_yield coherent column_major common compile
    compile_fragment concept const_cast consteval constexpr constinit crate debugger decltype
    delete demote demote_to_helper do dynamic_cast enum explicit export extends extern external
    fallthrough filter final finally friend from fxgroup get goto groupshared highp impl
    implements import inline instanceof interface layout lowp macro macro_rules match mediump
    meta mod module move mut mutable namespace new nil noexcept noinline nointerpolation
    non_coherent noncoherent noperspective null nullptr of operator package packoffset partition
    pass patch pixelfragment precise precision premerge priv protected pub public readonly ref
    regardless register reinterpret_cast require resource restrict self set shared sizeof smooth
    snorm static static_assert static_cast std subroutine super target template this thread_local
    throw trait try type typedef typeid typename typeof union unless unorm unsafe unsized use
    using varying virtual volatile wgsl where with writeonly yield
    """.split()
)


def wgsl_reserved_word_uses(source: str) -> list[str]:
    clean = strip_comments_and_strings(source)
    identifiers = re.findall(r"[A-Za-z_][A-Za-z0-9_]*", clean)
    return sorted(set(identifiers) & WGSL_RESERVED_WORDS)


def balanced(source: str) -> bool:
    clean = strip_comments_and_strings(source)
    pairs = {")": "(", "]": "[", "}": "{"}
    stack: list[str] = []
    for ch in clean:
        if ch in "([{":
            stack.append(ch)
        elif ch in pairs:
            if not stack or stack.pop() != pairs[ch]:
                return False
    return not stack


def rust_struct_fields(source: str, name: str) -> list[str]:
    match = re.search(rf"pub struct {re.escape(name)}\s*\{{(.*?)\n\}}", source, re.S)
    if not match:
        return []
    return re.findall(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*:", match.group(1), re.M)


def wgsl_struct_fields(source: str, name: str) -> list[str]:
    match = re.search(rf"struct {re.escape(name)}\s*\{{(.*?)\n\}}", source, re.S)
    if not match:
        return []
    return re.findall(r"^\s*([A-Za-z_][A-Za-z0-9_]*)\s*:", match.group(1), re.M)


def matmul(a: list[list[float]], b: list[list[float]]) -> list[list[float]]:
    return [[sum(a[r][k] * b[k][c] for k in range(3)) for c in range(3)] for r in range(3)]


def max_identity_error(matrix: list[list[float]]) -> float:
    return max(abs(matrix[r][c] - (1.0 if r == c else 0.0)) for r in range(3) for c in range(3))


def parse_wgsl_matrix(source: str, name: str) -> list[list[float]]:
    match = re.search(
        rf"const {re.escape(name)}: mat3x3<f32> = mat3x3<f32>\((.*?)\);",
        source,
        re.S,
    )
    if not match:
        raise ValueError(f"missing matrix {name}")
    columns = []
    for vector in re.findall(r"vec3<f32>\(([^)]*)\)", match.group(1)):
        columns.append([float(x.strip()) for x in vector.split(",") if x.strip()])
    if len(columns) != 3 or any(len(column) != 3 for column in columns):
        raise ValueError(f"invalid matrix {name}")
    return [[columns[c][r] for c in range(3)] for r in range(3)]


def main() -> int:
    c = Checks()
    rust_profile = read("src/pipeline/color_profile.rs")
    raw_loader = read("src/pipeline/raw_loader.rs")
    gpu = read("src/pipeline/gpu.rs")
    build_rs = read("build.rs")
    common = read("src/shaders/common.wgsl")
    profile = read("src/shaders/profile.wgsl")
    adjustments = read("src/shaders/adjustments.wgsl")
    tone_analysis = read("src/shaders/tone_analysis.wgsl")
    tonemap = read("src/shaders/tonemap.wgsl")

    for relative, source in [
        ("src/pipeline/color_profile.rs", rust_profile),
        ("src/pipeline/raw_loader.rs", raw_loader),
        ("src/pipeline/gpu.rs", gpu),
        ("build.rs", build_rs),
        ("src/shaders/common.wgsl", common),
        ("src/shaders/profile.wgsl", profile),
        ("src/shaders/adjustments.wgsl", adjustments),
        ("src/shaders/tone_analysis.wgsl", tone_analysis),
        ("src/shaders/tonemap.wgsl", tonemap),
    ]:
        c.check(balanced(source), f"balanced delimiters in {relative}")

    shader_sources = {
        path.relative_to(ROOT).as_posix(): path.read_text(encoding="utf-8")
        for path in sorted((ROOT / "src/shaders").glob("*.wgsl"))
    }
    reserved_uses = {
        relative: uses
        for relative, source in shader_sources.items()
        if (uses := wgsl_reserved_word_uses(source))
    }
    c.check(
        not reserved_uses,
        "WGSL sources contain no reserved words as identifiers"
        + (f": {reserved_uses}" if reserved_uses else ""),
    )

    c.check(
        '"src/shaders/profile.wgsl"' in build_rs,
        "build script tracks camera-profile shader changes",
    )

    rust_fields = rust_struct_fields(gpu, "GpuParams")
    wgsl_fields = wgsl_struct_fields(common, "Params")
    c.check(bool(rust_fields) and rust_fields == wgsl_fields, "Rust/WGSL parameter ABI field order matches")
    c.check(
        rust_fields[-5:] == ["profile_hue_sat", "profile_look", "profile_tone", "output_lut", "profile_flags"],
        "DCP/ICC metadata occupies the final five uniform vec4 slots",
    )

    c.require_in_order(
        gpu,
        [
            'const SHADER_TONE_ANALYSIS',
            'include_str!("../shaders/color.wgsl")',
            'include_str!("../shaders/profile.wgsl")',
            'include_str!("../shaders/tone_analysis.wgsl")',
        ],
        "adaptive tone shader includes camera-profile operations",
    )
    c.check("@group(0) @binding(20) var<storage, read> profile_data" in profile, "profile LUT storage binding is declared")
    c.check(gpu.count("storage_buffer_entry(20, true)") >= 2, "profile buffer is visible to tone analysis and final adjustments")
    c.check(gpu.count("binding: 20,\n                    resource: profile_buffer.as_entire_binding()") >= 2, "profile buffer is bound in both GPU paths")

    c.require_in_order(
        adjustments,
        [
            "apply_profile_hue_sat(working)",
            "exp2(profile_exposure_ev)",
            "apply_exposure(rgb)",
            "apply_profile_look(rgb)",
            "apply_profile_tone_curve(rgb)",
            "display_render",
        ],
        "render order is HueSat -> default exposure -> controls -> LookTable -> profile curve -> display transform",
    )
    c.require_in_order(
        tone_analysis,
        [
            "apply_profile_hue_sat(working)",
            "exp2(profile_exposure_ev)",
            "apply_profile_look(exposed)",
            "apply_profile_tone_curve(looked)",
        ],
        "adaptive histogram includes all fixed DCP rendering stages",
    )
    sigmoid_rust = read("src/pipeline/sigmoid.rs")
    c.check(
        "contrast: 1.5" in sigmoid_rust
        and "pub const MIDDLE_GREY: f32 = 0.1845" in sigmoid_rust
        and "5.0f32.powf(-skew)" in sigmoid_rust,
        "darktable sigmoid defaults and generalized log-logistic coefficients are present",
    )
    c.require_in_order(
        tonemap,
        [
            "let locally_shaped = apply_local_basic_tone(rgb, pos)",
            "let mapped = darktable_sigmoid(locally_shaped)",
            "apply_output_lut(mapped)",
        ],
        "darktable sigmoid runs before the bounded ICC LUT",
    )
    c.check(
        "preserve_hue_and_energy" in tonemap
        and "hyperbolic_chroma" in tonemap
        and "sigmoid_rgb_ratio" in tonemap,
        "both darktable sigmoid color-processing paths are present",
    )

    c.check('signature == *b"IIRC" || signature == *b"MMCR"' in rust_profile, "standalone DCP camera-profile header is recognized")
    c.check("42 | 0x4352" in rust_profile, "TIFF reader accepts DCP magic 0x4352")
    c.check("expected 9 or 12" in rust_profile and "expected 9 or 16" in rust_profile, "three- and four-plane DCP matrices are accepted")

    required_tags = {
        "COLOR_MATRIX_1": 50721,
        "COLOR_MATRIX_2": 50722,
        "CAMERA_CALIBRATION_1": 50723,
        "CAMERA_CALIBRATION_2": 50724,
        "CALIBRATION_ILLUMINANT_1": 50778,
        "CALIBRATION_ILLUMINANT_2": 50779,
        "PROFILE_HUE_SAT_MAP_DATA_1": 50938,
        "PROFILE_HUE_SAT_MAP_DATA_2": 50939,
        "PROFILE_TONE_CURVE": 50940,
        "FORWARD_MATRIX_1": 50964,
        "FORWARD_MATRIX_2": 50965,
        "PROFILE_LOOK_TABLE_DATA": 50982,
        "BASELINE_EXPOSURE_OFFSET": 51109,
    }
    c.check(
        all(re.search(rf"const {name}: u16 = {value};", rust_profile) for name, value in required_tags.items()),
        "required DNG/DCP tag numbers are present",
    )
    c.check("calibration_is_compatible" in rust_profile and "calibration_compatible" in raw_loader, "profile calibration signatures gate CameraCalibration")
    c.check(
        "interpolated_parsed_dng_profile" in raw_loader
        and "parsed_profile" in raw_loader
        and ".or_else(||" in raw_loader,
        "directly parsed DNG/DCP matrices are applied before the LibRaw fallback",
    )
    c.check("analog_balance_matrix(color.dng_levels.analogbalance)" in raw_loader, "DNG AnalogBalance participates in the matrix chain")
    c.check("multiply_4x4(analog_balance, profile.calibration)" in raw_loader, "AB * CC ordering is explicit")
    c.check("multiply_4x4_4x3(abcc, profile.color_matrix)" in raw_loader, "AB * CC * CM ordering is explicit")
    c.check("multiply_3x4_4x4(balanced_reference_to_xyz, inverse_abcc)" in raw_loader, "ForwardMatrix path applies FM * D * inverse(AB * CC)")
    c.check("mired_interpolation_weight" in raw_loader and "1_000_000.0 /" in raw_loader, "dual-illuminant interpolation is reciprocal-CCT based")
    c.check("baseline_exposure_offset += baseline_exposure" in raw_loader, "BaselineExposure and profile offset are combined")
    c.check(
        "baseline_exposure.is_finite() && baseline_exposure > -999.0" in raw_loader,
        "LibRaw's missing BaselineExposure sentinel cannot black out proprietary RAW previews",
    )
    c.check(
        "size_of::<super::GpuParams>(), 448" in gpu
        and "offset_of!(super::GpuParams, sigmoid_curve), 80" in gpu
        and "offset_of!(super::GpuParams, sigmoid_power), 96" in gpu
        and "offset_of!(super::GpuParams, profile_hue_sat), 368" in gpu
        and "offset_of!(super::GpuParams, profile_flags), 432" in gpu,
        "camera-profile uniform ABI regression test covers the appended metadata block",
    )

    c.check("ProfileHueSatMapEncoding" not in profile, "shader receives normalized encoding flags rather than reparsing metadata")
    c.check("unsupported DCP profile-table encoding" in rust_profile, "unknown DCP table encodings are rejected")
    c.check("value outermost, hue next, saturation innermost" in profile, "DCP HSV table indexing documents the specified storage order")
    c.check("hsv.z = profile_srgb_encode_value" in profile and "hsv.z = profile_srgb_decode_value" in profile, "SDR DCP encoding is applied to HSV value only")
    c.check("encoding == 1u && map_info.z > 1u" in profile, "DCP encoding tags are ignored for 2.5D maps")
    c.check("zero-saturation entry has value scale" in rust_profile, "DCP zero-saturation value-scale invariant is validated")
    c.check("dcp.interpolated_hue_sat_map(interpolation_weight)" in rust_profile, "HueSat maps use the matrix interpolation weight")
    c.check("PROFILE_TONE_LUT_SIZE: usize = 4096" in rust_profile, "profile tone curve is sampled at 4096 entries")
    c.check(
        "sample_natural_cubic" in rust_profile
        and "natural_cubic_second_derivatives" in rust_profile
        and "zero second derivative" in rust_profile,
        "profile tone curve uses a natural cubic spline",
    )
    c.check(
        "SDR DCP tone curves must start at (0, 0) and end at (1, 1)" in rust_profile,
        "SDR profile tone-curve endpoints are validated",
    )

    c.check("&bytes[36..40] != b\"acsp\"" in rust_profile, "ICC profile signature is validated")
    c.check("matches!(bytes[8], 2 | 4)" in rust_profile, "ICC v2/v4 profile versions are enforced")
    c.check(
        'profile_class != b"mntr"' in rust_profile
        and 'profile_class != b"prtr"' in rust_profile
        and 'profile_class != b"spac"' in rust_profile,
        "ICC display/output/color-space profile classes are enforced",
    )
    c.check("rXYZ" in rust_profile and "rTRC" in rust_profile, "ICC RGB colorants and transfer curves are parsed")
    c.check(
        "ICC transfer curve must be monotonic" in rust_profile
        and "try_for_each(TransferCurve::validate)" in rust_profile,
        "ICC transfer curves are finite and monotonic before inversion",
    )
    c.check("RenderingIntent::Perceptual" in rust_profile and "RenderingIntent::AbsoluteColorimetric" in rust_profile, "ICC rendering intents are exposed")
    c.check(
        "is_icc_lut_transform_signature" in rust_profile
        and 'b"A2B0"' in rust_profile
        and 'b"B2A0"' in rust_profile
        and 'b"D2B0"' in rust_profile
        and 'b"B2D0"' in rust_profile,
        "all standard ICC LUT transform tag families fail explicitly",
    )
    c.check("set_display_icc_profile" in gpu and "set_output_icc_profile" in gpu, "display and output ICC APIs are available")
    c.check(
        "load_raw_file_with_dcp" in raw_loader
        and "load_raw_file_with_selected_profile" in raw_loader,
        "external DCP profiles can drive RAW loading",
    )
    c.check("transform_rgb" in rust_profile, "CPU ICC LUT evaluation is available for export")

    try:
        rec_to_pro = parse_wgsl_matrix(profile, "REC2020_TO_PROPHOTO")
        pro_to_rec = parse_wgsl_matrix(profile, "PROPHOTO_TO_REC2020")
        c.check(max_identity_error(matmul(rec_to_pro, pro_to_rec)) < 2e-5, "Rec.2020/ProPhoto matrices are numerical inverses")
    except ValueError:
        c.check(False, "Rec.2020/ProPhoto matrices are numerical inverses")

    c.check("const OUTPUT_LUT_EDGE: u32 = 33" in rust_profile, "ICC GPU LUT uses a 33-cube")
    c.check("(b * lut_info.y + g) * lut_info.x + r" in profile, "GPU ICC LUT indexing matches CPU R-fastest packing")
    c.check("((b * edge + g) * edge + r)" in rust_profile, "CPU ICC LUT indexing matches GPU packing")

    return c.finish()


if __name__ == "__main__":
    sys.exit(main())
