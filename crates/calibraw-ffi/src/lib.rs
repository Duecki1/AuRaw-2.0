#[unsafe(no_mangle)]
pub extern "C" fn calibraw_abi_version() -> u32 {
    1
}

#[unsafe(no_mangle)]
pub extern "C" fn calibraw_version_packed() -> u32 {
    2_u32 << 16
}

#[cfg(target_os = "android")]
pub use android_activity::AndroidApp;

#[cfg(target_os = "android")]
pub mod pipeline {
    pub use calibraw_core::pipeline::*;
}

#[cfg(target_os = "android")]
pub mod sidecar {
    pub use calibraw_core::sidecar::*;
}

#[cfg(target_os = "android")]
pub mod thumbnail_cache {
    pub use calibraw_core::thumbnail_cache::*;
}

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "android")]
pub use android::*;
