# AuRaw 2.0 hybrid remediation

Base: `AuRaw-2.0-uhhhyea(3)`

Ported from/reconciled with `AuRaw-2.0-hmmokay(3)`:

- camera-space signal/opponent denoise basis for pre-characterization processing;
- one-time, symmetric G1/G2 noise canonicalization;
- continuous Blacks guard at non-positive luminance;
- guarded nonnegative Texture/Clarity projection;
- conservative final unit-gamut clamp after out-of-cube projection only;
- fallible shader-format specialization;
- release render-plan pass-count validation;
- fail-by-default desktop and Android LibRaw configuration with an explicit
  opt-out accepted only for `AURAW_ALLOW_NO_LIBRAW=1` or `true`;
- continuous lifted-black master-curve remapping across the signed
  zero-luminance plane using the master curve's scene-domain endpoint slope;
- signed slope-matched negative-domain extensions for global and local RGB
  channel curves, including descending first segments; composite master curves
  retain their explicit neutral black-floor policy;
- a half-float-safe C1 scene-curve shoulder, a shared finite scene-domain
  endpoint-slope limit applied to the actual global/local Hermite tangent,
  bounded signed channel outputs, and ratio-preserving composite-curve
  headroom limiting;
- synchronized low-tone analytical thresholds and color-ramp diagnostics.

Retained from uhhhyea:

- exact valid-unit-cube ICC LUT input policy and CPU/GPU reference tests;
- transactional display-profile installation and rollback;
- detailed/aggregate GPU resource planning;
- checked GPU readback arithmetic;
- generated/version-verified Lensfun bindings;
- official Gradle wrapper plus integrity validation;
- stronger thumbnail-cache hardening and tests.

The Python remediation and processing-math tests are source-policy and analytical
reference checks. They do not replace Rust compilation, Naga/WGSL validation, GPU
resource synchronization tests, or rendered pixel comparisons.

A lifted composite black endpoint is documented as an intentional neutral floor.
It necessarily reduces shadow colorfulness near absolute black; continuity and a
neutral black level are prioritized over preserving RGB ratios at the endpoint.

Scene point-curve decoding uses the original rational shaper through normal
HDR values, then enters a C1 Hermite shoulder at an exactly representable f32
coordinate and reaches 32768 scene-linear with zero derivative at encoded 1.0.
The paired encoder numerically inverts that shoulder. Its rational branch is
clamped to the exact encoded shoulder coordinate so float32 rounding cannot
advance across the join and make the composed decode(encode(y)) response
non-monotonic. The remaining small join plateau is an encoded quantization
limit; its first transition is one shoulder-decoder step and remains monotonic
through float16 storage. The first global or local Hermite tangent is limited
so its decoded scene-domain
slope cannot exceed 1048576; the same limited tangent drives both the positive
curve and the signed negative-domain extension. This prevents crafted endpoints
near 1.0 from turning the first representable half-float channel step into a
multi-thousand-unit false-colour jump without introducing a wide hard guard-band plateau.

Analytical float32 references cover the final 128 encoded values below 1.0,
realizable first-segment tangents through ±200, the first eight signed half-float
steps, a representative -1e-4 channel value, and float16 output storage. Signed
channel extensions remain independently bounded to ±60000 because those controls
are intentionally channel-specific. Composite master-curve candidates instead
receive one uniform scale when they exceed that headroom, preserving RGB ratios,
chromaticity, hue, and normalized chroma.

Known merge blockers remain:

- replace exact-equality view-transform selection with an explicit serialized enum;
- make delivery sharpening controllable and previewable;
- add full-pipeline rendered CPU/GPU regressions through ICC output, resize, and
  delivery sharpening;
- run real RAW comparisons and Rust/Naga/Android multi-backend validation;
- explicitly `sync_all()` the completed staged export and synchronize its parent
  directory before reporting durable success, or document the weaker guarantee;
- complete native release notices/SBOM coverage for LibRaw and Little CMS.

Large application and GPU modules are intentionally deferred to staged follow-up
refactoring rather than mixed into this correctness consolidation.
