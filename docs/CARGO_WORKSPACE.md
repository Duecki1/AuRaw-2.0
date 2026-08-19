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
```

Android version pins live in root `[workspace.metadata]`.
`AURAW_ALLOW_NO_LIBRAW=1` is only for non-production checks.

## Internal subsystem boundaries

Large processing modules keep their ABI- or algorithm-sensitive core visible while moving
cohesive support concerns behind narrow module boundaries:

- `auraw-gpu::pipeline::export` owns export orchestration and tiled rendering; `export::color`
  resolves output color transforms/profiles and `export::metadata` owns PNG/EXIF/TIFF metadata
  construction. Format-specific encoders share the same render request and cancellation path.
- `auraw-core::sidecar` owns the serialized schema, codec, and migrations; `sidecar::validation`
  contains safety/compatibility validation and `sidecar::desktop` owns desktop persistence and
  developed-thumbnail cache behavior. Serialized public models remain in the parent module.
  promptable SAM object-mask inference is isolated in `ai_masks::object` so its tensor/session
  plumbing does not expand the other model paths.
- Android `StorageManager` remains the SAF/library transaction coordinator. Regenerable thumbnail
  cache keying, persistence, LRU trimming, and legacy-cache migration are owned by
  `ThumbnailCache`; platform-independent filename/path/publish rules remain in
  `AndroidStorageContract`.

GPU uniform layouts and WGSL bindings remain explicit contracts: parameter packing may be split
into focused functions, but `#[repr(C)]` uniform structs, size assertions, binding numbers, and
shader entry points stay directly inspectable in `auraw-gpu`.
