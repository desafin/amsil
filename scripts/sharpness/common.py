"""Shared helpers for the sharpness tools: image I/O, spectra, tile classification, and the frequency
response of sharpening filters.

Images come from the Rust measurement tests (`src/raw16.rs` format). The folder is the first argument,
else `AMSIL_SHARPNESS_DIR`, else `/tmp/amsil-sharpness`.
"""
import os
import sys
from pathlib import Path

import numpy as np
from PIL import Image, ImageOps

ROOT = Path(__file__).resolve().parents[2]
SIDE = 2048
# Must match PHOTOS in src/raw16.rs: name -> (folder under DCIM, crop origin y, x)
PHOTOS = {
    "DSC04551": ("10361110", 3328, 0),
    "DSC04546": ("10361110", 3072, 0),
    "DSC04545": ("10361110", 768, 1536),
    "DSC04249": ("10161107", 1792, 256),
    "DSC04311": ("10161107", 1792, 768),
    "DSC04286": ("10161107", 768, 1536),
}
# High-ISO shots where the camera JPEG applies heavy noise reduction, so sharpness comparison is not meaningful
HIGH_ISO = {"DSC04286"}

N = 256  # tile size for spectrum measurement
BINS = np.linspace(0, 0.5, 51)
CENTRES = (BINS[:-1] + BINS[1:]) / 2  # cycles/pixel


def data_dir(argv=None):
    argv = sys.argv if argv is None else argv
    if len(argv) > 1 and not argv[1].startswith("-"):
        return Path(argv[1])
    return Path(os.environ.get("AMSIL_SHARPNESS_DIR", "/tmp/amsil-sharpness"))


def load_raw(path):
    """src/raw16.rs format -> (height, width, 3) in 0..1."""
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
    """Camera JPEG, rotated per EXIF orientation and cropped to the measurement area, as RGB (0..1)."""
    folder, y0, x0 = PHOTOS[name]
    jpeg = ImageOps.exif_transpose(Image.open(ROOT / f"DCIM/{folder}/{name}.JPG")).convert("RGB")
    return np.asarray(jpeg.crop((x0, y0, x0 + SIDE, y0 + SIDE)), dtype=np.float64) / 255.0


def to_image(rgb):
    return Image.fromarray((np.clip(rgb, 0, 1) * 255).astype(np.uint8))


def tiles(size=SIDE, n=N):
    return [(y, x) for y in range(0, size - n + 1, n) for x in range(0, size - n + 1, n)]


def spectrum(L, coords, n=N):
    """Radially averaged power spectrum over the tiles (at each frequency in CENTRES)."""
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
    """Classify tiles by mid-band (0.08-0.2) energy: (detail = top 15%, flat = bottom 25%)."""
    coords = tiles(L.shape[0])
    mid = (CENTRES > 0.08) & (CENTRES < 0.2)
    energy = np.array([spectrum(L, [c])[mid].mean() for c in coords])
    detail = [c for c, e in zip(coords, energy) if e >= np.quantile(energy, 0.85)]
    flat = [c for c, e in zip(coords, energy) if e <= np.quantile(energy, 0.25)]
    return detail, flat


def curve(a, b, coords):
    """Amplitude ratio of a to b at each frequency, normalized to 1 over 0.02-0.06 (removes brightness/contrast differences)."""
    g = np.sqrt(spectrum(a, coords) / spectrum(b, coords))
    return g / g[(CENTRES > 0.02) & (CENTRES < 0.06)].mean()


def band(g, low, high):
    """Mean of the curve over [low, high] cycles/pixel."""
    sel = (CENTRES > low) & (CENTRES < high)
    return g[sel].mean()


def at(g, frequency):
    return g[np.argmin(abs(CENTRES - frequency))]


# Frequency response of sharpening filters (radial average approximated in 1D)
def usm_gain(f, amount, r, passes=1):
    """Unsharp mask built from a box blur (radius r, `passes` passes). Amsil's creative sharpening uses r = long side x 0.0004, 1 pass."""
    n = 2 * r + 1
    with np.errstate(invalid="ignore", divide="ignore"):
        b = np.where(f == 0, 1.0, np.sin(np.pi * f * n) / (n * np.sin(np.pi * f)))
    return 1 + amount * (1 - b ** passes)


def gauss_gain(f, amount, sigma):
    return 1 + amount * (1 - np.exp(-2 * np.pi ** 2 * sigma ** 2 * f ** 2))


def bandpass_gain(f, amount, s1, s2):
    """Adds the difference of a small blur (s1) and a large blur (s2): boosts the finest band less."""
    g = lambda s: np.exp(-2 * np.pi ** 2 * s ** 2 * f ** 2)
    return 1 + amount * (g(s1) - g(s2))


# Spatial filters (no scipy)
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
