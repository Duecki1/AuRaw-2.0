# Cargo workspace architecture

AuRaw is a virtual Cargo workspace. The root manifest owns shared package metadata,
dependency versions, and release profiles; production code lives in six focused crates.

| Crate | Responsibility | Dependency boundary |
| --- | --- | --- |
| `auraw-core` | Color science, CPU processing, RAW/LibRaw and DCP metadata, sidecar JSON, thumbnails | No UI, GPU, ONNX, or JNI dependencies |
| `auraw-gpu` | wgpu pipelines, WGSL preprocessing/validation, GPU resources, tiled export | Depends on `auraw-core`; no `eframe` or `ort` |
| `auraw-ai` | BiRefNet, SAM 2.1, LaMa, and RAW-denoise ONNX bindings | Sole owner of `ort`; depends on core/GPU data types |
| `auraw-ui` | egui/eframe application, Develop panels, touch controls | Sole owner of `eframe`; composes core, GPU, and AI |
| `auraw-ffi` | C ABI and Android JNI/storage bridge | Sole owner of `jni`; emits `cdylib` and `rlib` |
| `auraw-cli` | Headless exports | Depends on core/GPU; no UI or ONNX runtime |

The dependency graph is acyclic:

```text
auraw-core <- auraw-ffi
     ^             ^
     |             |
auraw-gpu ---------+
     ^
     |
auraw-ai
     ^
     |
auraw-ui

auraw-core <- auraw-gpu <- auraw-cli
```

The `auraw-gpu -> auraw-ffi` edge exists only on Android for direct-export path
handling. Desktop builds do not compile the FFI crate into the GPU layer.

## Common commands

```sh
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p auraw-ui --bin auraw --release
cargo run -p auraw-cli --bin auraw-develop-export -- --help
./gradlew assembleDebug -PaurawAbis=arm64-v8a,x86_64
cargo xtask check-all
```

`AURAW_ALLOW_NO_LIBRAW=1` remains available only for intentional non-production
checks on hosts without LibRaw. Production builds still require LibRaw.

The Android build contract is defined only in the root `Cargo.toml` under
`[workspace.metadata]`. `android/build-contract.properties` is retained solely as
an empty legacy compatibility marker and must not contain contract values.

## Ownership rules

New dependencies must be declared in `[workspace.dependencies]` and inherited by
the owning crate. Keep `ort` in `auraw-ai`, `eframe` in `auraw-ui`, and `jni` in
`auraw-ffi`. Shared algorithmic types belong in `auraw-core`; GPU representations
belong in `auraw-gpu`. Moving a module between crates must not change its numerical
implementation or shader logic.
