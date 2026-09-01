#!/usr/bin/env bash
set -euo pipefail

output=${1:-THIRD_PARTY_LICENSES.md}
temporary=$(mktemp "${TMPDIR:-/tmp}/calibraw-licenses.XXXXXX")
trap 'rm -f "$temporary"' EXIT

# cargo-about has emitted CRLF in otherwise identical output on some hosts.
# Normalize only line endings so the checked-in notice is reproducible without
# changing the license text itself.
LC_ALL=C cargo about generate --locked --workspace --fail \
  -o "$temporary" about.hbs
sed 's/\r$//' "$temporary" > "$output"
