# Development

Use Rust 1.92 with LibRaw, Lensfun, libclang, and the platform graphics
dependencies.

```sh
cargo fmt --all -- --check
cargo check --workspace --all-targets
cargo test --workspace --all-targets
cargo clippy --workspace --all-targets --all-features -- \
  -D warnings -W clippy::perf -W clippy::large_stack_arrays \
  -W clippy::redundant_clone
cargo deny check
cargo run -p auraw-ui --bin auraw --release
```
