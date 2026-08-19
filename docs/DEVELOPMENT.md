# Development

Use Rust 1.92 with LibRaw, Lensfun, libclang, and the platform graphics
dependencies.

```sh
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p auraw-ui --bin auraw --release
```

Run `cargo xtask --help` for the Android and icon helpers. Android setup is in
[ANDROID.md](../ANDROID.md). `data/wb_presets.json` contains a GPL-licensed
subset of darktable's camera white-balance data.
