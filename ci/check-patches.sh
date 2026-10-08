#!/usr/bin/env bash
# Guards what we build on top of upstream Zebra:
#  1. each patch touches only the files allowlisted for it below;
#  2. zebrad-c's Cargo.lock pins every package it shares with upstream's Cargo.lock
#     at upstream's version and checksum.
# Usage: ci/check-patches.sh <upstream zebra source> [patches dir] [zebrad-c Cargo.lock]
set -euo pipefail

here="$(cd "$(dirname "$0")/.." && pwd)"
upstream="${1:?usage: $0 <upstream zebra source> [patches dir] [Cargo.lock]}"
patches="${2:-$here/patches/zebra}"
lock="${3:-$here/zebrad-c/Cargo.lock}"

# The files each patch needs. Consensus, chain, state, network and script code stay
# upstream's: only 0003 touches zebra-consensus, to expose a constructor.
allowed() {
  case "$1" in
    0001-*) echo "zebrad/src/commands/start.rs" ;;
    0002-*) echo "zebrad/src/components/sync/end_of_support.rs" ;;
    0003-*) echo "zebra-consensus/src/lib.rs zebra-consensus/src/primitives.rs" ;;
    0004-*) echo "zebra-rpc/src/lightwalletd/server.rs zebrad/src/commands/start.rs" ;;
  esac
}

fail=0
n=0
for p in "$patches"/*.patch; do
  name="$(basename "$p")"
  n=$((n + 1))
  ok="$(allowed "$name")"
  if [ -z "$ok" ]; then
    echo "FAIL $name: no allowlist entry in ci/check-patches.sh" >&2
    fail=1
    continue
  fi
  # Both sides of every diff header, so a rename cannot slip a path past the list.
  touched="$(awk '/^diff --git /{sub(/^a\//, "", $3); sub(/^b\//, "", $4); print $3; print $4}' "$p" | sort -u)"
  [ -n "$touched" ] || { echo "FAIL $name: no diff found" >&2; fail=1; continue; }
  bad=0
  for f in $touched; do
    case " $ok " in
      *" $f "*) ;;
      *) echo "FAIL $name touches $f, which is not on its allowlist" >&2; bad=1 ;;
    esac
  done
  if [ "$bad" = 0 ]; then echo "ok   $name: $(echo $touched)"; else fail=1; fi
done
[ "$n" -gt 0 ] || { echo "FAIL: no patches in $patches" >&2; exit 1; }

python3 - "$upstream/Cargo.lock" "$lock" <<'PY' || fail=1
import sys, tomllib

def packages(path):
    with open(path, "rb") as f:
        return tomllib.load(f)["package"]

upstream = {}
for p in packages(sys.argv[1]):
    upstream.setdefault(p["name"], {})[p["version"]] = (p.get("source"), p.get("checksum"))

ours = packages(sys.argv[2])
shared, extra, bad = 0, [], []
for p in ours:
    versions = upstream.get(p["name"])
    if versions is None:
        extra.append(f'{p["name"]} {p["version"]}')
        continue
    shared += 1
    if p["version"] not in versions:
        bad.append(f'{p["name"]} {p["version"]}: upstream has {", ".join(sorted(versions))}')
    elif versions[p["version"]] != (p.get("source"), p.get("checksum")):
        bad.append(f'{p["name"]} {p["version"]}: source or checksum differs from upstream')

for b in bad:
    print(f"FAIL Cargo.lock {b}", file=sys.stderr)
print(f"Cargo.lock: {shared} packages shared with upstream, {len(bad)} disagree; "
      f"only ours: {', '.join(extra) or 'none'}")
sys.exit(1 if bad else 0)
PY

[ "$fail" = 0 ] && echo "patch guard: OK" || { echo "patch guard: FAILED" >&2; exit 1; }
