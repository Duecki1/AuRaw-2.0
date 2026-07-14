#!/usr/bin/env sh
set -eu

usage() {
    echo "usage: $0 URL OUTPUT EXPECTED_DIGEST_OR_HTTPS_SHA256_URL" >&2
    echo "digest formats: 64-hex SHA-256, sha256:HEX, or sha512:HEX" >&2
    exit 2
}

[ "$#" -eq 3 ] || usage
url=$1
output=$2
expected_source=$3

case "$url" in
    https://*) ;;
    *) echo "refusing non-HTTPS download: $url" >&2; exit 2 ;;
esac

command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }

algorithm=
expected=
case "$expected_source" in
    https://*)
        checksum_text="$(
            curl --proto '=https' --tlsv1.2 --http1.1 \
                --fail --location --show-error \
                --retry 8 --retry-all-errors --retry-delay 3 \
                --connect-timeout 30 --max-time 300 \
                "$expected_source"
        )"
        algorithm=sha256
        expected="$(printf '%s\n' "$checksum_text" | grep -Eo '[0-9a-fA-F]{64}' | head -n 1 || true)"
        ;;
    sha256:*)
        algorithm=sha256
        expected=${expected_source#sha256:}
        ;;
    sha512:*)
        algorithm=sha512
        expected=${expected_source#sha512:}
        ;;
    *)
        algorithm=sha256
        expected=$expected_source
        ;;
esac

case "$expected" in
    ""|*[!0-9a-fA-F]*) echo "invalid checksum value" >&2; exit 2 ;;
esac

case "$algorithm" in
    sha256)
        [ "${#expected}" -eq 64 ] || {
            echo "SHA-256 must contain 64 hex digits" >&2
            exit 2
        }
        checksum_command=sha256sum
        ;;
    sha512)
        [ "${#expected}" -eq 128 ] || {
            echo "SHA-512 must contain 128 hex digits" >&2
            exit 2
        }
        checksum_command=sha512sum
        ;;
    *)
        echo "unsupported checksum algorithm: $algorithm" >&2
        exit 2
        ;;
esac

command -v "$checksum_command" >/dev/null 2>&1 || {
    echo "$checksum_command is required" >&2
    exit 1
}

# Normalize once so uppercase digests are accepted and diagnostics are stable.
expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"

verify_file() {
    actual="$("$checksum_command" "$1" | awk '{print $1}')" || return 1
    if [ "$actual" != "$expected" ]; then
        echo "$algorithm checksum mismatch for $1" >&2
        echo "expected: $expected" >&2
        echo "actual:   $actual" >&2
        return 1
    fi
}

# A stale or corrupt cache entry should trigger a fresh download without making
# a successful recovery look like a failed build.
if [ -f "$output" ] && verify_file "$output" 2>/dev/null; then
    echo "verified cached download: $output"
    exit 0
fi

temporary="${output}.download.$$"
trap 'rm -f "$temporary"' EXIT HUP INT TERM
rm -f "$temporary"
curl --proto '=https' --tlsv1.2 --http1.1 \
    --fail --location --show-error \
    --retry 8 --retry-all-errors --retry-delay 3 \
    --connect-timeout 30 --max-time 900 \
    "$url" -o "$temporary"
verify_file "$temporary"
chmod --reference="$output" "$temporary" 2>/dev/null || true
mv -f "$temporary" "$output"
trap - EXIT HUP INT TERM
