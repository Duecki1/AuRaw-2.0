#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
API=26
LIBRAW_VERSION=0.22.1
LIBRAW_REVISION=b860248a89d9082b8e0a1e202e516f46af9adb29
LIBRAW_ARCHIVE_SHA256=f5da1e522ea195b54b30f3ff105ef2193daa04ea165dea825b4d6fe9d886395b
BUILD_CONTRACT="$ROOT/android/build-contract.properties"
EXPECTED_NDK_VERSION=$(sed -n 's/^ndkVersion=//p' "$BUILD_CONTRACT")
EXPECTED_CMAKE_VERSION=3.22.1
LIBRAW_CMAKE_COMMIT=eb98e4325aef2ce85d2eb031c2ff18640ca616d3
LIBRAW_CMAKE_ARCHIVE_SHA256=3cd218bf6d1254de86e27269541277fbfc5bae57a9002ce0b46fbe2a97088b43

if [ "${1:-}" = "--print-build-contract" ]; then
    printf '{"ndkVersion":"%s"}\n' "$EXPECTED_NDK_VERSION"
    exit 0
fi

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

SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
CMAKE=cmake
if [ -n "$SDK" ] && [ -x "$SDK/cmake/$EXPECTED_CMAKE_VERSION/bin/cmake" ]; then
    CMAKE="$SDK/cmake/$EXPECTED_CMAKE_VERSION/bin/cmake"
elif ! command -v "$CMAKE" >/dev/null 2>&1; then
    echo "CMake $EXPECTED_CMAKE_VERSION is required to build LibRaw" >&2
    exit 1
fi
CMAKE_VERSION=$("$CMAKE" --version | sed -n '1s/^cmake version //p')
CMAKE_BASE_VERSION=${CMAKE_VERSION%%-*}
if [ "$CMAKE_BASE_VERSION" != "$EXPECTED_CMAKE_VERSION" ]; then
    echo "CMake $EXPECTED_CMAKE_VERSION is required, found ${CMAKE_VERSION:-unknown}" >&2
    exit 1
fi
command -v curl >/dev/null 2>&1 || {
    echo "curl is required to download LibRaw" >&2
    exit 1
}
command -v sha256sum >/dev/null 2>&1 || {
    echo "sha256sum is required to verify LibRaw sources" >&2
    exit 1
}
unset AR CC CFLAGS CPPFLAGS CXX CXXFLAGS LDFLAGS RANLIB

SRC_ROOT="$ROOT/android/native/src"
LIBRAW_SRC="$SRC_ROOT/LibRaw-$LIBRAW_VERSION"
CMAKE_SRC="$SRC_ROOT/LibRaw-cmake-$LIBRAW_CMAKE_COMMIT"
BUILD_DIR="$ROOT/android/native/build/libraw-$ABI"
INSTALL_DIR="$ROOT/android/native/libraw/$ABI"
mkdir -p "$SRC_ROOT"

BUILD_KEY="LibRaw=$LIBRAW_VERSION@$LIBRAW_REVISION cmake-files=$LIBRAW_CMAKE_COMMIT cmake=$CMAKE_VERSION abi=$ABI api=$API ndk=$NDK_REVISION"
if [ "${AURAW_REBUILD_LIBRAW:-0}" != 1 ] \
    && [ -f "$INSTALL_DIR/include/libraw/libraw.h" ] \
    && [ -f "$INSTALL_DIR/lib/libraw.a" ] \
    && [ -f "$INSTALL_DIR/.auraw-build" ] \
    && grep -Fqx "$BUILD_KEY" "$INSTALL_DIR/.auraw-build"; then
    echo "Using cached LibRaw $LIBRAW_VERSION for $ABI in $INSTALL_DIR"
    exit 0
fi

if [ ! -f "$LIBRAW_SRC/libraw/libraw.h" ] \
    || [ ! -f "$LIBRAW_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$LIBRAW_SRC/.auraw-archive-sha256")" != "$LIBRAW_ARCHIVE_SHA256" ]; then
    LIBRAW_ARCHIVE=$(mktemp "$SRC_ROOT/.libraw.XXXXXX.tar.gz")
    trap 'rm -f "${LIBRAW_ARCHIVE:-}" "${CMAKE_ARCHIVE:-}"' EXIT HUP INT TERM
    curl --fail --location --proto "=https" --tlsv1.2 --retry 3 \
        --output "$LIBRAW_ARCHIVE" \
        "https://github.com/LibRaw/LibRaw/archive/$LIBRAW_REVISION.tar.gz"
    printf '%s  %s\n' "$LIBRAW_ARCHIVE_SHA256" "$LIBRAW_ARCHIVE" | sha256sum --check --status
    rm -rf "$LIBRAW_SRC"
    mkdir -p "$LIBRAW_SRC"
    tar -xzf "$LIBRAW_ARCHIVE" --strip-components=1 -C "$LIBRAW_SRC"
    printf '%s\n' "$LIBRAW_ARCHIVE_SHA256" > "$LIBRAW_SRC/.auraw-archive-sha256"
    rm -f "$LIBRAW_ARCHIVE"
    LIBRAW_ARCHIVE=
fi

if [ ! -f "$CMAKE_SRC/CMakeLists.txt" ] \
    || [ ! -f "$CMAKE_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$CMAKE_SRC/.auraw-archive-sha256")" != "$LIBRAW_CMAKE_ARCHIVE_SHA256" ]; then
    CMAKE_ARCHIVE=$(mktemp "$SRC_ROOT/.libraw-cmake.XXXXXX.tar.gz")
    trap 'rm -f "${LIBRAW_ARCHIVE:-}" "${CMAKE_ARCHIVE:-}"' EXIT HUP INT TERM
    curl --fail --location --proto "=https" --tlsv1.2 --retry 3 \
        --output "$CMAKE_ARCHIVE" \
        "https://github.com/LibRaw/LibRaw-cmake/archive/$LIBRAW_CMAKE_COMMIT.tar.gz"
    printf '%s  %s\n' "$LIBRAW_CMAKE_ARCHIVE_SHA256" "$CMAKE_ARCHIVE" | sha256sum --check --status
    rm -rf "$CMAKE_SRC"
    mkdir -p "$CMAKE_SRC"
    tar -xzf "$CMAKE_ARCHIVE" --strip-components=1 -C "$CMAKE_SRC"
    printf '%s\n' "$LIBRAW_CMAKE_ARCHIVE_SHA256" > "$CMAKE_SRC/.auraw-archive-sha256"
    rm -f "$CMAKE_ARCHIVE"
    CMAKE_ARCHIVE=
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
"$CMAKE" -S "$LIBRAW_SRC" -B "$BUILD_DIR" $GENERATOR \
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
"$CMAKE" --build "$BUILD_DIR" --target install --parallel

test -f "$INSTALL_DIR/include/libraw/libraw.h"
test -f "$INSTALL_DIR/lib/libraw.a"
printf '%s\n' "$BUILD_KEY" > "$INSTALL_DIR/.auraw-build"
echo "LibRaw $LIBRAW_VERSION for $ABI installed in $INSTALL_DIR"
