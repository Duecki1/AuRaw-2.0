#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
PROFILE=${2:-release}
API=26
EXPECTED_NDK_VERSION=27.0.12077973
EXPECTED_CARGO_NDK_VERSION=4.1.2

case "$ABI" in
    arm64-v8a)
        CLANG_TARGET="aarch64-linux-android$API"
        CXX_TRIPLE=aarch64-linux-android
        ;;
    armeabi-v7a)
        CLANG_TARGET="armv7a-linux-androideabi$API"
        CXX_TRIPLE=arm-linux-androideabi
        ;;
    x86)
        CLANG_TARGET="i686-linux-android$API"
        CXX_TRIPLE=i686-linux-android
        ;;
    x86_64)
        CLANG_TARGET="x86_64-linux-android$API"
        CXX_TRIPLE=x86_64-linux-android
        ;;
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
    NDK="$SDK/ndk/$EXPECTED_NDK_VERSION"
fi
if [ -z "$NDK" ] || [ ! -f "$NDK/build/cmake/android.toolchain.cmake" ] || [ ! -f "$NDK/source.properties" ]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT)." >&2
    exit 1
fi
NDK_REVISION=$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -n 1)
if [ "$NDK_REVISION" != "$EXPECTED_NDK_VERSION" ]; then
    echo "Android NDK $EXPECTED_NDK_VERSION is required, found ${NDK_REVISION:-unknown} at $NDK" >&2
    exit 1
fi
export ANDROID_NDK_HOME="$NDK"

NDK_HOST=$(find "$NDK/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
if [ -z "$NDK_HOST" ] || [ ! -d "$NDK_HOST/sysroot" ]; then
    echo "The selected NDK has no LLVM sysroot: $NDK" >&2
    exit 1
fi
export BINDGEN_EXTRA_CLANG_ARGS="--target=$CLANG_TARGET --sysroot=$NDK_HOST/sysroot"

if [ -z "${LIBCLANG_PATH:-}" ]; then
    for candidate in "$NDK_HOST/lib64" "$NDK_HOST/lib"; do
        if find "$candidate" -maxdepth 1 -name 'libclang.so*' -print -quit 2>/dev/null | grep -q .; then
            export LIBCLANG_PATH="$candidate"
            break
        fi
    done
fi
if [ -z "${LIBCLANG_PATH:-}" ]; then
    LIBCLANG_SO=$(find /usr/lib -path '*/llvm-*/lib/libclang.so*' -print -quit 2>/dev/null || true)
    if [ -n "$LIBCLANG_SO" ]; then
        LIBCLANG_PATH=$(dirname "$LIBCLANG_SO")
        export LIBCLANG_PATH
    fi
fi

command -v cargo-ndk >/dev/null 2>&1 || {
    echo "cargo-ndk $EXPECTED_CARGO_NDK_VERSION is required. Install it with: cargo install cargo-ndk --version $EXPECTED_CARGO_NDK_VERSION --locked" >&2
    exit 1
}
CARGO_NDK_VERSION=$(cargo ndk --version 2>/dev/null | sed -n 's/^cargo-ndk //p')
if [ "$CARGO_NDK_VERSION" != "$EXPECTED_CARGO_NDK_VERSION" ]; then
    echo "cargo-ndk $EXPECTED_CARGO_NDK_VERSION is required, found ${CARGO_NDK_VERSION:-unknown}" >&2
    exit 1
fi

if [ -z "${LIBCLANG_PATH:-}" ] && command -v ldconfig >/dev/null 2>&1 \
    && ! ldconfig -p 2>/dev/null | grep -q 'libclang\.so'; then
    echo "Warning: bindgen needs host libclang; install libclang-dev or set LIBCLANG_PATH if the build cannot find it." >&2
fi

if [ "$PROFILE" = release ]; then
    REVISION=$("$ROOT/scripts/verify-source-revision.sh")
    export AURAW_REQUIRE_COMMITTED_SOURCE=1
    export AURAW_SOURCE_REVISION="$REVISION"
    export SOURCE_DATE_EPOCH="$(git -C "$ROOT" show -s --format=%ct "$REVISION")"
    rm -rf "$ROOT/android/native"
fi
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$ROOT/target"
unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS RUSTFLAGS RUSTDOCFLAGS

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
rm -rf "$ROOT/android/app/src/main/jniLibs/$ABI"
# shellcheck disable=SC2086
cargo ndk -t "$ABI" -o "$ROOT/android/app/src/main/jniLibs" \
    build --locked $CARGO_PROFILE --lib --manifest-path "$ROOT/Cargo.toml"

CXX_RUNTIME="$NDK_HOST/sysroot/usr/lib/$CXX_TRIPLE/libc++_shared.so"
test -f "$CXX_RUNTIME"
cp "$CXX_RUNTIME" "$ROOT/android/app/src/main/jniLibs/$ABI/libc++_shared.so"
test -f "$ROOT/android/app/src/main/jniLibs/$ABI/libauraw.so"
test -f "$ROOT/android/app/src/main/jniLibs/$ABI/libc++_shared.so"

if [ "$PROFILE" = release ]; then
    if ! FINAL_REVISION=$("$ROOT/scripts/verify-source-revision.sh") \
        || [ "$FINAL_REVISION" != "$REVISION" ]; then
        rm -rf "$ROOT/android/app/src/main/jniLibs/$ABI"
        echo "source changed during the build; discarded the Android native library" >&2
        exit 1
    fi
fi

echo "Rust and LibRaw Android libraries are ready for Gradle ($ABI, $PROFILE)."
