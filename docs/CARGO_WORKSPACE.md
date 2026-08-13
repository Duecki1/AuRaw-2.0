# Cargo workspace

| Crate | Responsibility |
| --- | --- |
| `auraw-core` | Processing, metadata, sidecars, and thumbnails |
| `auraw-gpu` | wgpu pipelines, shaders, and tiled export |
| `auraw-ai` | ONNX models and the sole `ort` dependency |
| `auraw-ui` | egui app and the sole `eframe` dependency |
| `auraw-ffi` | C ABI, Android JNI/storage, and the sole `jni` dependency |
| `auraw-cli` | Headless exports |

Dependencies flow from UI/CLI/FFI through AI/GPU to core. The GPU-to-FFI edge
exists only on Android.

Declare shared dependencies in the root `[workspace.dependencies]`. Keep
algorithmic types in core, GPU representations in GPU, and UI code in UI.

```sh
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p auraw-ui --bin auraw --release
cargo xtask check-all
```

Android version pins live in root `[workspace.metadata]`.
`AURAW_ALLOW_NO_LIBRAW=1` is only for non-production checks.
