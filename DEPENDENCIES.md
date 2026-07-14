# Dependency policy

## ONNX Runtime

AuRaw uses `ort = 2.0.0-rc.12` because the crate's 2.x dynamic-loading and Android XNNPACK APIs used by the application are still published under the release-candidate line. The dependency is exact-pinned, API level 18 is selected explicitly, desktop loads a user-approved runtime by SHA-256, and Android uses the crate-managed binary feature set.

Upgrade only after running desktop dynamic-runtime tests, Android arm64 builds, subject-mask output-layout tests, and the full image-regression suite. A stable 2.x release should replace the RC as soon as it exposes the required APIs without regressions.
