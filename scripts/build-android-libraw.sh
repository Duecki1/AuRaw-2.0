#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
API=${ANDROID_API_LEVEL:-26}
LIBRAW_VERSION=0.22.1
LIBRAW_CMAKE_COMMIT=eb98e4325aef2ce85d2eb031c2ff18640ca616d3

case "$ABI" in
    arm64-v8a|armeabi-v7a|x86|x86_64) ;;
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

command -v cmake >/dev/null 2>&1 || {
    echo "cmake is required to build LibRaw" >&2
    exit 1
}
command -v curl >/dev/null 2>&1 || {
    echo "curl is required to download LibRaw" >&2
    exit 1
}

SRC_ROOT="$ROOT/android/native/src"
LIBRAW_SRC="$SRC_ROOT/LibRaw-$LIBRAW_VERSION"
CMAKE_SRC="$SRC_ROOT/LibRaw-cmake-$LIBRAW_CMAKE_COMMIT"
BUILD_DIR="$ROOT/android/native/build/libraw-$ABI"
INSTALL_DIR="$ROOT/android/native/libraw/$ABI"
mkdir -p "$SRC_ROOT"

BUILD_KEY="LibRaw=$LIBRAW_VERSION cmake=$LIBRAW_CMAKE_COMMIT abi=$ABI api=$API ndk=$NDK"
if [ "${AURAW_REBUILD_LIBRAW:-0}" != 1 ] \
    && [ -f "$INSTALL_DIR/include/libraw/libraw.h" ] \
    && [ -f "$INSTALL_DIR/lib/libraw.a" ] \
    && [ -f "$INSTALL_DIR/.auraw-build" ] \
    && grep -Fqx "$BUILD_KEY" "$INSTALL_DIR/.auraw-build"; then
    echo "Using cached LibRaw $LIBRAW_VERSION for $ABI in $INSTALL_DIR"
    exit 0
fi

if [ ! -f "$LIBRAW_SRC/libraw/libraw.h" ]; then
    rm -rf "$LIBRAW_SRC"
    mkdir -p "$LIBRAW_SRC"
    curl -fL "https://api.github.com/repos/LibRaw/LibRaw/tarball/$LIBRAW_VERSION" \
        | tar -xz --strip-components=1 -C "$LIBRAW_SRC"
fi

if [ ! -f "$CMAKE_SRC/CMakeLists.txt" ]; then
    rm -rf "$CMAKE_SRC"
    mkdir -p "$CMAKE_SRC"
    curl -fL "https://github.com/LibRaw/LibRaw-cmake/archive/$LIBRAW_CMAKE_COMMIT.tar.gz" \
        | tar -xz --strip-components=1 -C "$CMAKE_SRC"
fi

# LibRaw intentionally maintains its CMake files in a companion repository.
cp "$CMAKE_SRC/CMakeLists.txt" "$LIBRAW_SRC/CMakeLists.txt"
rm -rf "$LIBRAW_SRC/cmake"
cp -R "$CMAKE_SRC/cmake" "$LIBRAW_SRC/cmake"

GENERATOR=
if command -v ninja >/dev/null 2>&1; then
    GENERATOR="-GNinja"
fi

rm -rf "$BUILD_DIR" "$INSTALL_DIR"
cmake -S "$LIBRAW_SRC" -B "$BUILD_DIR" $GENERATOR \
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI="$ABI" \
    -DANDROID_PLATFORM="android-$API" \
    -DANDROID_STL=c++_static \
    -DCMAKE_BUILD_TYPE=Release \
    -DCMAKE_INSTALL_PREFIX="$INSTALL_DIR" \
    -DCMAKE_INSTALL_LIBDIR=lib \
    -DBUILD_SHARED_LIBS=OFF \
    -DENABLE_OPENMP=OFF \
    -DENABLE_LCMS=OFF \
    -DENABLE_JASPER=OFF \
    -DENABLE_EXAMPLES=OFF \
    -DENABLE_RAWSPEED=OFF \
    -DENABLE_X3FTOOLS=OFF \
    -DLIBRAW_INSTALL=ON \
    -DLIBRAW_UNINSTALL_TARGET=OFF \
    -DCMAKE_DISABLE_FIND_PACKAGE_JPEG=ON
cmake --build "$BUILD_DIR" --target install --parallel

test -f "$INSTALL_DIR/include/libraw/libraw.h"
test -f "$INSTALL_DIR/lib/libraw.a"
printf '%s\n' "$BUILD_KEY" > "$INSTALL_DIR/.auraw-build"
echo "LibRaw $LIBRAW_VERSION for $ABI installed in $INSTALL_DIR"
