#!/usr/bin/env python3
"""캡처 샤픈 조건마다 선예도·노이즈·넘침·용량을 카메라 JPEG와 견준다.

먼저 측정용 그림을 만든다:
    cargo test --release -- --ignored render_for_sharpness_measurement
그 다음:
    python3 scripts/sharpness/measure.py [/tmp/amsil-sharpness]

numpy와 Pillow가 필요하다. 사진은 저장소의 DCIM/(개인 사진, git에서 빠져 있음)에서 읽는다.
"""
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parents[2]
DIR = Path(sys.argv[1] if len(sys.argv) > 1 else "/tmp/amsil-sharpness")
SIDE = 2048
# render_for_sharpness_measurement와 같은 (사진, 자를 곳의 y, x)
PHOTOS = {
    "DSC04551": ("10361110", 3328, 0),
    "DSC04546": ("10361110", 3072, 0),
    "DSC04545": ("10361110", 768, 1536),
    "DSC04249": ("10161107", 1792, 256),
    "DSC04311": ("10161107", 1792, 768),
    "DSC04286": ("10161107", 768, 1536),
}
N = 256  # 스펙트럼을 재는 구역의 크기
BINS = np.linspace(0, 0.5, 51)
CENTRES = (BINS[:-1] + BINS[1:]) / 2


def load_raw(path):
    w, h = np.fromfile(path, dtype="<u4", count=2)
    return np.fromfile(path, dtype="<u2", offset=8).reshape(int(h), int(w), 3) / 65535.0


def luma(rgb):
    return 0.2126 * rgb[..., 0] + 0.7152 * rgb[..., 1] + 0.0722 * rgb[..., 2]


def spectrum(L, coords):
    win = np.outer(np.hanning(N), np.hanning(N))
    f = np.sqrt(np.fft.fftfreq(N)[None, :] ** 2 + np.fft.fftfreq(N)[:, None] ** 2).ravel()
    idx = np.digitize(f, BINS) - 1
    ok = idx < len(CENTRES)
    acc = np.zeros(len(CENTRES))
    cnt = np.zeros(len(CENTRES))
    for y, x in coords:
        t = L[y:y + N, x:x + N]
        p = np.abs(np.fft.fft2((t - t.mean()) * win)) ** 2
        acc += np.bincount(idx[ok], weights=p.ravel()[ok], minlength=len(acc))
        cnt += np.bincount(idx[ok], minlength=len(acc))
    return acc / np.maximum(cnt, 1)


def gain(a, b, coords, band):
    """a가 b보다 그 대역에서 몇 배인가 (진폭, 0.02~0.06 대역으로 맞춘 것)."""
    g = np.sqrt(spectrum(a, coords) / spectrum(b, coords))
    g /= g[(CENTRES > 0.02) & (CENTRES < 0.06)].mean()
    sel = (CENTRES > band[0]) & (CENTRES < band[1])
    return g[sel].mean()


def overshoot(sharp, plain):
    """경계 둘레에서 샤픈 전 5×5의 최댓값·최솟값을 넘은 정도: (평균×1000, 0.02 넘는 화소 %)."""
    pad = np.pad(plain, 2, mode="edge")
    windows = np.lib.stride_tricks.sliding_window_view(pad, (5, 5))
    hi, lo = windows.max(axis=(-1, -2)), windows.min(axis=(-1, -2))
    edges = (hi - lo) > 0.1
    over = np.maximum(sharp - hi, 0) + np.maximum(lo - sharp, 0)
    return over[edges].mean() * 1000, (over[edges] > 0.02).mean() * 100


def main():
    rows = [line.split("\t") for line in (DIR / "results.tsv").read_text().splitlines()[1:]]
    conditions = list(dict.fromkeys(r[1] for r in rows))
    size = {(r[0], r[1]): int(r[2]) for r in rows}
    seconds = {(r[0], r[1]): float(r[3]) for r in rows}
    auto = {r[0]: (r[4], r[5]) for r in rows}
    totals = {c: [] for c in conditions}
    for name, (folder, y0, x0) in PHOTOS.items():
        jpeg = ImageOps.exif_transpose(Image.open(ROOT / f"DCIM/{folder}/{name}.JPG")).convert("RGB")
        camera = luma(np.asarray(jpeg.crop((x0, y0, x0 + SIDE, y0 + SIDE)), dtype=np.float64) / 255.0)
        plain = luma(load_raw(DIR / f"{name}-off-pre.raw"))
        coords = [(y, x) for y in range(0, SIDE - N + 1, N) for x in range(0, SIDE - N + 1, N)]
        mid = (CENTRES > 0.08) & (CENTRES < 0.2)
        energy = np.array([spectrum(plain, [c])[mid].mean() for c in coords])
        detail = [c for c, e in zip(coords, energy) if e >= np.quantile(energy, 0.85)]
        flat = [c for c, e in zip(coords, energy) if e <= np.quantile(energy, 0.25)]
        camera_detail = gain(camera, plain, detail, (0.15, 0.35))
        print(f"\n{name} (자동 반경 {auto[name][0]}, 자동 문턱 {auto[name][1]}) — 카메라 JPEG의 세부: 샤픈 없는 현상의 {camera_detail:.2f}배")
        print(f"  {'조건':12s} {'세부/카메라':>10s} {'압축 후':>8s} {'평평/끔':>8s} {'넘침':>6s} {'넘침%':>6s} {'용량':>6s} {'시간':>6s}")
        for c in conditions:
            pre = luma(load_raw(DIR / f"{name}-{c}-pre.raw"))
            post = luma(load_raw(DIR / f"{name}-{c}-post.raw"))
            d_pre = gain(pre, camera, detail, (0.15, 0.35))
            d_post = gain(post, camera, detail, (0.15, 0.35))
            noise = gain(pre, plain, flat, (0.25, 0.45))
            o_mean, o_frac = overshoot(pre, plain)
            rel = size[(name, c)] / size[(name, "off")] * 100
            print(f"  {c:12s} {d_pre:10.2f} {d_post:8.2f} {noise:8.2f} {o_mean:6.2f} {o_frac:6.1f} {rel:5.0f}% {seconds[(name, c)]:5.1f}s")
            totals[c].append((d_pre, d_post, noise, o_mean, o_frac, rel))
    print("\n평균 (6장)")
    print(f"  {'조건':12s} {'세부/카메라':>10s} {'압축 후':>8s} {'평평/끔':>8s} {'넘침':>6s} {'넘침%':>6s} {'용량':>6s}")
    for c in conditions:
        m = np.mean(totals[c], axis=0)
        print(f"  {c:12s} {m[0]:10.2f} {m[1]:8.2f} {m[2]:8.2f} {m[3]:6.2f} {m[4]:6.1f} {m[5]:5.0f}%")


if __name__ == "__main__":
    main()
