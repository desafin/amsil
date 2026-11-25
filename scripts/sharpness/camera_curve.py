#!/usr/bin/env python3
"""What the camera JPEG does at each spatial frequency, separately for detailed and flat areas.

This is the amplitude ratio of the camera JPEG to the unsharpened render (`*-off-pre.raw`). Above 1 means
the camera boosted that frequency (sharpening); below 1 means it cut it (noise reduction). The result is
written to `camera_curve.json` for fit_filter.py.

    cargo test --release -- --ignored render_for_sharpness_measurement
    python3 scripts/sharpness/camera_curve.py [folder]
"""
import json
import sys

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from common import CENTRES, PHOTOS, at, camera_jpeg, curve, data_dir, load_raw, luma, split  # noqa: E402

SHOW = [0.05, 0.1, 0.15, 0.2, 0.25, 0.3, 0.35, 0.4, 0.45]


def main():
    folder = data_dir()
    result = {"frequencies": CENTRES.tolist(), "photos": {}}
    print("Camera JPEG / unsharpened render, amplitude ratio (cycles/pixel)")
    print(f"{'photo':10s} {'area':6s}" + "".join(f"{f:>7.2f}" for f in SHOW))
    for name in PHOTOS:
        camera = luma(camera_jpeg(name))
        plain = luma(load_raw(folder / f"{name}-off-pre.raw"))
        detail, flat = split(plain)
        curves = {"detail": curve(camera, plain, detail), "flat": curve(camera, plain, flat)}
        for kind, g in curves.items():
            print(f"{name:10s} {kind:6s}" + "".join(f"{at(g, f):7.2f}" for f in SHOW))
        result["photos"][name] = {kind: g.tolist() for kind, g in curves.items()}
    (folder / "camera_curve.json").write_text(json.dumps(result))
    print(f"\n{folder / 'camera_curve.json'}")


if __name__ == "__main__":
    main()
