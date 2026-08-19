# AuRaw 2.0 — Temperature, Tint, and RAW White-Balance Audit

Audit date: 2026-08-19

## Executive conclusion

The principal color-quality defect is not slider scaling. In the original AuRaw 2.0 implementation, DNG/DCP white balance was edited by changing pre-demosaic camera-channel multipliers while the camera→working matrix and dual-illuminant DCP interpolation weight remained frozen at load time. AuRaw's DNG matrix is explicitly factored to cancel the WB that was applied to the CFA. Once a different WB is applied with the old matrix, that factorization is algebraically invalid. This is a **Critical** standards and colorimetry defect and the most likely source of large camera-dependent color changes during WB edits.

Two additional **High** defects materially affect Temp/Tint quality: the original Tint coordinate copied darktable's legacy `XYZ.Y /= tint` hack, which current darktable itself documents as non-orthogonal to the Planckian locus and temperature-changing; and AuRaw approximated darktable's blackbody/daylight switch with two analytic loci that have a measurable discontinuity at 4000 K.

The patch keeps RAW WB in camera space before demosaic, retains the serialized relative mired/tint fields, but changes the internal white-point representation to CCT + signed CIE 1960 Duv; rebuilds DNG matrices and DCP HueSat interpolation from the selected white; replaces the local shader's arbitrary Bradford-domain gains with an explicit source-white→destination-white Bradford adaptation; preserves two green planes; and makes the picker robust to outliers.

## A. Complete RAW/color pipeline

| Stage | AuRaw location | Input → output | Linear? | White / adaptation | WB-dependent? | Audit |
|---|---|---|---|---|---|---|
| RAW unpack / metadata | `raw_loader/libraw_loader.rs` ~1185+ | encoded CFA → sensor codes + metadata | sensor-linear codes | none | metadata only | LibRaw `cam_mul` is As Shot; `pre_mul` is daylight balance. AuRaw delegates DNG AsShotNeutral/AsShotWhiteXY parsing to LibRaw. |
| Black subtraction + normalization | `raw_sampling.wgsl:23-42` | sensor codes → normalized camera CFA | yes | none | no | Correct order. Current `clamp(...,0,4)` discards negative post-black values; separate shadow/noise fidelity issue. |
| Global RAW WB | `raw_sampling.wgsl:44-47`; uniforms packed by `gpu.rs:975-999` | normalized CFA → WB camera CFA | yes | selected camera neutral | **yes** | Correctly pre-demosaic. G1/G2 remain distinct. |
| Highlight detection/reconstruction | `raw_sampling.wgsl:49-65`, `highlights.wgsl` | WB/raw CFA → reconstructed CFA | yes | selected WB affects thresholds | yes | Uses current WB. This is coherent but makes highlight behavior WB-dependent; real stress captures remain required. |
| Demosaic | Bayer RCD / `xtrans_demosaic.wgsl` | CFA → camera RGB | yes | none | indirectly | Correct placement: do not move global WB after demosaic. |
| Camera→working | `scene_adjustments.wgsl:169-175`, `color.wgsl`; CPU matrix from `dng_camera_to_working()` | camera RGB → Rec.2020 D65 | yes | DNG PCS D50 then D50→D65; Bradford in no-FM branch | **DNG yes; fixed matrix no** | Original stale-state bug; patched dynamic for DNG. |
| DNG matrix construction | `libraw_loader.rs:2763-2826` | camera neutral + CM/CC/AB/FM → camera→Rec.2020 | yes | D50 PCS | **yes for interpolated DNG profiles** | Core matrix directions/order are correct; selected state was wrong before patch. |
| DCP HueSatMap | `profile.wgsl:165-207` | Rec.2020 D65 ↔ ProPhoto/RIMM-like D50 table domain → Rec.2020 | yes/table HSV | D50 profile domain | **dual maps yes** | Patched live endpoint weight from selected CCT. Pre-table nonnegative gamut projection can differ from Adobe for extreme signed RGB. |
| Exposure/scene ops | scene shaders | Rec.2020 D65 → Rec.2020 D65 | yes | D65 | no (except explicit local Temp/Tint) | Local Temp/Tint is separate from RAW WB. |
| DCP LookTable | profile/view path | working RGB → profile look → working RGB | scene/view bridge | D50 profile table domain | normally no | Creative profile stage, not global WB. |
| ProfileToneCurve | `profile.wgsl:237-270` | helper exists | view-like | profile-defined | no | Parsed/implemented but no active call found. DNG describes a default profile curve that may be used; omission is a rendering-compatibility caveat, not the WB root cause. |
| AuRaw view transform | `view_transform.wgsl` / sigmoid | scene-linear → display-linear | transition | display/view white | no | Separate from WB. |
| Output ICC/LUT | profile/output path | display-linear → device/output | as defined | output white | no | Separate from WB. |

### Matrix algebra proving the original DNG failure

Let the selected sensor WB be diagonal `D(W)`. AuRaw applies `D(W)` to CFA values before demosaic. Its DNG camera matrix is therefore stored with those gains factored out:

`M(W) = T(W) · D(W)^-1`

where `T(W)` is the selected DNG camera→working transform (including selected calibration/profile state). Correct rendering is:

`M(W) · D(W) · c = T(W) · c`.

The original code loaded `M(W_as)` and then, on a WB edit, applied `D(W_new)` while leaving the matrix fixed:

`M(W_as) · D(W_new) · c = T(W_as) · D(W_as)^-1 · D(W_new) · c`.

Unless `W_new = W_as`, the diagonal factors do not cancel. This is mathematically wrong even before considering that `T(W)` itself changes for a dual-illuminant DNG profile. For an ordinary single fixed camera matrix that was not built from a WB-dependent DNG profile, keeping the underlying characterization fixed is correct; only the diagonal sensor WB changes.

## B. RAW metadata interpretation

### LibRaw-facing quantities

- `cam_mul[4]`: As Shot WB coefficients. AuRaw uses this as `base_wb`.
- `pre_mul[4]`: LibRaw daylight balance. AuRaw does not misuse it as As Shot; RawNIND daylight handling has its own path.
- `as_shot_wb_applied`: original AuRaw ignored it. Patched `libraw_loader.rs:1195-1201` rejects these inputs from the single-channel sensor-linear path to avoid double WB.
- `cdesc` / CFA map: physical planes are canonicalized to R, G1, B, G2. Second green is a calibration plane, not an independent RGB primary.
- AuRaw does **not** independently parse DNG `AsShotNeutral` / `AsShotWhiteXY`; their resolution into LibRaw color/WB state is delegated to LibRaw.

### DNG matrix direction and order

Adobe's DNG processing model defines `CM` as XYZ→reference-camera, `CC` as reference-camera→individual-camera calibration, and `AB` as the camera-coordinate AnalogBalance diagonal. Thus:

`XYZtoCamera = AB · CC · CM`.

AuRaw's `dng_camera_to_working()` and selected XYZ→camera construction use that order. With ForwardMatrix, AuRaw implements the DNG form:

`CameraToXYZ_D50 = FM · D · inverse(AB · CC)`

with `ReferenceNeutral = inverse(AB · CC) · CameraNeutral` and `D = inverse(diag(ReferenceNeutral))`. Without ForwardMatrix it pseudo-inverts `AB·CC·CM` and adapts selected white to D50 with Bradford. These directions are already correct and should not be “fixed” by transposing/inverting them again.

Limitations: parser/data structures currently contain only two matrix/calibration/HueSat endpoints (`color_profile.rs:245-258`; `dcp.rs:1-23, 127-150`). DNG 1.6+ permits a third calibration. ReductionMatrix is not parsed; this matters for genuine n>3 camera color spaces. AuRaw's R/G/B/G2 CFA abstraction is not a general four-spectral-primary camera model.

## C. Original Temp/Tint mathematics

Original source line references are against the supplied archive before this audit.

- `libraw_loader.rs:2018-2037` `adjusted_white_balance_coefficients()`: derived As Shot Temp/Tint, applied relative mired/tint offsets, generated new camera gains.
- `libraw_loader.rs:2042-2055` `white_balance_xyz_to_camera()`: for DNG, selected a **fixed 6504 K** interpolation endpoint blend.
- `libraw_loader.rs:2058-2075` `darktable_temperature_xyz()`: analytic Planckian `xy` below 4000 K, analytic D-series daylight `xy` at/above 4000 K.
- `libraw_loader.rs:2077-2081`: Tint was exactly `XYZ.Y /= tint`.
- `libraw_loader.rs:2083-2105`: XYZ→camera, reciprocal channel response, green normalization.
- `libraw_loader.rs:2107-2145`: camera multipliers→XYZ used the fixed matrix, solved temperature from **Z/X only**, then recovered Tint from Y/X.
- `raw_loader.rs:1079-1115`: WB edits returned new multipliers but explicitly reused `self.cam_to_srgb` and the load-time `camera_profile.interpolation_weight`.

The Z/X binary search is not a rigorous CCT solution. It discards one chromaticity degree of freedom, then reconstructs “tint” with a coupled ratio. The replacement projects the reconstructed white onto the Planckian locus in CIE 1960 `(u,v)` and measures signed perpendicular Duv. For dual-illuminant DNG, multiplier→white is iterative because the XYZ→camera matrix itself depends on the selected CCT, as the DNG specification requires.

### Why CIE 1960 uv + CCT/Duv

CCT is fundamentally a coordinate relative to the Planckian locus; Duv expresses signed off-locus distance in the traditional CIE 1960 UCS. This gives two explicit chromaticity coordinates. It is more defensible than scaling XYZ Y and allows D65/daylight whites to be represented as a CCT plus non-zero Duv instead of pretending every named daylight white lies exactly on the Planckian locus. CIE 1976 u'v' is useful diagnostically but is not the conventional Duv definition used here. Reciprocal temperature remains useful for UI offset serialization and DNG profile interpolation.

Patched implementation: `libraw_loader.rs:2285-2476`, with Planckian SPD integration against the CIE 1931 2° observer and a reciprocal-temperature LUT; nearest-locus projection in `white_point_from_xyz()` (`2341-2395`); selected camera gains in `2397-2430`; iterative inverse in `2432-2456`.

## D. Temperature locus audit

Current darktable 5.6.0 `src/iop/temperature.c` (tag `release-5.6.0`, release commit `3c17b29`) uses a blackbody spectrum below 4000 K and a daylight spectrum at/above 4000 K. It retains limits 1901–25000 K and Tint 0.135–2.326. AuRaw's old comments claimed compatibility, but AuRaw used analytic xy approximations, not darktable's spectral synthesis.

Numerical reference diagnostics on the supplied old formulas measure a CIE 1960 uv jump of **0.0027352738** across the 4000 K branch. That is not a tiny floating-point seam; it is a visible chromaticity-scale discontinuity. The replacement global CCT locus is a continuous Planckian locus at all CCT, with Duv representing off-locus daylight; the same diagnostic across 3999.999→4000.001 K gives **4.379×10⁻⁸ uv**.

This deliberately stops claiming legacy darktable Temp/Tint compatibility. If exact legacy darktable slider emulation is ever required, it should be a named compatibility mode using darktable's actual spectral model, not the production CCT/Duv model.

## E. Tint audit

Current darktable 5.6.0 explicitly marks its `xyz.Y /= tint` operation as bad and states that it is not orthogonal to the Planckian locus and therefore changes temperature. AuRaw copied the same conceptual hack and the same numeric range.

The diagnostic grid projects the old tinted white back to the Planckian locus and finds:

- maximum true projected-CCT change caused by Tint alone: **~8099 K**;
- median projected-CCT change: **~2201 K**.

That is proof that the old Tint control was not an independent green↔magenta coordinate. In the new implementation the user-facing legacy-shaped coordinate is only a parameterization of signed Duv (`basicadj.rs:250-277`). Across the tested interior grid (2500–10000 K, tint/Duv extremes), projected-CCT error is **<0.083 K**. The exact 25000 K + extreme-Duv corner is a clamped coordinate boundary, so it is tested as bounded/stable rather than falsely asserted perfectly invertible.

## F. darktable comparison

| Behavior | Original AuRaw | darktable 5.6.0 | Patched AuRaw | Important difference? |
|---|---|---|---|---|
| As Shot | LibRaw `cam_mul` | camera WB coefficients | same | no material issue |
| Kelvin inversion | fixed matrix + Z/X binary search | Z/X binary search | nearest Planckian CCT in uv; iterative DNG | **yes** |
| Tint | divide XYZ.Y | divide XYZ.Y; upstream TODO calls it bad | signed Duv | **yes** |
| Locus | analytic Planckian/daylight split | spectral blackbody/daylight split at 4000 | continuous Planckian CCT + Duv | **yes** |
| Temp→camera gains | reciprocal camera response | reciprocal `XYZ_to_CAM` response | reciprocal selected camera response | DNG state now correct |
| Green handling | mean-green normalization | four-coeff camera WB path | mean-green, preserves G2/G1 | convention differs, not inherently wrong |
| Dual green | old edit could collapse G2 | four coefficients available | preserves G2/G1 | fixed |
| DNG matrix interpolation | frozen after load | not the same DNG/DCP architecture | selected-WB reciprocal-CCT per DNG | **critical for AuRaw** |
| WB location | CFA before demosaic | early raw WB module | CFA before demosaic | correct |
| Modern CAT | local fake Bradford gains | Color Calibration defaults CAT16; also Bradford options | explicit Bradford for local/raster | old AuRaw wrong; CAT choice is separate |
| Picker | arithmetic sensor mean | picker infrastructure / raw-aware pipeline | clipped robust CFA-plane statistic | improved |

Darktable's modern Color Calibration (`src/iop/channelmixerrgb.c`) defaults to CAT16 and also implements Bradford modes. That module is conceptually distinct from its legacy sensor temperature module; AuRaw should likewise keep global camera-space RAW WB separate from local/raster scene adaptation.

## G. RawTherapee comparison

RawTherapee 5.13 (`rtengine/colortemp.cc`, release tag `5.13`; current stable release dated 2026-07-26) also has a mature spectral/observer implementation, but its `ColorTemp::mul2temp()` still solves temperature by the blue/red multiplier ratio and derives its Green coordinate afterward (`colortemp.cc:2551-2580`). It therefore is not a reason to copy the legacy two-step geometry. RawTherapee adds substantial observer/spectral data, presets, spot-WB and auto-WB machinery, and empirical UI behavior; those are useful references, but a standards-based CCT+Duv representation is cleaner for AuRaw's internal coordinate system.

RawTherapee's `rawimagesource.cc` also makes a normalization/headroom choice explicit: `calculate_scale_mul()` normalizes `pre_mul` by its maximum and `getWBMults()` recomputes channel scale/headroom for the selected WB (`rawimagesource.cc:3206-3398` in the 5.13 tag). That is different from AuRaw's mean-green normalization, and it explains why a mature processor may show a small exposure/headroom change when WB is moved without implying that one normalization is the unique colorimetric truth. RawTherapee exposes `getSpotWB()` in its `ImageSource` interface; I did not find evidence in the inspected 5.13 source that would justify copying a specific robust-statistics estimator wholesale. The patched AuRaw picker therefore uses independently testable sensor normalization, dark/near-clip rejection, per-plane sampling, 10% trimmed means, and separate G1/G2. Noise weighting or median-of-means can be added if real-camera field tests show instability.

## H. Adobe DNG / Lightroom-ACR comparison

Adobe DNG 1.7.1.0 is source-verifiable. It requires two/three calibration sets to be interpolated based on the **white balance selected by the user**, with inverse-CCT interpolation for DNG 1.2+. It defines `XYZtoCamera = AB·CC·CM`, requires iterative camera-neutral→white conversion because calibration interpolation changes with white, defines the ForwardMatrix form above, and states that multiple HueSatMap tables are interpolated the same way as calibration tags. The patch follows these requirements for the supported two-endpoint case.

Lightroom/ACR Temp/Tint slider internals are **empirical/proprietary behavior — not source verified**. No claim is made that Adobe uses CCT+Duv internally for its UI. Correct validation is same RAW + same profile + neutralized creative/tone settings + matched exposure/output space, measuring neutral and ColorChecker patches at matched white points. See `docs/COLORCHECKER_WB_VALIDATION.md`.

## I. DNG/DCP dual-illuminant interpolation

Yes: when global WB changes, the DNG profile interpolation weight must change. The interpolation coordinate is the CCT of the **user-selected white balance**. That does not mean “multiply WB twice”; it means the selected white controls both (1) the camera-neutral diagonal correction and (2) which calibration/profile characterization is appropriate.

Patched `adjusted_white_balance_render_state()` (`libraw_loader.rs:2051-2090`) computes the selected CCT/Duv, selected camera gains, interpolates DNG endpoints at selected CCT, rebuilds the camera→working transform with those same gains, and returns the live DCP weight. `gpu.rs:984-999` already had a single state-packing hook and propagates the returned matrix/weight, so no broad GPU host rewrite was needed.

## J. Global WB versus local/raster Temp/Tint

Original `basic_adjustments.wgsl:9-38` transformed to Bradford LMS but used fixed hand-tuned exponent gains:

`2^(0.22*T + 0.08*tint), 2^(-0.24*tint), 2^(-0.34*T + 0.08*tint)`.

There was no defined destination white and therefore no chromatic-adaptation matrix in the colorimetric sense. Calling this “Bradford” only described the coordinate basis.

Patched `basic_adjustments.wgsl:1-110` constructs a destination white from reciprocal-temperature + Duv, computes source and destination Bradford cone responses, and uses their ratio as the cone scaling. Input/output remain scene-linear Rec.2020 D65. Bradford is scientifically defensible here and is also the DNG-recommended linear CAT in the no-ForwardMatrix path. CAT16 is a reasonable future option for local creative adaptation (darktable's modern module defaults to it), but changing CAT alone is less important than defining actual source/destination whites.

## K. Picker, normalization, highlights, demosaic

### Picker

Original `raw_loader.rs:954-1042` used a simple arithmetic mean after black normalization and effectively collapsed second green. Patched `raw_loader.rs:1059-1168` rejects <0.003 and ≥0.97 normalized samples, caps sampling density, uses a 10% trimmed mean per canonical CFA plane, and retains G2. A synthetic contamination test reduces WB coefficient L2 error from **0.249281** to **0.000626**.

### WB normalization

`white_balance()` normalizes gains to the arithmetic mean of valid green-plane gains. This makes average green = 1 and preserves G1/G2 ratio. It is a convention, not a chromaticity error. It does affect numeric exposure scale and post-WB clipping headroom; therefore highlight thresholds must use the selected gains. AuRaw's highlight path does use current WB. Do not switch to max/min-normalization just to imitate another processor without re-auditing exposure and reconstruction.

### Highlights

AuRaw reconstructs/detects highlights in raw/WB sensor space before demosaic. Changing WB therefore can change which channel clips and the reconstruction result. That is expected in this architecture, but it needs real regression RAWs for tungsten bulbs, sunset, blue sky, skin highlights, red LEDs, and stage lighting. No such capture corpus was present in the supplied archive, so those empirical cases are not claimed validated here.

### Demosaic

Global WB before Bayer/X-Trans demosaic is correct. `cdesc` physical→canonical mapping keeps R/G1/B/G2 calibration. Do not move global RAW WB to Rec.2020 merely to simplify matrix state.

## L. Findings and severity

| Severity | Finding | Evidence / consequence |
|---|---|---|
| **Critical** | DNG/DCP matrix + profile weight frozen while WB changes | Violates selected-WB DNG interpolation; matrix factorization algebra is wrong; synthetic neutral error L2 **0.117495** old vs **0** corrected. |
| **High** | Tint is XYZ-Y scaling, not independent green/magenta coordinate | darktable upstream labels same model bad; old test shows up to **~8099 K** effective CCT shift from Tint. |
| **High** | DNG Temp↔coefficients used a fixed 6504-K characterization | Dual-illuminant camera gains solved under wrong matrix away from reference. |
| **High** | 4000-K analytic Planckian/daylight discontinuity | measured Δuv **0.00273527**. |
| **High** | Local GPU “Bradford” uses arbitrary LMS gains | no source/destination white; not a CAT. |
| **High** (rare input) | ignored LibRaw `as_shot_wb_applied` | possible double WB for already-balanced small/multishot/pseudo-RAW inputs; patched path rejects them. |
| **Medium** | Picker arithmetic mean / green collapse | outlier-sensitive; synthetic regression demonstrates large improvement with robust per-plane statistic. |
| **Medium** | DNG third calibration unsupported | DNG 1.6+ supports 3 sets; AuRaw parses 2. |
| **Medium** | ReductionMatrix / true n>3 color cameras unsupported | DNG permits n=4 general camera spaces; AuRaw's 4th slot is fundamentally G2-centric. |
| **Medium** | DCP HueSatMap path projects signed ProPhoto values to nonnegative before table | may diverge from Adobe for extreme out-of-gamut/signed camera colors; not the first WB divergence. |
| **Medium** | DNG 1.7 HDR ProfileDynamicRange behavior incomplete | profile compatibility gap, separate from core WB. |
| **Medium/Low** | normalized raw is clamped to nonnegative early | can affect deep-shadow/noise color; separate from WB geometry. |
| **Low** | misleading “darktable-compatible” UI/comments | compatibility was only partial and copied a known legacy limitation. |

## M. Tests and diagnostics added

### Rust regression tests

`libraw_loader.rs` test block (~3490+):

- Temp/Tint→coefficients→Temp/Tint across multiple synthetic camera matrices and extreme values;
- separate bounded 25,000 K coordinate-boundary test;
- DNG dual-illuminant round trip;
- CCT continuity through 4000 K;
- Duv/tint CCT invariance;
- D65 non-zero Duv representation;
- global DNG WB changes matrix and interpolation weight;
- camera-neutral→working neutrality.

`raw_loader.rs` tests (~1550+):

- extended temperature endpoints / clamping;
- area picker coefficient recovery;
- rejection of sparse dark/bright outliers;
- preservation of second-green behavior.

### Developer diagnostics

New CLI: `crates/auraw-cli/src/bin/auraw-wb-diagnostics.rs`. It prints camera make/model, As Shot and selected physical/canonical multipliers, camera neutral, As Shot/selected CCT and Tint, Duv, XYZ, xy, CIE 1960 uv, CIE 1976 u'v', selected DNG matrices, profile interpolation weight, and final camera→working matrix.

Reference tool: `tools/wb_reference_diagnostics.py` with outputs under `diagnostics/`. ColorChecker tool: `tools/colorchecker_wb_validate.py`.

## N. Numerical before/after evidence

From `diagnostics/wb_reference_results.json`:

| Diagnostic | Original | Patched/reference target |
|---|---:|---:|
| 4000-K locus step, CIE 1960 uv | 0.002735274 | 4.379×10⁻⁸ |
| max effective CCT shift caused by Tint (tested grid) | ~8099 K | <0.083 K interior projection error |
| median effective CCT shift caused by Tint | ~2201 K | near numerical projection floor |
| Tint-coordinate round-trip max error | coupled/not rigorous | ~6.03×10⁻⁷ |
| synthetic stale DNG neutral L2 error | 0.117495 | 0.000000 |
| synthetic contaminated picker WB L2 error | 0.249281 | 0.000626 |

Spectral CIE-1931 integration projects D65 to approximately CCT **6505.68 K**, Duv **+0.003194** in this 5-nm implementation. The small CCT difference from commonly quoted ~6504 K is numerical/methodological; the important result is that D65 is off the Planckian locus and therefore cannot be represented exactly by “6504 K, zero Duv”.

## O. What was changed, deleted, and retained

### Deleted/replaced rather than patched

- old `darktable_temperature_xyz()` analytic blackbody/daylight split;
- old `darktable_temperature_tint_xyz()` Y-scaling tint;
- fixed-6504 `white_balance_xyz_to_camera()` path;
- Z/X-only Temp inversion + after-the-fact Y/X tint recovery;
- local shader's hand-tuned Bradford LMS gain constants.

### Already correct and retained

- black subtraction before WB;
- camera-space WB on CFA before demosaic;
- physical CFA channel mapping and separate G1/G2 calibration concept;
- `AB·CC·CM` matrix direction;
- ForwardMatrix algebra and D50 PCS handling;
- reciprocal-temperature DNG profile-weight helper;
- dual HueSatMap endpoint storage/sampling architecture;
- fixed camera matrix staying fixed for non-DNG single-characterization cameras;
- existing GPU state packer architecture.

## P. Sidecar compatibility

The on-disk `temperature` and `tint` fields remain relative mired/tint-offset numbers. Zero still means exact As Shot, and `adjusted_white_balance_and_camera_transform()` has an exact zero-offset fast path (`raw_loader.rs:1215-1220`) returning the loaded As Shot WB, matrix, and profile weight verbatim. The renderer behind non-zero Tint values has changed intentionally; preserving pixel-identical output from mathematically incorrect old sidecars would defeat the audit goal. The developed-thumbnail cache salt was bumped to `...0007` (`sidecar.rs:18-19`) so old cached renders are invalidated.

If pixel-perfect legacy rendering is ever required, implement an explicit versioned “legacy WB renderer” selected by sidecar schema/version. Do not contaminate the corrected internal CCT+Duv model.

## Q. Validation status and required native commands

The sandbox does not contain Rust/cargo/rustfmt/naga executables, so native Rust compilation, unit tests, and shader compilation could not be executed here. That limitation is explicit: no claim is made that `cargo test` passed. Python diagnostics and the ColorChecker ΔE2000 self-check did run successfully, and the modified source was statically reviewed.

Run in the repository's Rust 1.92 environment:

```bash
cargo fmt --all -- --check
cargo test --workspace
cargo test -p auraw-core
cargo test -p auraw-gpu
cargo run -p auraw-cli --bin auraw-wb-diagnostics -- path/to/file.raw
python tools/wb_reference_diagnostics.py
python tools/colorchecker_wb_validate.py measurements.csv --json diagnostics/colorchecker_results.json
```

Add real-camera fixtures before release: at least one Bayer DNG/DCP dual-illuminant camera, one Bayer proprietary RAW, one X-Trans body, a file with meaningful G1/G2 split, and highlight stress captures under tungsten/daylight/LED.

## R. Direct answers to the 20 required questions

1. **Is original camera multipliers→Kelvin mathematically correct?** No, not as rigorous CCT. It used a fixed characterization and solved one Z/X constraint, especially wrong for dual-illuminant DNG. The patch uses nearest-locus CCT/Duv and iterates DNG matrix selection.
2. **Is original Kelvin→camera-multiplier conversion correct?** The reciprocal camera-response idea is correct for a fixed matrix. The original DNG implementation is not, because it used a 6504-K-fixed XYZ→camera matrix. Patched DNG uses the selected CCT matrix.
3. **Is original Tint correct/perceptually useful?** No as a colorimetric axis. It is a legacy XYZ-Y ratio hack. Patched Tint is a UI coordinate over signed CIE 1960 Duv.
4. **Actually darktable-compatible?** Only partially with darktable's legacy temperature module ranges/hack. AuRaw's old spectral locus was not the same implementation, and darktable itself marks the tint method defective. The patch intentionally stops claiming exact compatibility.
5. **Does Kelvin unintentionally change Tint?** In the old coupled coordinates, yes: the effective off-locus relation changes with the generated locus/matrix. New CCT/Duv coordinates are explicitly separated.
6. **Does Tint unintentionally change effective Kelvin?** Old: yes, dramatically; ~8099 K worst case in the test grid. New: <0.083 K interior projection error.
7. **Is the 4000-K transition correct/continuous?** darktable really switches blackbody→daylight at 4000 K, but AuRaw's analytic approximation was not continuous (Δuv ~0.002735). The patched global locus has no 4000-K branch.
8. **Should DNG/DCP profile weight change with WB?** Yes. DNG requires calibration and multiple HueSat maps to interpolate from the user-selected WB CCT in inverse-temperature space.
9. **Is fixed `cam_to_srgb` during global WB correct?** Yes for a genuinely fixed single camera characterization; no for AuRaw's DNG/DCP transform whose selected calibration/neutral adaptation and factored WB depend on the selected white.
10. **Are CM/CC/AB/FM combined correctly?** Core directions/order largely yes: `AB·CC·CM` and the FM branch match DNG. Original error was stale selection/state, not simple transpose. Third calibration/ReductionMatrix are incomplete.
11. **WB at correct point relative to demosaic?** Yes. Keep sensor WB pre-demosaic.
12. **Is WB normalization affecting exposure/headroom?** Yes numerically. Mean-green normalization is acceptable, but post-WB clipping/reconstruction depends on gains. AuRaw uses current selected WB in highlight logic.
13. **Is original generic GPU Bradford Temp/Tint scientifically defensible?** No. It merely used Bradford coordinates with arbitrary gains; no source/destination white existed.
14. **Should local Temp/Tint use explicit whites + CAT?** Yes. Patched local/raster path constructs a destination white and a real Bradford cone-ratio adaptation. CAT16 can be evaluated later as an alternative.
15. **Is original picker robust enough?** No. Plain means and G2 handling were vulnerable. Patched picker uses clipping/dark rejection, 10% trimmed per-plane statistics, and separate greens.
16. **Where does AuRaw first measurably diverge?** For edited DNG/DCP, at WB-dependent state construction; the first pixel-domain error appears when wrong selected gains hit CFA data, and a guaranteed second divergence occurs at camera→working because the stale matrix cancels As Shot rather than selected WB.
17. **Top three visible improvements?** (1) dynamic selected-WB DNG matrix + DCP HueSat weight, (2) CCT/Duv + selected-matrix camera coefficient conversion, (3) explicit local/raster CAT instead of hand gains. Picker robustness is next.
18. **Which code should be deleted, not patched?** The old `darktable_temperature_xyz`, `darktable_temperature_tint_xyz`, fixed-6504 WB matrix/XZ solver, and arbitrary local Bradford gain constants. They are replaced in this patch.
19. **Which parts were already correct?** Pre-demosaic camera-space WB, black-before-WB ordering, CFA/cdesc concept, core DNG matrix direction/ForwardMatrix math, reciprocal profile weighting helper, dual HSM storage, and fixed-matrix behavior for ordinary matrix cameras.
20. **What numerical evidence is better?** 4000-K Δuv 0.002735→4.38e-8; old Tint-induced CCT error up to ~8099 K→<0.083 K interior; synthetic DNG neutral error 0.117495→0; contaminated picker coefficient error 0.249281→0.000626. Native Rust/shader test results remain pending a Rust-enabled environment.

## S. Primary upstream references used

- darktable 5.6.0, tag `release-5.6.0`, `src/iop/temperature.c`: `_temperature_to_XYZ`, `_temperature_tint_to_XYZ`, `_XYZ_to_temperature`, `_xyz2mul`, `_temp2mul`; and `src/iop/channelmixerrgb.c` for CAT16/Bradford modern chromatic adaptation.
- RawTherapee 5.13, tag `5.13`, `rtengine/colortemp.cc`: `ColorTemp::temp2mul`, `ColorTemp::mul2temp`, observer/spectral data; `rtengine/rawimagesource.cc` for raw WB scaling/headroom behavior and `rtengine/imagesource.h` for the Spot-WB interface.
- Adobe Digital Negative Specification 1.7.1.0 (September 2023), pages 100–103: selected-WB calibration interpolation; inverse-CCT interpolation; `AB·CC·CM`; iterative neutral→xy; ForwardMatrix equations; HueSatMap interpolation.
- Adobe DNG SDK 1.7.1 build 2611 (June 9, 2026) as the current SDK reference noted on Adobe's DNG page at audit time.
- LibRaw API / repository-pinned 0.22.1 data model: `cam_mul`, `pre_mul`, `dng_color[2]`, `dng_levels`, `as_shot_wb_applied`.
- CIE / Ohno CCT-Duv literature: CCT nearest-point/isotemperature-line geometry and signed Duv in CIE 1960 `(u,v)` relative to the Planckian locus.
