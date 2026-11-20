#!/usr/bin/env python3
"""카메라 JPEG의 샤픈 곡선(camera_curve.py)에 어떤 샤픈 필터가 맞는지, 그리고 경계에만 거는 샤픈을
흉내 내면 세부와 평평한 곳이 어떻게 되는지.

    python3 scripts/sharpness/camera_curve.py [폴더]    # 먼저
    python3 scripts/sharpness/fit_filter.py [폴더] [--photos DSC04551,DSC04546] [--simulate 0.22 0.02 0.05]

--simulate 세기 문턱1 문턱2: 암실의 창작 샤픈(상자 흐림 반경 2)을 그 세기로, 기울기가 문턱1~문턱2인 곳에서
0→1로 커지는 경계 마스크를 씌워 `*-off-pre.raw`에 걸어 본다.
"""
import argparse
import json
import sys

import numpy as np

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from common import (  # noqa: E402
    CENTRES, HIGH_ISO, PHOTOS, at, bandpass_gain, box_blur, camera_jpeg, curve, data_dir, gauss_gain,
    gaussian_blur, gradient, load_raw, luma, split, usm_gain,
)

SHOW = [0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45]
FIT_BAND = (CENTRES > 0.04) & (CENTRES < 0.46)


def fit(model, grid, target):
    """모양마다 세기는 최소제곱으로 구하고, 오차가 가장 작은 모양을 고른다."""
    best = None
    for params in grid:
        shape = model(CENTRES, 1.0, *params) - 1
        amount = np.sum(shape[FIT_BAND] * (target[FIT_BAND] - 1)) / np.sum(shape[FIT_BAND] ** 2)
        error = np.sqrt(np.mean((1 + amount * shape[FIT_BAND] - target[FIT_BAND]) ** 2))
        if best is None or error < best[0]:
            best = (error, amount, params)
    return best


def simulate(folder, names, amount, t0, t1):
    print(f"\n경계 마스크 샤픈 흉내 (세기 {amount}, 상자 반경 2, 기울기 {t0}~{t1}): 샤픈 없는 현상 대비, 0.1/0.2/0.3/0.4")
    for name in names:
        rgb = load_raw(folder / f"{name}-off-pre.raw")
        L = luma(rgb)
        detail, flat = split(L)
        camera = luma(camera_jpeg(name))
        mask = np.clip((gaussian_blur(gradient(L), 1.5) - t0) / (t1 - t0), 0, 1)
        sharp = np.clip(L + amount * (L - box_blur(L, 2)) * mask, 0, 1)
        for kind, coords in [("detail", detail), ("flat", flat)]:
            cam = curve(camera, L, coords)
            ours = curve(sharp, L, coords)
            print(f"  {name} {kind:6s} 카메라 " + " ".join(f"{at(cam, f):.2f}" for f in [0.1, 0.2, 0.3, 0.4])
                  + "  흉내 " + " ".join(f"{at(ours, f):.2f}" for f in [0.1, 0.2, 0.3, 0.4]))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("folder", nargs="?")
    parser.add_argument("--photos", help="곡선을 평균할 사진 (쉼표로). 기본은 고감도를 뺀 전부")
    parser.add_argument("--simulate", nargs=3, type=float, metavar=("세기", "문턱1", "문턱2"))
    args = parser.parse_args()
    folder = data_dir([sys.argv[0], args.folder] if args.folder else [sys.argv[0]])
    names = args.photos.split(",") if args.photos else [n for n in PHOTOS if n not in HIGH_ISO]

    data = json.loads((folder / "camera_curve.json").read_text())
    target = np.mean([data["photos"][n]["detail"] for n in names], axis=0)
    print(f"맞출 곡선: 세부 구역, {', '.join(names)}의 평균")
    print(f"{'':36s}" + "".join(f"{f:>6.2f}" for f in SHOW) + "   오차")
    print(f"{'카메라':36s}" + "".join(f"{at(target, f):6.2f}" for f in SHOW))
    candidates = []
    for r in [1, 2, 3]:
        for p in [1, 2, 3]:
            err, a, _ = fit(lambda f, a, r=r, p=p: usm_gain(f, a, r, p), [()], target)
            candidates.append((f"상자 반경 {r}, {p}번", err, a, lambda f, a, r=r, p=p: usm_gain(f, a, r, p)))
    err, a, (s,) = fit(lambda f, a, s: gauss_gain(f, a, s), [(s,) for s in np.arange(0.4, 2.51, 0.05)], target)
    candidates.append((f"가우스 σ {s:.2f}", err, a, lambda f, a, s=s: gauss_gain(f, a, s)))
    grid = [(s1, s2) for s1 in np.arange(0.0, 1.01, 0.05) for s2 in np.arange(0.6, 3.01, 0.05) if s2 > s1 + 0.2]
    err, a, (s1, s2) = fit(lambda f, a, s1, s2: bandpass_gain(f, a, s1, s2), grid, target)
    candidates.append((f"대역 통과 σ {s1:.2f}/{s2:.2f}", err, a, lambda f, a, s1=s1, s2=s2: bandpass_gain(f, a, s1, s2)))
    for label, err, a, model in sorted(candidates, key=lambda c: c[1]):
        g = model(CENTRES, a)
        print(f"{label + f', 세기 {a:.2f}':36s}" + "".join(f"{at(g, f):6.2f}" for f in SHOW) + f"  {err:.3f}")
    if args.simulate:
        simulate(folder, names, *args.simulate)


if __name__ == "__main__":
    main()
