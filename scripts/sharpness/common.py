"""선예도 측정 도구가 같이 쓰는 것: 그림 읽기·쓰기, 스펙트럼, 구역 나누기, 샤픈 필터의 주파수 응답.

그림은 Rust 쪽의 측정 테스트(`src/raw16.rs` 형식)가 만든다. 폴더는 명령의 첫 인자, 없으면
`AMSIL_SHARPNESS_DIR`, 그것도 없으면 `/tmp/amsil-sharpness`.
"""
import os
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parents[2]
SIDE = 2048
# src/raw16.rs의 PHOTOS와 같아야 한다: 이름 → (DCIM 아래 폴더, 자를 곳의 y, x)
PHOTOS = {
    "DSC04551": ("10361110", 3328, 0),
    "DSC04546": ("10361110", 3072, 0),
    "DSC04545": ("10361110", 768, 1536),
    "DSC04249": ("10161107", 1792, 256),
    "DSC04311": ("10161107", 1792, 768),
    "DSC04286": ("10161107", 768, 1536),
}
# 카메라 JPEG가 노이즈를 강하게 지워 선예도 비교가 맞지 않는 고감도 사진
HIGH_ISO = {"DSC04286"}

N = 256  # 스펙트럼을 재는 구역의 크기
BINS = np.linspace(0, 0.5, 51)
CENTRES = (BINS[:-1] + BINS[1:]) / 2  # cycles/pixel


def data_dir(argv=None):
    argv = sys.argv if argv is None else argv
    if len(argv) > 1 and not argv[1].startswith("-"):
        return Path(argv[1])
    return Path(os.environ.get("AMSIL_SHARPNESS_DIR", "/tmp/amsil-sharpness"))


def load_raw(path):
    """src/raw16.rs 형식 → (세로, 가로, 3) 0..1."""
    w, h = np.fromfile(path, dtype="<u4", count=2)
    return np.fromfile(path, dtype="<u2", offset=8).reshape(int(h), int(w), 3) / 65535.0


def save_raw(path, rgb):
    h, w, _ = rgb.shape
    with open(path, "wb") as f:
        np.array([w, h], dtype="<u4").tofile(f)
        (np.clip(rgb, 0, 1) * 65535 + 0.5).astype("<u2").tofile(f)


def luma(rgb):
    return 0.2126 * rgb[..., 0] + 0.7152 * rgb[..., 1] + 0.0722 * rgb[..., 2]


def camera_jpeg(name):
    """카메라 JPEG를 EXIF 방향대로 세워 측정 영역과 같은 자리를 자른 RGB (0..1)."""
    folder, y0, x0 = PHOTOS[name]
    jpeg = ImageOps.exif_transpose(Image.open(ROOT / f"DCIM/{folder}/{name}.JPG")).convert("RGB")
    return np.asarray(jpeg.crop((x0, y0, x0 + SIDE, y0 + SIDE)), dtype=np.float64) / 255.0


def to_image(rgb):
    return Image.fromarray((np.clip(rgb, 0, 1) * 255).astype(np.uint8))


def tiles(size=SIDE, n=N):
    return [(y, x) for y in range(0, size - n + 1, n) for x in range(0, size - n + 1, n)]


def spectrum(L, coords, n=N):
    """구역들의 방사 평균 파워 스펙트럼 (CENTRES의 주파수마다)."""
    win = np.outer(np.hanning(n), np.hanning(n))
    f = np.sqrt(np.fft.fftfreq(n)[None, :] ** 2 + np.fft.fftfreq(n)[:, None] ** 2).ravel()
    idx = np.digitize(f, BINS) - 1
    ok = idx < len(CENTRES)
    acc = np.zeros(len(CENTRES))
    cnt = np.zeros(len(CENTRES))
    for y, x in coords:
        t = L[y:y + n, x:x + n]
        p = np.abs(np.fft.fft2((t - t.mean()) * win)) ** 2
        acc += np.bincount(idx[ok], weights=p.ravel()[ok], minlength=len(acc))
        cnt += np.bincount(idx[ok], minlength=len(acc))
    return acc / np.maximum(cnt, 1)


def split(L):
    """중간 대역(0.08~0.2) 에너지로 구역을 나눈다: (세부 = 위 15%, 평평한 곳 = 아래 25%)."""
    coords = tiles(L.shape[0])
    mid = (CENTRES > 0.08) & (CENTRES < 0.2)
    energy = np.array([spectrum(L, [c])[mid].mean() for c in coords])
    detail = [c for c, e in zip(coords, energy) if e >= np.quantile(energy, 0.85)]
    flat = [c for c, e in zip(coords, energy) if e <= np.quantile(energy, 0.25)]
    return detail, flat


def curve(a, b, coords):
    """a의 진폭이 b의 몇 배인가, 주파수마다. 0.02~0.06 대역을 1로 맞춘다 (밝기·대비 차이를 뺀다)."""
    g = np.sqrt(spectrum(a, coords) / spectrum(b, coords))
    return g / g[(CENTRES > 0.02) & (CENTRES < 0.06)].mean()


def band(g, low, high):
    """곡선의 [low, high] cycles/pixel 평균."""
    sel = (CENTRES > low) & (CENTRES < high)
    return g[sel].mean()


def at(g, frequency):
    return g[np.argmin(abs(CENTRES - frequency))]


# 샤픈 필터의 주파수 응답 (방사 평균을 1D로 근사)
def usm_gain(f, amount, r, passes=1):
    """상자 흐림(반경 r, passes번)으로 만든 언샤프 마스크. 암실의 창작 샤픈은 r = 긴 변 × 0.0004, 1번."""
    n = 2 * r + 1
    with np.errstate(invalid="ignore", divide="ignore"):
        b = np.where(f == 0, 1.0, np.sin(np.pi * f * n) / (n * np.sin(np.pi * f)))
    return 1 + amount * (1 - b ** passes)


def gauss_gain(f, amount, sigma):
    return 1 + amount * (1 - np.exp(-2 * np.pi ** 2 * sigma ** 2 * f ** 2))


def bandpass_gain(f, amount, s1, s2):
    """작게 흐린 것(s1)과 크게 흐린 것(s2)의 차이를 더한다: 가장 고운 대역은 덜 키운다."""
    g = lambda s: np.exp(-2 * np.pi ** 2 * s ** 2 * f ** 2)
    return 1 + amount * (g(s1) - g(s2))


# 공간 필터 (scipy 없이)
def _separable(L, k):
    r = len(k) // 2
    P = np.pad(L, ((0, 0), (r, r)), mode="reflect")
    out = sum(k[i] * P[:, i:i + L.shape[1]] for i in range(len(k)))
    P = np.pad(out, ((r, r), (0, 0)), mode="reflect")
    return sum(k[i] * P[i:i + L.shape[0], :] for i in range(len(k)))


def box_blur(L, r, passes=1):
    for _ in range(passes):
        L = _separable(L, np.full(2 * r + 1, 1.0 / (2 * r + 1)))
    return L


def gaussian_blur(L, sigma):
    r = int(3 * sigma + 0.5)
    x = np.arange(-r, r + 1)
    k = np.exp(-x ** 2 / (2 * sigma * sigma))
    return _separable(L, k / k.sum())


def gradient(L):
    P = np.pad(L, 1, mode="reflect")
    return np.hypot((P[1:-1, 2:] - P[1:-1, :-2]) / 2, (P[2:, 1:-1] - P[:-2, 1:-1]) / 2)
