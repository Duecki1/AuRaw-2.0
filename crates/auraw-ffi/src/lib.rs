//! C ABI and Android JNI boundary for AuRaw.

/// Stable ABI marker for native hosts.
#[unsafe(no_mangle)]
pub extern "C" fn auraw_abi_version() -> u32 {
    1
}

/// AuRaw's packed semantic version: major << 16 | minor << 8 | patch.
#[unsafe(no_mangle)]
pub extern "C" fn auraw_version_packed() -> u32 {
    2_u32 << 16
}

#[cfg(target_os = "android")]
pub use android_activity::AndroidApp;

#[cfg(target_os = "android")]
pub mod pipeline {
    pub use auraw_core::pipeline::*;
}

#[cfg(target_os = "android")]
pub mod sidecar {
    pub use auraw_core::sidecar::*;
}

#[cfg(target_os = "android")]
pub mod thumbnail_cache {
    pub use auraw_core::thumbnail_cache::*;
}

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;
