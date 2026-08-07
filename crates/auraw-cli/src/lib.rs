//! Headless regression rendering, benchmark, and export support.

pub mod pipeline {
    pub use auraw_gpu::pipeline::*;
}

pub mod regression;

pub use auraw_core::SOURCE_REVISION;
