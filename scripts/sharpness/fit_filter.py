#!/usr/bin/env python3
"""Which sharpening filter best fits the camera JPEG's sharpening curve (camera_curve.py), and what
edge-masked sharpening would do to detailed and flat areas.

    python3 scripts/sharpness/camera_curve.py [folder]    # first
    python3 scripts/sharpness/fit_filter.py [folder] [--photos DSC04551,DSC04546] [--simulate 0.22 0.02 0.05]

--simulate AMOUNT T1 T2: applies Amsil's creative sharpening (box blur radius 2) at AMOUNT to
`*-off-pre.raw`, through an edge mask that ramps 0->1 as the gradient goes from T1 to T2.
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
    """Solve the amount for each shape by least squares, then pick the shape with the smallest error."""
    best = None
    for params in grid:
        shape = model(CENTRES, 1.0, *params) - 1
        amount = np.sum(shape[FIT_BAND] * (target[FIT_BAND] - 1)) / np.sum(shape[FIT_BAND] ** 2)
        error = np.sqrt(np.mean((1 + amount * shape[FIT_BAND] - target[FIT_BAND]) ** 2))
        if best is None or error < best[0]:
            best = (error, amount, params)
    return best


def simulate(folder, names, amount, t0, t1):
    print(f"\nEdge-masked sharpening simulation (amount {amount}, box radius 2, gradient {t0}-{t1}): vs. unsharpened render, at 0.1/0.2/0.3/0.4")
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
            print(f"  {name} {kind:6s} camera " + " ".join(f"{at(cam, f):.2f}" for f in [0.1, 0.2, 0.3, 0.4])
                  + "  simulated " + " ".join(f"{at(ours, f):.2f}" for f in [0.1, 0.2, 0.3, 0.4]))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("folder", nargs="?")
    parser.add_argument("--photos", help="photos to average the curve over (comma-separated); default: all except high-ISO")
    parser.add_argument("--simulate", nargs=3, type=float, metavar=("AMOUNT", "T1", "T2"))
    args = parser.parse_args()
    folder = data_dir([sys.argv[0], args.folder] if args.folder else [sys.argv[0]])
    names = args.photos.split(",") if args.photos else [n for n in PHOTOS if n not in HIGH_ISO]

    data = json.loads((folder / "camera_curve.json").read_text())
    target = np.mean([data["photos"][n]["detail"] for n in names], axis=0)
    print(f"Target curve: detail tiles, mean of {', '.join(names)}")
    print(f"{'':36s}" + "".join(f"{f:>6.2f}" for f in SHOW) + "  error")
    print(f"{'camera':36s}" + "".join(f"{at(target, f):6.2f}" for f in SHOW))
    candidates = []
    for r in [1, 2, 3]:
        for p in [1, 2, 3]:
            err, a, _ = fit(lambda f, a, r=r, p=p: usm_gain(f, a, r, p), [()], target)
            candidates.append((f"box radius {r}, {p} pass(es)", err, a, lambda f, a, r=r, p=p: usm_gain(f, a, r, p)))
    err, a, (s,) = fit(lambda f, a, s: gauss_gain(f, a, s), [(s,) for s in np.arange(0.4, 2.51, 0.05)], target)
    candidates.append((f"Gaussian σ {s:.2f}", err, a, lambda f, a, s=s: gauss_gain(f, a, s)))
    grid = [(s1, s2) for s1 in np.arange(0.0, 1.01, 0.05) for s2 in np.arange(0.6, 3.01, 0.05) if s2 > s1 + 0.2]
    err, a, (s1, s2) = fit(lambda f, a, s1, s2: bandpass_gain(f, a, s1, s2), grid, target)
    candidates.append((f"band-pass σ {s1:.2f}/{s2:.2f}", err, a, lambda f, a, s1=s1, s2=s2: bandpass_gain(f, a, s1, s2)))
    for label, err, a, model in sorted(candidates, key=lambda c: c[1]):
        g = model(CENTRES, a)
        print(f"{label + f', amount {a:.2f}':36s}" + "".join(f"{at(g, f):6.2f}" for f in SHOW) + f"  {err:.3f}")
    if args.simulate:
        simulate(folder, names, *args.simulate)


if __name__ == "__main__":
    main()
