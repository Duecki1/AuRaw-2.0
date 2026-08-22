
pub const ANDROID_NDK_VERSION: &str = env!("AURAW_ANDROID_NDK_VERSION");
pub const ANDROID_BUILD_TOOLS_VERSION: &str = env!("AURAW_ANDROID_BUILD_TOOLS_VERSION");
pub const ANDROID_COMPILE_SDK: u32 = parse_u32(env!("AURAW_ANDROID_COMPILE_SDK"));
pub const ANDROID_MIN_SDK: u32 = parse_u32(env!("AURAW_ANDROID_MIN_SDK"));
pub const ANDROID_TARGET_SDK: u32 = parse_u32(env!("AURAW_ANDROID_TARGET_SDK"));
pub const LIBRAW_REVISION: &str = env!("AURAW_LIBRAW_REVISION");
pub const LENSFUN_REVISION: &str = env!("AURAW_LENSFUN_REVISION");
pub const ANDROID_USE_LEGACY_PACKAGING: bool =
    parse_bool(env!("AURAW_ANDROID_USE_LEGACY_PACKAGING"));

const fn parse_u32(value: &str) -> u32 {
    let bytes = value.as_bytes();
    if bytes.is_empty() {
        panic!("empty build metadata integer");
    }
    let mut index = 0;
    let mut result = 0_u32;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte < b'0' || byte > b'9' {
            panic!("invalid build metadata integer");
        }
        result = result * 10 + (byte - b'0') as u32;
        index += 1;
    }
    result
}

const fn parse_bool(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() == 4
        && bytes[0] == b't'
        && bytes[1] == b'r'
        && bytes[2] == b'u'
        && bytes[3] == b'e'
    {
        true
    } else if bytes.len() == 5
        && bytes[0] == b'f'
        && bytes[1] == b'a'
        && bytes[2] == b'l'
        && bytes[3] == b's'
        && bytes[4] == b'e'
    {
        false
    } else {
        panic!("invalid build metadata boolean");
    }
}
