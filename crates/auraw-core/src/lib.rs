pub mod build_metadata;
pub mod color_math;
pub mod diagnostics;
pub mod file_ops;
pub mod pipeline;
pub mod sidecar;
pub mod thumbnail_cache;

#[used]
pub static SOURCE_REVISION: &str = env!("AURAW_SOURCE_REVISION");
