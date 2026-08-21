# Development

Use Rust 1.92 with LibRaw, Lensfun, libclang, and the platform graphics
dependencies.

```sh
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo run -p auraw-ui --bin auraw --release
```
