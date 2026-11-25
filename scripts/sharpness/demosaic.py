#!/usr/bin/env python3
"""Compare demosaic methods: RCD (current), PPG (previous), and the AI denoise model.

    cargo test --release -- --ignored render_for_demosaic_comparison
    python3 scripts/sharpness/demosaic.py [folder]

Prints the amplitude ratio to RCD (detail and flat areas) and chroma high-frequency energy (lower means
less color bleeding and false color), and writes side-by-side 100% crops per photo to
`demosaic/<photo>-compare.png` (leftmost is the camera JPEG).
"""
import sys

import numpy as np
from PIL import ImageDraw

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from common import PHOTOS, at, camera_jpeg, curve, data_dir, load_raw, luma, split, to_image  # noqa: E402

SHOW = [0.1, 0.2, 0.3, 0.4, 0.45]
CROP = (600, 600, 360)  # crop y, x, size (within the 2048 area)


def chroma_hf(rgb):
    y = luma(rgb)
    return (np.abs(np.diff(rgb[..., 2] - y, axis=1)).mean() + np.abs(np.diff(rgb[..., 0] - y, axis=1)).mean()) * 1000


def main():
    folder = data_dir() / "demosaic"
    print(f"{'photo':10s} {'method':6s} {'area':6s}" + "".join(f"{f:>6.2f}" for f in SHOW) + f" {'chroma HF':>9s}")
    for name in PHOTOS:
        methods = {m: load_raw(folder / f"{name}-{m}.raw") for m in ["rcd", "ppg", "ai"] if (folder / f"{name}-{m}.raw").exists()}
        if not methods:
            continue
        base = luma(methods["rcd"])
        detail, flat = split(base)
        for method, rgb in methods.items():
            L = luma(rgb)
            for kind, coords in [("detail", detail), ("flat", flat)]:
                g = curve(L, base, coords)
                tail = f" {chroma_hf(rgb):9.2f}" if kind == "detail" else ""
                print(f"{name:10s} {method:6s} {kind:6s}" + "".join(f"{at(g, f):6.2f}" for f in SHOW) + tail)
        y, x, s = CROP
        tiles = [("camera JPEG", camera_jpeg(name))] + list(methods.items())
        canvas = to_image(np.ones((s + 22, len(tiles) * (s + 8) - 8, 3)))
        draw = ImageDraw.Draw(canvas)
        for i, (label, rgb) in enumerate(tiles):
            canvas.paste(to_image(rgb[y:y + s, x:x + s]), (i * (s + 8), 22))
            draw.text((i * (s + 8) + 4, 5), label, fill="black")
        canvas.save(folder / f"{name}-compare.png")
    print(f"\nCrops: {folder}/<photo>-compare.png")


if __name__ == "__main__":
    main()
