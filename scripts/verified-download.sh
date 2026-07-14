#!/usr/bin/env sh
set -eu

usage() {
    echo "usage: $0 URL OUTPUT EXPECTED_SHA256" >&2
    exit 2
}

[ "$#" -eq 3 ] || usage
url=$1
output=$2
expected=$3

case "$url" in
    https://*) ;;
    *) echo "refusing non-HTTPS download: $url" >&2; exit 2 ;;
esac
case "$expected" in
    ""|*[!0-9a-fA-F]*) echo "invalid SHA-256 value" >&2; exit 2 ;;
esac
[ "${#expected}" -eq 64 ] || { echo "SHA-256 must contain 64 hex digits" >&2; exit 2; }

command -v curl >/dev/null 2>&1 || { echo "curl is required" >&2; exit 1; }
command -v sha256sum >/dev/null 2>&1 || { echo "sha256sum is required" >&2; exit 1; }

if [ -f "$output" ] && printf '%s  %s\n' "$expected" "$output" | sha256sum --check --status; then
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
printf '%s  %s\n' "$expected" "$temporary" | sha256sum --check --status
chmod --reference="$output" "$temporary" 2>/dev/null || true
mv -f "$temporary" "$output"
trap - EXIT HUP INT TERM
