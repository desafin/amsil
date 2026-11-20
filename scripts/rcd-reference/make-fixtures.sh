#!/usr/bin/env bash
# 원본 RCD로 정답지(tests/data/rcd/*.bin)를 다시 만든다. gcc가 필요하다.
# 원본 코드는 임시 폴더에 복사해서 그 안에서 빌드한다.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/tests/data/rcd"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

cp "$here/harness.c" "$here/rcd_demosaicing.c" "$work/"
# 부동소수점 계산을 정해진 대로 하게: FMA로 묶지 않고, 빠른 수학을 끈다.
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
