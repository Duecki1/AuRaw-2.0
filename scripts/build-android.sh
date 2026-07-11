#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
PROFILE=${2:-release}
API=${ANDROID_API_LEVEL:-26}

case "$ABI" in
    arm64-v8a) CLANG_TARGET="aarch64-linux-android$API" ;;
    armeabi-v7a) CLANG_TARGET="armv7a-linux-androideabi$API" ;;
    x86) CLANG_TARGET="i686-linux-android$API" ;;
    x86_64) CLANG_TARGET="x86_64-linux-android$API" ;;
    *)
        echo "Unsupported ABI '$ABI' (use arm64-v8a, armeabi-v7a, x86, or x86_64)" >&2
        exit 2
        ;;
esac

NDK=${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-}}
if [ -z "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ] \
    && [ -f "$ROOT/android/local.properties" ]; then
    LOCAL_SDK=$(sed -n 's/^sdk\.dir=//p' "$ROOT/android/local.properties" | tail -n 1)
    if [ -n "$LOCAL_SDK" ]; then
        export ANDROID_SDK_ROOT="$LOCAL_SDK"
    fi
fi
if [ -z "$NDK" ] && [ -n "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ]; then
    SDK=${ANDROID_SDK_ROOT:-$ANDROID_HOME}
    NDK=$(find "$SDK/ndk" -mindepth 1 -maxdepth 1 -type d 2>/dev/null | sort -V | tail -n 1 || true)
fi
if [ -z "$NDK" ] || [ ! -f "$NDK/build/cmake/android.toolchain.cmake" ]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT)." >&2
    exit 1
fi
export ANDROID_NDK_HOME="$NDK"

NDK_HOST=$(find "$NDK/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
if [ -z "$NDK_HOST" ] || [ ! -d "$NDK_HOST/sysroot" ]; then
    echo "The selected NDK has no LLVM sysroot: $NDK" >&2
    exit 1
fi
export BINDGEN_EXTRA_CLANG_ARGS="--target=$CLANG_TARGET --sysroot=$NDK_HOST/sysroot ${BINDGEN_EXTRA_CLANG_ARGS:-}"

if [ -z "${LIBCLANG_PATH:-}" ]; then
    for candidate in "$NDK_HOST/lib64" "$NDK_HOST/lib"; do
        if find "$candidate" -maxdepth 1 -name 'libclang.so*' -print -quit 2>/dev/null | grep -q .; then
            export LIBCLANG_PATH="$candidate"
            break
        fi
    done
fi

command -v cargo-ndk >/dev/null 2>&1 || {
    echo "cargo-ndk is required. Install it with: cargo install cargo-ndk" >&2
    exit 1
}

if [ -z "${LIBCLANG_PATH:-}" ] && command -v ldconfig >/dev/null 2>&1 \
    && ! ldconfig -p 2>/dev/null | grep -q 'libclang\.so'; then
    echo "Warning: bindgen needs host libclang; install libclang-dev or set LIBCLANG_PATH if the build cannot find it." >&2
fi

"$ROOT/scripts/build-android-libraw.sh" "$ABI"

case "$PROFILE" in
    release) CARGO_PROFILE=--release ;;
    debug) CARGO_PROFILE= ;;
    *)
        echo "Unknown profile '$PROFILE' (use release or debug)" >&2
        exit 2
        ;;
esac

export AURAW_LIBRAW_ROOT="$ROOT/android/native/libraw/$ABI"
# shellcheck disable=SC2086
cargo ndk -t "$ABI" -o "$ROOT/android/app/src/main/jniLibs" \
    build $CARGO_PROFILE --lib --manifest-path "$ROOT/Cargo.toml"

echo "Rust and LibRaw Android libraries are ready for Gradle ($ABI, $PROFILE)."
