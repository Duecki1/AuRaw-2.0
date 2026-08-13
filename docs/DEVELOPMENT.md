# Development guide

Use Rust 1.92 and install LibRaw, Lensfun, libclang, and the platform graphics
dependencies before building.

```sh
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p auraw-ui --bin auraw --release
```

Repository and release helpers live in `xtask`:

```sh
cargo xtask --help
cargo xtask check-all
cargo xtask icons
```

Android builds use the versions in the root `[workspace.metadata]` table. See
[`ANDROID.md`](../ANDROID.md) for setup and packaging commands.

`data/wb_presets.json` is a compact subset of darktable's GPL-licensed camera
white-balance database.
