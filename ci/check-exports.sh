#!/usr/bin/env bash
# Asserts libzebrad_c exports exactly the functions its header declares, nothing else.
# Usage: ci/check-exports.sh <libzebrad_c.dylib|.so> <zebrad_c.h>
set -euo pipefail
lib="$1"
header="$2"

case "$(uname -s)" in
  Darwin) exports() { nm -gU "$1" | awk '{print $NF}' | sed 's/^_//'; } ;;
  Linux) exports() { nm -D --defined-only "$1" | awk '$2 ~ /^[A-Z]$/ {print $NF}'; } ;;
  *) echo "unsupported host $(uname -s)" >&2; exit 1 ;;
esac

want="$(grep -o -E 'ZEBRAD_[a-z_]+\(' "$header" | tr -d '(' | sort -u)"
got="$(exports "$lib" | sort -u)"
if [ "$got" != "$want" ]; then
  echo "exports of $lib differ from $header (< header, > library):" >&2
  diff <(echo "$want") <(echo "$got") >&2 || true
  exit 1
fi
echo "exports: $(echo "$got" | wc -l | tr -d ' ') ZEBRAD_* functions and nothing else"
