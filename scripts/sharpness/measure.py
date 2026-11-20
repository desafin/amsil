#!/usr/bin/env python3
"""캡처 샤픈 조건마다 선예도·노이즈·넘침·용량을 카메라 JPEG와 견준다.

    cargo test --release -- --ignored render_for_sharpness_measurement
    python3 scripts/sharpness/measure.py [폴더]
"""
import sys

import numpy as np

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from common import HIGH_ISO, PHOTOS, band, camera_jpeg, curve, data_dir, load_raw, luma, split  # noqa: E402


def overshoot(sharp, plain):
    """경계 둘레에서 샤픈 전 5×5의 최댓값·최솟값을 넘은 정도: (평균×1000, 0.02 넘는 화소 %)."""
    pad = np.pad(plain, 2, mode="edge")
    windows = np.lib.stride_tricks.sliding_window_view(pad, (5, 5))
    hi, lo = windows.max(axis=(-1, -2)), windows.min(axis=(-1, -2))
    edges = (hi - lo) > 0.1
    over = np.maximum(sharp - hi, 0) + np.maximum(lo - sharp, 0)
    return over[edges].mean() * 1000, (over[edges] > 0.02).mean() * 100


def main():
    folder = data_dir()
    rows = [line.split("\t") for line in (folder / "results.tsv").read_text().splitlines()[1:]]
    conditions = list(dict.fromkeys(r[1] for r in rows))
    size = {(r[0], r[1]): int(r[2]) for r in rows}
    seconds = {(r[0], r[1]): float(r[3]) for r in rows}
    auto = {r[0]: (r[4], r[5]) for r in rows}
    totals = {c: [] for c in conditions}
    header = f"  {'조건':16s} {'세부/카메라':>10s} {'압축 후':>8s} {'평평/끔':>8s} {'넘침':>6s} {'넘침%':>6s} {'용량':>6s}"
    for name in PHOTOS:
        camera = luma(camera_jpeg(name))
        plain = luma(load_raw(folder / f"{name}-off-pre.raw"))
        detail, flat = split(plain)
        cam = band(curve(camera, plain, detail), 0.15, 0.35)
        note = " (고감도: 카메라가 노이즈를 지워 비교가 맞지 않음, 평균에서 뺌)" if name in HIGH_ISO else ""
        print(f"\n{name} (자동 반경 {auto[name][0]}, 자동 문턱 {auto[name][1]}) — 카메라 JPEG의 세부: 샤픈 없는 현상의 {cam:.2f}배{note}")
        print(header + f" {'시간':>6s}")
        for c in conditions:
            pre = luma(load_raw(folder / f"{name}-{c}-pre.raw"))
            post = luma(load_raw(folder / f"{name}-{c}-post.raw"))
            values = (
                band(curve(pre, camera, detail), 0.15, 0.35),
                band(curve(post, camera, detail), 0.15, 0.35),
                band(curve(pre, plain, flat), 0.25, 0.45),
                *overshoot(pre, plain),
                size[(name, c)] / size[(name, "off")] * 100,
            )
            d, p, n, o, f, s = values
            print(f"  {c:16s} {d:10.2f} {p:8.2f} {n:8.2f} {o:6.2f} {f:6.1f} {s:5.0f}% {seconds[(name, c)]:5.1f}s")
            if name not in HIGH_ISO:
                totals[c].append(values)
    print(f"\n평균 (고감도 사진 {', '.join(sorted(HIGH_ISO))}를 뺀 {len(PHOTOS) - len(HIGH_ISO)}장)")
    print(header)
    for c in conditions:
        d, p, n, o, f, s = np.mean(totals[c], axis=0)
        print(f"  {c:16s} {d:10.2f} {p:8.2f} {n:8.2f} {o:6.2f} {f:6.1f} {s:5.0f}%")


if __name__ == "__main__":
    main()
