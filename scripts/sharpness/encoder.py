#!/usr/bin/env python3
"""x265 설정마다 용량과, 압축이 세부를 얼마나 남기는지.

    cargo test --release -- --ignored render_for_sharpness_measurement
    cargo test --release -- --ignored encode_for_encoder_comparison
    python3 scripts/sharpness/encoder.py [폴더]

keep: 압축 전 그림에 대한 압축 후 진폭 (세부 구역, 0.2와 0.3 cycles/pixel). final: 샤픈 없는 현상 대비.
크기는 캡처 샤픈을 끈 그림을 품질 55로 압축한 것에 대한 비율.
"""
import collections
import sys

import numpy as np

sys.path.insert(0, str(__import__("pathlib").Path(__file__).parent))
from common import PHOTOS, at, curve, data_dir, load_raw, luma, split  # noqa: E402


def main():
    folder = data_dir()
    out = folder / "encoder"
    sizes = {}
    for line in (out / "sizes.tsv").read_text().splitlines():
        stem, label, size = line.split("\t")
        sizes[(stem, label)] = int(size)
    labels = list(dict.fromkeys(label for _, label in sizes))
    sources = list(dict.fromkeys(stem.split("-", 1)[1] for stem, _ in sizes))
    rows = collections.defaultdict(list)
    for name in PHOTOS:
        plain = luma(load_raw(folder / f"{name}-off-pre.raw"))
        detail, _ = split(plain)
        reference = sizes[(f"{name}-off", "q55")]
        for source in sources:
            src = luma(load_raw(folder / f"{name}-{source}-pre.raw"))
            for label in labels:
                dec = luma(load_raw(out / f"{name}-{source}__{label}.raw"))
                keep = curve(dec, src, detail)
                final = curve(dec, plain, detail)
                psnr = 10 * np.log10(1 / np.mean((dec - src) ** 2))
                size = sizes[(f"{name}-{source}", label)] / reference * 100
                rows[(source, label)].append((size, at(keep, 0.2), at(keep, 0.3), at(final, 0.2), at(final, 0.3), psnr))
    print(f"평균 ({len(PHOTOS)}장, 2048×2048 영역)")
    print(f"{'그림':12s} {'인코더':10s} {'크기%':>6s} {'keep.2':>7s} {'keep.3':>7s} {'final.2':>8s} {'final.3':>8s} {'PSNR':>6s}")
    for source in sources:
        for label in labels:
            m = np.mean(rows[(source, label)], axis=0)
            print(f"{source:12s} {label:10s} {m[0]:6.0f} {m[1]:7.2f} {m[2]:7.2f} {m[3]:8.2f} {m[4]:8.2f} {m[5]:6.1f}")


if __name__ == "__main__":
    main()
