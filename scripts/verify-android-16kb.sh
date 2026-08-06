#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
APK=${1:-"$ROOT/android/app/build/outputs/apk/debug/app-debug.apk"}
BUILD_CONTRACT="$ROOT/android/build-contract.properties"
EXPECTED_NDK_VERSION=$(sed -n 's/^ndkVersion=//p' "$BUILD_CONTRACT")
BUILD_TOOLS_VERSION=$(sed -n 's/^buildToolsVersion=//p' "$BUILD_CONTRACT")

if [ "${1:-}" = "--print-build-contract" ]; then
    printf '{"ndkVersion":"%s","buildToolsVersion":"%s"}\n' "$EXPECTED_NDK_VERSION" "$BUILD_TOOLS_VERSION"
    exit 0
fi

if [ ! -f "$APK" ]; then
    echo "APK not found: $APK" >&2
    exit 1
fi

if [ -z "${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}" ] \
    && [ -f "$ROOT/android/local.properties" ]; then
    LOCAL_SDK=$(sed -n 's/^sdk\.dir=//p' "$ROOT/android/local.properties" | tail -n 1)
    if [ -n "$LOCAL_SDK" ]; then
        export ANDROID_SDK_ROOT="$LOCAL_SDK"
    fi
fi
SDK=${ANDROID_SDK_ROOT:-${ANDROID_HOME:-}}
if [ -z "$SDK" ]; then
    echo "Android SDK not found. Set ANDROID_SDK_ROOT (or ANDROID_HOME)." >&2
    exit 1
fi

NDK=${ANDROID_NDK_HOME:-${ANDROID_NDK_ROOT:-"$SDK/ndk/$EXPECTED_NDK_VERSION"}}
if [ ! -f "$NDK/source.properties" ]; then
    echo "Android NDK not found at $NDK" >&2
    exit 1
fi
NDK_REVISION=$(sed -n 's/^Pkg.Revision[[:space:]]*=[[:space:]]*//p' "$NDK/source.properties" | head -n 1)
if [ "$NDK_REVISION" != "$EXPECTED_NDK_VERSION" ]; then
    echo "Android NDK $EXPECTED_NDK_VERSION is required, found ${NDK_REVISION:-unknown} at $NDK" >&2
    exit 1
fi

NDK_HOST=$(find "$NDK/toolchains/llvm/prebuilt" -mindepth 1 -maxdepth 1 -type d | head -n 1)
OBJDUMP="$NDK_HOST/bin/llvm-objdump"
ZIPALIGN="$SDK/build-tools/$BUILD_TOOLS_VERSION/zipalign"
if [ ! -x "$OBJDUMP" ]; then
    echo "llvm-objdump not found: $OBJDUMP" >&2
    exit 1
fi
if [ ! -x "$ZIPALIGN" ]; then
    echo "zipalign $BUILD_TOOLS_VERSION not found: $ZIPALIGN" >&2
    exit 1
fi

TMP=$(mktemp -d "${TMPDIR:-/tmp}/auraw-16kb.XXXXXX")
trap 'rm -rf "$TMP"' EXIT HUP INT TERM
unzip -qq "$APK" 'lib/*/*.so' -d "$TMP"

FOUND_64=0
for ABI in arm64-v8a x86_64; do
    LIBDIR="$TMP/lib/$ABI"
    [ -d "$LIBDIR" ] || continue
    for SO in "$LIBDIR"/*.so; do
        [ -f "$SO" ] || continue
        FOUND_64=1
        ALIGNMENTS=$(
            "$OBJDUMP" -p "$SO" \
                | sed -n '/LOAD/s/.*align 2\*\*\([0-9][0-9]*\).*/\1/p'
        )
        if [ -z "$ALIGNMENTS" ]; then
            echo "Could not read ELF LOAD alignment from $SO" >&2
            exit 1
        fi
        if printf '%s\n' "$ALIGNMENTS" | awk '$1 < 14 { bad = 1 } END { exit bad ? 0 : 1 }'; then
            echo "16 KB ELF alignment check failed: $SO" >&2
            "$OBJDUMP" -p "$SO" | grep LOAD >&2 || true
            exit 1
        fi
        echo "16 KB ELF aligned: ${SO#$TMP/}"
    done
done

if [ "$FOUND_64" -eq 0 ]; then
    echo "No 64-bit native libraries found; ELF 16 KB check not applicable."
fi

# -P 16 verifies that uncompressed native libraries are page-aligned in the APK.
"$ZIPALIGN" -c -P 16 -v 4 "$APK"
echo "Android 16 KB page-size checks passed: $APK"
