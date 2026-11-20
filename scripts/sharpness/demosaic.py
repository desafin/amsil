#!/usr/bin/env python3
"""색 복원(디모자익) 방식끼리 견준다: RCD(지금), PPG(예전), AI 노이즈 제거 모델.

    cargo test --release -- --ignored render_for_demosaic_comparison
    python3 scripts/sharpness/demosaic.py [폴더]

RCD에 대한 진폭 비율(세부·평평한 곳), 색 성분의 고주파(낮을수록 색 번짐·거짓 색이 적음)를 적고,
사진마다 100% 크롭을 나란히 놓은 그림을 `demosaic/<사진>-compare.png`에 쓴다 (맨 왼쪽은 카메라 JPEG).
"""
import sys

import numpy as np
from PIL import ImageDraw

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from common import PHOTOS, at, camera_jpeg, curve, data_dir, load_raw, luma, split, to_image  # noqa: E402

SHOW = [0.1, 0.2, 0.3, 0.4, 0.45]
CROP = (600, 600, 360)  # 크롭의 y, x, 크기 (2048 영역 안에서)


def chroma_hf(rgb):
    y = luma(rgb)
    return (np.abs(np.diff(rgb[..., 2] - y, axis=1)).mean() + np.abs(np.diff(rgb[..., 0] - y, axis=1)).mean()) * 1000


def main():
    folder = data_dir() / "demosaic"
    print(f"{'사진':10s} {'방식':5s} {'구역':6s}" + "".join(f"{f:>6.2f}" for f in SHOW) + f" {'색 고주파':>9s}")
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
                print(f"{name:10s} {method:5s} {kind:6s}" + "".join(f"{at(g, f):6.2f}" for f in SHOW) + tail)
        y, x, s = CROP
        tiles = [("camera JPEG", camera_jpeg(name))] + list(methods.items())
        canvas = to_image(np.ones((s + 22, len(tiles) * (s + 8) - 8, 3)))
        draw = ImageDraw.Draw(canvas)
        for i, (label, rgb) in enumerate(tiles):
            canvas.paste(to_image(rgb[y:y + s, x:x + s]), (i * (s + 8), 22))
            draw.text((i * (s + 8) + 4, 5), label, fill="black")
        canvas.save(folder / f"{name}-compare.png")
    print(f"\n크롭: {folder}/<사진>-compare.png")


if __name__ == "__main__":
    main()
