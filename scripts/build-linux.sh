#!/bin/sh
set -eu

ROOT=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
if [ "$#" -ne 0 ]; then
    echo "build-linux.sh does not accept Cargo overrides" >&2
    exit 2
fi
REVISION=$(sh "$ROOT/scripts/verify-source-revision.sh")

export AURAW_REQUIRE_COMMITTED_SOURCE=1
export AURAW_SOURCE_REVISION="$REVISION"
export SOURCE_DATE_EPOCH="$(git -C "$ROOT" show -s --format=%ct "$REVISION")"
export CARGO_INCREMENTAL=0
export CARGO_TARGET_DIR="$ROOT/target"
unset CARGO_BUILD_TARGET CARGO_ENCODED_RUSTFLAGS RUSTFLAGS RUSTDOCFLAGS

cargo build --locked --release --manifest-path "$ROOT/Cargo.toml"
test -f "$ROOT/target/release/auraw"
test -f "$ROOT/target/release/auraw-regression-render"

if ! FINAL_REVISION=$(sh "$ROOT/scripts/verify-source-revision.sh") \
    || [ "$FINAL_REVISION" != "$REVISION" ]; then
    rm -f "$ROOT/target/release/auraw" \
        "$ROOT/target/release/auraw-regression-render"
    echo "source changed during the build; discarded the Linux binary" >&2
    exit 1
fi

printf 'Built AuRaw from %s\n' "$REVISION"
