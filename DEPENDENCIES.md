# Dependency policy

## ONNX Runtime

AuRaw uses `ort = 2.0.0-rc.12` because the crate's 2.x dynamic-loading and Android XNNPACK APIs used by the application are still published under the release-candidate line. The dependency is exact-pinned, API level 18 is selected explicitly, desktop loads a user-approved runtime by SHA-256, and Android uses the crate-managed binary feature set.

Upgrade only after running desktop dynamic-runtime tests, Android arm64 builds, subject-mask output-layout tests, and the full image-regression suite. A stable 2.x release should replace the RC as soon as it exposes the required APIs without regressions.

## Lensfun

Desktop builds probe the system `lensfun` package through `pkg-config`. When it
is present, AuRaw links to the shared Lensfun library and uses its camera/lens
profile database for distortion, lateral chromatic-aberration, and vignetting
correction. When it is absent, the build remains functional with lens correction
disabled. Android currently uses that disabled fallback.

Release packages must include the Lensfun shared library when the platform does
not provide it and must include the compatible XML database under `lensfun/` or
`share/auraw/lensfun/`. `AURAW_LENSFUN_DB` may prioritize an alternate database
root for development or local profile testing. Keep the library and database
notices in `THIRD_PARTY_NOTICES.md` when redistributing either component.
