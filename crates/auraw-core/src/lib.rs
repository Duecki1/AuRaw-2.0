//! Platform-neutral AuRaw processing, metadata, persistence, and RAW decoding.

pub mod build_metadata;
pub mod diagnostics;
pub mod file_ops;
pub mod pipeline;
pub mod sidecar;
pub mod thumbnail_cache;

/// Git revision embedded by `build.rs` for traceable binaries.
#[used]
pub static SOURCE_REVISION: &str = env!("AURAW_SOURCE_REVISION");
