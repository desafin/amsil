#!/usr/bin/env bash
# Regenerates the reference outputs (tests/data/rcd/*.bin) from the original RCD. Requires gcc.
# The original source is copied to a temporary folder and built there.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/tests/data/rcd"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp "$here/harness.c" "$here/rcd_demosaicing.c" "$work/"
# Keep floating-point deterministic: no FMA contraction, no fast-math.
gcc -O2 -std=gnu99 -ffp-contract=off -fno-fast-math -o "$work/harness" "$work/harness.c" -lm

mkdir -p "$out"
for layout in rggb:94949494 bggr:16161616 grbg:61616161 gbrg:49494949; do
    name="${layout%%:*}"
    filters="${layout#*:}"
    for content in random edges; do
        "$work/harness" 47 41 "$filters" "$content" "$out/$name-$content.bin"
    done
done
ls -l "$out"
