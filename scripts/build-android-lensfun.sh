#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
ABI=${1:-arm64-v8a}
API=26
LENSFUN_VERSION=0.3.4
LENSFUN_REVISION=101c745e847a5de4a1e569a94368ce2027198598
LENSFUN_ARCHIVE_SHA256=a11cbe6aeec657839540448b253217c25d20b7a45b6aebfef406f7239933c7a6
ICONV_VERSION=1.17
ICONV_ARCHIVE_SHA256=8f74213b56238c85a50a5329f77e06198771e70dd9a739779f4c02f65d971313
GLIB_VERSION=2.78.6
GLIB_ARCHIVE_SHA256=244854654dd82c7ebcb2f8e246156d2a05eb9cd1ad07ed7a779659b4602c9fae
MESON_VERSION=1.7.0
MESON_WHEEL_SHA256=ae3f12953045f3c7c60e27f2af1ad862f14dee125b4ed9bcb8a842a5080dbf85
SETUPTOOLS_VERSION=83.0.0
SETUPTOOLS_WHEEL_SHA256=29b23c360f22f414dc7336bb39178cc7bcbf6021ed2733cde173f09dba19abb3
EXPECTED_NDK_VERSION=28.2.13676358

case "$ABI" in
    arm64-v8a)
        CLANG_TARGET="aarch64-linux-android$API"
        AUTOCONF_HOST=aarch64-linux-android
        MESON_CPU_FAMILY=aarch64
        MESON_CPU=aarch64
        ;;
    armeabi-v7a)
        CLANG_TARGET="armv7a-linux-androideabi$API"
        AUTOCONF_HOST=arm-linux-androideabi
        MESON_CPU_FAMILY=arm
        MESON_CPU=armv7
        ;;
    x86)
        CLANG_TARGET="i686-linux-android$API"
        AUTOCONF_HOST=i686-linux-android
        MESON_CPU_FAMILY=x86
        MESON_CPU=i686
        ;;
    x86_64)
        CLANG_TARGET="x86_64-linux-android$API"
        AUTOCONF_HOST=x86_64-linux-android
        MESON_CPU_FAMILY=x86_64
        MESON_CPU=x86_64
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
if [ -z "$NDK" ] || [ ! -f "$NDK/build/cmake/android.toolchain.cmake" ] \
    || [ ! -f "$NDK/source.properties" ]; then
    echo "Android NDK not found. Set ANDROID_NDK_HOME (or ANDROID_SDK_ROOT)." >&2
    exit 1
fi
NDK_REVISION=$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -n 1)
if [ "$NDK_REVISION" != "$EXPECTED_NDK_VERSION" ]; then
    echo "Android NDK $EXPECTED_NDK_VERSION is required, found ${NDK_REVISION:-unknown} at $NDK" >&2
    exit 1
fi
NDK_HOST=$(find "$NDK/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
if [ -z "$NDK_HOST" ] || [ ! -x "$NDK_HOST/bin/${CLANG_TARGET}-clang" ]; then
    echo "The selected NDK has no compiler for $ABI: $NDK" >&2
    exit 1
fi

command -v ninja >/dev/null 2>&1 || {
    echo "Ninja is required to build Android Lensfun." >&2
    exit 1
}

SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
CMAKE=cmake
if [ -n "$SDK" ] && [ -x "$SDK/cmake/3.22.1/bin/cmake" ]; then
    CMAKE="$SDK/cmake/3.22.1/bin/cmake"
fi
command -v "$CMAKE" >/dev/null 2>&1 || {
    echo "CMake is required to build Android Lensfun." >&2
    exit 1
}

SRC_ROOT="$ROOT/android/native/src"
GLIB_SRC="$SRC_ROOT/glib-$GLIB_VERSION"
ICONV_SRC="$SRC_ROOT/libiconv-$ICONV_VERSION"
LENSFUN_SRC="$SRC_ROOT/lensfun-$LENSFUN_VERSION"
ICONV_BUILD="$ROOT/android/native/build/libiconv-$ABI"
GLIB_BUILD="$ROOT/android/native/build/glib-$ABI"
LENSFUN_BUILD="$ROOT/android/native/build/lensfun-$ABI"
INSTALL_DIR="$ROOT/android/native/lensfun/$ABI"
CROSS_FILE="$ROOT/android/native/build/glib-$ABI.cross"
TOOLS_ROOT="$ROOT/android/native/tools"
MESON_VENV="$TOOLS_ROOT/meson-$MESON_VERSION"
MESON="$MESON_VENV/bin/meson"
mkdir -p "$SRC_ROOT" "$ROOT/android/native/build" "$TOOLS_ROOT"

BUILD_KEY="Lensfun=$LENSFUN_VERSION@$LENSFUN_REVISION glib=$GLIB_VERSION iconv=$ICONV_VERSION abi=$ABI api=$API ndk=$NDK_REVISION"
if [ "${AURAW_REBUILD_LENSFUN:-0}" != 1 ] \
    && [ -f "$INSTALL_DIR/include/lensfun/lensfun.h" ] \
    && [ -f "$INSTALL_DIR/lib/liblensfun.a" ] \
    && [ -f "$INSTALL_DIR/lib/libiconv.a" ] \
    && [ -f "$INSTALL_DIR/lib/libglib-2.0.a" ] \
    && [ -f "$INSTALL_DIR/lib/libintl.a" ] \
    && [ -n "$(find "$INSTALL_DIR/apk-assets/lensfun" -type f -name '*.xml' -print -quit 2>/dev/null)" ] \
    && [ -f "$INSTALL_DIR/.auraw-build" ] \
    && grep -Fqx "$BUILD_KEY" "$INSTALL_DIR/.auraw-build"; then
    echo "Using cached Lensfun $LENSFUN_VERSION for $ABI in $INSTALL_DIR"
    exit 0
fi

# Meson is a build-time implementation detail. Keep it out of the developer's
# global Python environment and include setuptools because GLib 2.78's code
# generator still imports distutils (provided by setuptools on Python 3.12+).
if [ ! -x "$MESON" ] \
    || ! "$MESON" --version 2>/dev/null | grep -Fqx "$MESON_VERSION" \
    || ! "$MESON_VENV/bin/python" -c 'import distutils.version' 2>/dev/null; then
    rm -rf "$MESON_VENV"
    python3 -m venv "$MESON_VENV" || {
        echo "Python venv support is required to bootstrap Android Lensfun's build tool." >&2
        exit 1
    }
    MESON_REQUIREMENTS="$TOOLS_ROOT/meson-requirements.txt"
    cat > "$MESON_REQUIREMENTS" <<EOF
meson==$MESON_VERSION --hash=sha256:$MESON_WHEEL_SHA256
setuptools==$SETUPTOOLS_VERSION --hash=sha256:$SETUPTOOLS_WHEEL_SHA256
EOF
    "$MESON_VENV/bin/python" -m pip install --disable-pip-version-check --no-cache-dir \
        --no-deps --only-binary=:all: --require-hashes -r "$MESON_REQUIREMENTS" || {
        rm -rf "$MESON_VENV"
        echo "Could not bootstrap Meson for Android Lensfun." >&2
        exit 1
    }
fi

fetch_archive() {
    destination=$1
    url=$2
    expected_sha256=$3
    archive=$(mktemp "$SRC_ROOT/.auraw-native.XXXXXX")
    curl --fail --location --proto "=https" --tlsv1.2 --retry 3 --output "$archive" "$url"
    printf '%s  %s\n' "$expected_sha256" "$archive" | sha256sum --check --status
    tar -xf "$archive" --strip-components=1 -C "$destination"
    rm -f "$archive"
}

if [ ! -f "$GLIB_SRC/meson.build" ] \
    || [ ! -f "$GLIB_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$GLIB_SRC/.auraw-archive-sha256")" != "$GLIB_ARCHIVE_SHA256" ]; then
    rm -rf "$GLIB_SRC"
    mkdir -p "$GLIB_SRC"
    fetch_archive "$GLIB_SRC" \
        "https://download.gnome.org/sources/glib/2.78/glib-$GLIB_VERSION.tar.xz" \
        "$GLIB_ARCHIVE_SHA256"
    printf '%s\n' "$GLIB_ARCHIVE_SHA256" > "$GLIB_SRC/.auraw-archive-sha256"
fi

if [ ! -f "$ICONV_SRC/configure" ] \
    || [ ! -f "$ICONV_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$ICONV_SRC/.auraw-archive-sha256")" != "$ICONV_ARCHIVE_SHA256" ]; then
    rm -rf "$ICONV_SRC"
    mkdir -p "$ICONV_SRC"
    fetch_archive "$ICONV_SRC" \
        "https://ftp.gnu.org/pub/gnu/libiconv/libiconv-$ICONV_VERSION.tar.gz" \
        "$ICONV_ARCHIVE_SHA256"
    printf '%s\n' "$ICONV_ARCHIVE_SHA256" > "$ICONV_SRC/.auraw-archive-sha256"
fi

if [ ! -f "$LENSFUN_SRC/CMakeLists.txt" ] \
    || [ ! -f "$LENSFUN_SRC/.auraw-archive-sha256" ] \
    || [ "$(cat "$LENSFUN_SRC/.auraw-archive-sha256")" != "$LENSFUN_ARCHIVE_SHA256" ]; then
    rm -rf "$LENSFUN_SRC"
    mkdir -p "$LENSFUN_SRC"
    fetch_archive "$LENSFUN_SRC" \
        "https://github.com/lensfun/lensfun/archive/$LENSFUN_REVISION.tar.gz" \
        "$LENSFUN_ARCHIVE_SHA256"
    printf '%s\n' "$LENSFUN_ARCHIVE_SHA256" > "$LENSFUN_SRC/.auraw-archive-sha256"
fi

cat > "$CROSS_FILE" <<EOF
[binaries]
c = '$NDK_HOST/bin/${CLANG_TARGET}-clang'
cpp = '$NDK_HOST/bin/${CLANG_TARGET}-clang++'
ar = '$NDK_HOST/bin/llvm-ar'
strip = '$NDK_HOST/bin/llvm-strip'
pkg-config = 'pkg-config'

[properties]
needs_exe_wrapper = true

[built-in options]
c_args = ['-I$INSTALL_DIR/include']
cpp_args = ['-I$INSTALL_DIR/include']
c_link_args = ['-L$INSTALL_DIR/lib']
cpp_link_args = ['-L$INSTALL_DIR/lib']

[host_machine]
system = 'android'
cpu_family = '$MESON_CPU_FAMILY'
cpu = '$MESON_CPU'
endian = 'little'
EOF

rm -rf "$ICONV_BUILD" "$GLIB_BUILD" "$LENSFUN_BUILD" "$INSTALL_DIR"
mkdir -p "$ICONV_BUILD"
(
    cd "$ICONV_BUILD"
    CC="$NDK_HOST/bin/${CLANG_TARGET}-clang" \
    CXX="$NDK_HOST/bin/${CLANG_TARGET}-clang++" \
    AR="$NDK_HOST/bin/llvm-ar" \
    RANLIB="$NDK_HOST/bin/llvm-ranlib" \
    "$ICONV_SRC/configure" --host="$AUTOCONF_HOST" --prefix="$INSTALL_DIR" \
        --disable-shared --enable-static
    make -j"$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 1)" install
)
mkdir -p "$INSTALL_DIR/lib/pkgconfig"
cat > "$INSTALL_DIR/lib/pkgconfig/iconv.pc" <<EOF
prefix=$INSTALL_DIR
libdir=\${prefix}/lib

Name: iconv
Description: GNU libiconv for Android Lensfun
Version: $ICONV_VERSION
Libs: -L\${libdir} -liconv -lcharset
EOF

PKG_CONFIG_LIBDIR="$INSTALL_DIR/lib/pkgconfig" \
PKG_CONFIG_PATH="$INSTALL_DIR/lib/pkgconfig" \
"$MESON" setup "$GLIB_BUILD" "$GLIB_SRC" --cross-file "$CROSS_FILE" --wrap-mode=forcefallback \
    --prefix "$INSTALL_DIR" --libdir lib --default-library static --buildtype release \
    -Dtests=false -Dnls=disabled -Dglib_debug=disabled -Dglib_assert=false -Dglib_checks=false \
    -Dselinux=disabled -Dxattr=false -Dlibmount=disabled -Dman=false -Dgtk_doc=false
"$MESON" compile -C "$GLIB_BUILD"
"$MESON" install -C "$GLIB_BUILD"

PKG_CONFIG_LIBDIR="$INSTALL_DIR/lib/pkgconfig" \
PKG_CONFIG_PATH="$INSTALL_DIR/lib/pkgconfig" \
"$CMAKE" -S "$LENSFUN_SRC" -B "$LENSFUN_BUILD" -GNinja \
    -DCMAKE_TOOLCHAIN_FILE="$NDK/build/cmake/android.toolchain.cmake" \
    -DANDROID_ABI="$ABI" -DANDROID_PLATFORM="android-$API" -DANDROID_STL=c++_shared \
    -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX="$INSTALL_DIR" -DCMAKE_INSTALL_LIBDIR=lib \
    -DCMAKE_INSTALL_DATAROOTDIR=apk-assets \
    -DBUILD_STATIC=ON -DBUILD_TESTS=OFF -DBUILD_LENSTOOL=OFF -DBUILD_DOC=OFF \
    -DINSTALL_PYTHON_MODULE=OFF -DINSTALL_HELPER_SCRIPTS=OFF -DBUILD_FOR_SSE=OFF -DBUILD_FOR_SSE2=OFF
"$CMAKE" --build "$LENSFUN_BUILD" --target install --parallel

test -f "$INSTALL_DIR/include/lensfun/lensfun.h"
test -f "$INSTALL_DIR/lib/liblensfun.a"
test -f "$INSTALL_DIR/lib/libiconv.a"
test -f "$INSTALL_DIR/lib/libcharset.a"
test -f "$INSTALL_DIR/lib/libglib-2.0.a"
test -f "$INSTALL_DIR/lib/libpcre2-8.a"
test -f "$INSTALL_DIR/lib/libffi.a"
test -f "$INSTALL_DIR/lib/libz.a"
test -f "$INSTALL_DIR/lib/libintl.a"
test -n "$(find "$INSTALL_DIR/apk-assets/lensfun" -type f -name '*.xml' -print -quit)"
printf '%s\n' "$BUILD_KEY" > "$INSTALL_DIR/.auraw-build"
echo "Lensfun $LENSFUN_VERSION and its database for $ABI installed in $INSTALL_DIR"
