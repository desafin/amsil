//! Capture sharpening: undoes blur from the lens, low-pass filter, and diffraction at the RAW
//! stage.
//!
//! A port of RawTherapee's Capture Sharpening (Ingo Weyrich, GPL-3.0-or-later).
//! Reference version: `rtengine/capturesharpening.cc` and `boxblur.cc` at commit c6d04960e14f,
//! `rtengine/rt_algo.cc` at commit 1c2b3f33772e (<https://github.com/RawTherapee/RawTherapee>).
//! Richardson–Lucy deconvolution runs on luminance only, and is blended in only where edges are
//! distinct.
//!
//! Differences from RawTherapee:
//! - Iterates over the whole image instead of computing 32×32 tiles separately (the tiling is for
//!   speed). Only the auto-stop decision is made per tile.
//! - Keeps highlight protection even when the contrast threshold is 0 (RawTherapee drops the
//!   protection mask in that case).
//! - Does not blend the last tile twice. The corner radius boost does not use the previous tile's
//!   value.
//! - Pre- and post-sharpening noise reduction and X-Trans sensors are not ported.

use crate::raw::Sensor;
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

/// Per-photo capture sharpening settings (`capture` in the project file).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capture {
    /// Whether to apply capture sharpening.
    pub enabled: bool,
    /// Iteration count. More iterations undo more blur (acts as the strength).
    pub iterations: u32,
    /// Radius (σ, pixels) of the blur to undo. Derived from the photo if unset.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// Contrast threshold. Higher values sharpen only distinct edges. Derived from the photo if
    /// unset (RawTherapee's auto).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    /// Amount added to the radius towards the corners.
    pub corner: f32,
    /// Stop iterating in a tile once overshoot appears.
    pub auto_stop: bool,
}

/// Default contrast threshold. RawTherapee derives it from the photo (auto), but that comes out
/// about 1.3× stronger than the camera JPEG and sharpens even the texture noise in trees and grass,
/// growing file size by 28%. In the 2026-11-20 measurement (`scripts/sharpness`), 0.4 matched the
/// camera JPEG after compression (1.02×) with a 10% size increase. The user chose this value.
pub const DEFAULT_THRESHOLD: f32 = 0.4;

impl Default for Capture {
    fn default() -> Self {
        Self {
            enabled: true,
            iterations: 20,
            radius: None,
            threshold: Some(DEFAULT_THRESHOLD),
            corner: 0.0,
            auto_stop: true,
        }
    }
}

impl Capture {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// Settings with capture sharpening off (e.g. the unadjusted image in before/after comparison).
    pub fn off() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// Sharpening area view: how the preview shows where capture sharpening applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureView {
    /// Red over the photo (same color as the [Mask] panel).
    Overlay,
    /// Black-and-white map: white is sharpened, black is left alone.
    Map,
}

/// Selectable iteration counts.
pub const ITERATIONS: RangeInclusive<u32> = 1..=100;
/// Selectable radii.
pub const RADIUS: RangeInclusive<f32> = 0.4..=2.0;
/// Selectable contrast thresholds.
pub const THRESHOLD: RangeInclusive<f32> = 0.0..=2.0;
/// Selectable corner radii.
pub const CORNER: RangeInclusive<f32> = -0.5..=0.5;

/// Upper limit of the auto radius (RawTherapee's `maxSigma`).
const MAX_SIGMA: f32 = 2.0;
/// Sensor values at or above this fraction count as saturated and their surroundings are protected
/// (RawTherapee's `clipLimit`).
const CLIP_LIMIT: f32 = 0.95;

/// Kernel radius for blur radius σ. The choice used without the corner boost (RawTherapee
/// CaptureDeconvSharpening).
fn kernel_radius(sigma: f32) -> usize {
    if sigma < 0.6 {
        1
    } else if sigma <= 0.84 {
        2
    } else if sigma <= 1.15 {
        3
    } else if sigma <= 1.5 {
        4
    } else {
        6
    }
}

/// Gaussian kernel of radius `r` ((2r+1)² coefficients, row-major). Same computation as
/// RawTherapee's `compute{3,5,7,9,13}x…kernel2`: zero outside the support, `exp` in double, sum in
/// float.
fn kernel_of(sigma: f32, r: usize) -> Vec<f32> {
    // Support: i² + j² ≤ limit. 3×3 has no limit.
    let limit = match r {
        1 => f64::INFINITY,
        2 => (3.0f64 * 0.84).powi(2),
        3 => (3.0f64 * 1.15).powi(2),
        4 => (3.0f64 * 1.5).powi(2),
        6 => (3.0f64 * 2.0).powi(2),
        _ => unreachable!("kernel radius {r}"),
    };
    let temp = (-2.0f32 * sigma * sigma) as f64;
    let n = 2 * r + 1;
    let mut kernel = vec![0f32; n * n];
    let mut sum = 0f32;
    let r = r as i32;
    for i in -r..=r {
        for j in -r..=r {
            let d2 = (i * i + j * j) as f64;
            if d2 <= limit {
                let v = (d2 / temp).exp() as f32;
                kernel[((i + r) * (2 * r + 1) + j + r) as usize] = v;
                sum += v;
            }
        }
    }
    kernel.iter_mut().for_each(|v| *v /= sum);
    kernel
}

/// Kernel for blur radius σ: (radius, coefficients).
#[cfg(test)]
fn kernel(sigma: f32) -> (usize, Vec<f32>) {
    let r = kernel_radius(sigma);
    (r, kernel_of(sigma, r))
}

/// Highlight protection: a mask that is 0 on the 21 pixels (5×5 minus the four corners) around each
/// saturated sensor value (0.95 or more) and 1 elsewhere. Built over the full sensor, then cropped
/// to the output area (RawTherapee `buildClipMaskBayer`).
fn highlight_guard(sensor: &Sensor) -> Vec<f32> {
    let (w, h) = (sensor.width, sensor.height);
    let mut guard = vec![1f32; w * h];
    for row in 2..h.saturating_sub(2) {
        for col in 2..w.saturating_sub(2) {
            if sensor.cfa[row * w + col] >= CLIP_LIMIT {
                for dy in -2isize..=2 {
                    for dx in -2isize..=2 {
                        if dy.abs() == 2 && dx.abs() == 2 {
                            continue;
                        }
                        let (y, x) = (row.wrapping_add_signed(dy), col.wrapping_add_signed(dx));
                        guard[y * w + x] = 0.0;
                    }
                }
            }
        }
    }
    let (cx, cy, cw, ch) = sensor.crop;
    let mut out = Vec::with_capacity(cw * ch);
    for row in cy..cy + ch {
        out.extend_from_slice(&guard[row * w + cx..row * w + cx + cw]);
    }
    out
}

/// Per-channel multipliers that convert camera RGB to RawTherapee units: apply white balance and
/// scale so the saturation of the channel with the largest multiplier becomes 65535 (RawTherapee
/// `scale_mul`).
fn rt_scale(wb: [f32; 3]) -> [f32; 3] {
    let k = 65535.0 / wb.iter().copied().fold(f32::MIN, f32::max);
    wb.map(|c| c * k)
}

/// Auto radius: σ = sqrt(1 / ln R) from the maximum ratio R between diagonally adjacent green
/// pixels (RawTherapee `calcRadiusBayer`). Capped at 2.0.
fn auto_radius(sensor: &Sensor, wb: [f32; 3]) -> f32 {
    let (w, h) = (sensor.width, sensor.height);
    let green = rt_scale(wb)[1];
    let raw = |row: usize, col: usize| sensor.cfa[row * w + col] * green;
    let (lower, upper) = (1000f32, green);
    let mut max_ratio = 1f32;
    for row in 4..h.saturating_sub(4) {
        // Start at the green columns: even columns (from 6) if this row starts with green,
        // otherwise odd columns (from 5).
        let start = if crate::rcd::colour_at(0, row, sensor.red) == 1 {
            6
        } else {
            5
        };
        for col in (start..w.saturating_sub(4)).step_by(2) {
            let v00 = raw(row, col);
            if v00 <= 0.0 {
                continue;
            }
            let v1m1 = raw(row + 1, col - 1);
            let v1p1 = raw(row + 1, col + 1);
            let max0 = v00.max(v1m1);
            if v1m1 > 0.0 && max0 > lower {
                let min = v00.min(v1m1);
                if max0 > max_ratio * min {
                    // Check whether a saturated neighboring green is involved.
                    let clipped = if max0 == v00 {
                        raw(row - 1, col - 1).max(raw(row - 1, col + 1)).max(v1p1) >= upper
                    } else {
                        raw(row, col - 2)
                            .max(v00)
                            .max(raw(row + 2, col - 2))
                            .max(raw(row + 2, col))
                            >= upper
                    };
                    if !clipped {
                        max_ratio = max0 / min;
                    }
                }
            }
            let max1 = v00.max(v1p1);
            if v1p1 > 0.0 && max1 > lower {
                let min = v00.min(v1p1);
                if max1 > max_ratio * min {
                    let clipped = if max1 == v00 {
                        raw(row - 1, col - 1).max(raw(row - 1, col + 1)).max(v1p1) >= upper
                    } else {
                        v00.max(raw(row, col + 2))
                            .max(raw(row + 2, col))
                            .max(raw(row + 2, col + 2))
                            >= upper
                    };
                    if !clipped {
                        max_ratio = max1 / min;
                    }
                }
            }
        }
    }
    (1.0 / max_ratio.ln()).sqrt().min(MAX_SIGMA)
}

/// CIE L* for contrast measurement, in RawTherapee units (0..32768 = L* × 327.68). Input is RGB in
/// RawTherapee units (`Color::RGB2L`, luminance y with BT.709 weights).
fn lightness_at(r: f32, g: f32, b: f32) -> f32 {
    let y = 0.212671 * r + 0.715160 * g + 0.072169 * b;
    if y > 65535.0 * 216.0 / 24389.0 {
        327.68 * (116.0 * (y / 65535.0).cbrt() - 16.0)
    } else {
        327.68 * (24389.0 / 27.0) * y / 65535.0
    }
}

#[cfg(test)]
fn lightness(rgb: &[[f32; 3]]) -> Vec<f32> {
    rgb.iter().map(|&[r, g, b]| lightness_at(r, g, b)).collect()
}

/// Sigmoid that maps contrast to a blend factor. 0.5 at the threshold (RawTherapee
/// `calcBlendFactor`).
fn blend_factor(contrast: f32, threshold: f32) -> f32 {
    let x = -16.0 + (16.0 / threshold) * contrast;
    0.5 * (1.0 + x / (1.0 + x * x).sqrt())
}

/// Local contrast at pixel (j, i): vertical and horizontal differences at distances 1 and 2
/// (contrast computation in RawTherapee `buildBlendMask`).
fn contrast_at(l: &[f32], w: usize, j: usize, i: usize) -> f32 {
    let at = |y: usize, x: usize| l[y * w + x];
    let sq = |a: f32, b: f32| (a - b) * (a - b);
    (sq(at(j, i + 1), at(j, i - 1))
        + sq(at(j + 1, i), at(j - 1, i))
        + sq(at(j, i + 2), at(j, i - 2))
        + sq(at(j + 2, i), at(j - 2, i)))
    .sqrt()
        * (0.0625 / 327.68)
}

fn tile_average(l: &[f32], w: usize, y0: usize, x0: usize, size: usize) -> f32 {
    let mut sum = 0f32;
    for y in y0..y0 + size {
        for v in &l[y * w + x0..y * w + x0 + size] {
            sum += v;
        }
    }
    sum / (size * size) as f32
}

/// A tile's variance divided by its mean (RawTherapee `tileVariance`).
fn tile_variance(l: &[f32], w: usize, y0: usize, x0: usize, size: usize, average: f32) -> f32 {
    let mut sum = 0f32;
    for y in y0..y0 + size {
        for v in &l[y * w + x0..y * w + x0 + size] {
            sum += (v - average) * (v - average);
        }
    }
    sum / ((size * size) as f32 * average)
}

/// A tile's variance/mean. Tiles that are too dark, too bright, or too uniform (not even noise) are
/// not used (infinity).
fn usable_variance(l: &[f32], w: usize, y0: usize, x0: usize, size: usize) -> f32 {
    const MIN_LUMINANCE: f32 = 2000.0;
    const MAX_LUMINANCE: f32 = 20000.0;
    const MIN_TILE_VARIANCE: f32 = 0.5;
    let average = tile_average(l, w, y0, x0, size);
    if !(MIN_LUMINANCE..=MAX_LUMINANCE).contains(&average) {
        return f32::INFINITY;
    }
    let variance = tile_variance(l, w, y0, x0, size, average);
    if variance < MIN_TILE_VARIANCE {
        f32::INFINITY
    } else {
        variance
    }
}

/// In a flat tile, the smallest threshold at which the mean blend factor is 1% or less, plus 0.01
/// (RawTherapee `calcContrastThreshold`).
fn contrast_threshold_of_tile(l: &[f32], w: usize, y0: usize, x0: usize, size: usize) -> f32 {
    let mut contrasts = Vec::with_capacity((size - 4) * (size - 4));
    for j in y0 + 2..y0 + size - 2 {
        for i in x0 + 2..x0 + size - 2 {
            contrasts.push(contrast_at(l, w, j, i));
        }
    }
    let limit = ((size - 4) * (size - 4)) as f32 / 100.0;
    let mut c = 1;
    while c < 100 {
        let threshold = c as f32 / 100.0;
        let sum: f32 = contrasts.iter().map(|&v| blend_factor(v, threshold)).sum();
        if sum <= limit {
            break;
        }
        c += 1;
    }
    (c + 1) as f32 / 100.0
}

/// Auto contrast threshold: finds the flattest (noise-only) tile in the photo and returns the
/// threshold at which that tile is barely sharpened. 0 if no suitable tile exists (autoContrast in
/// RawTherapee `buildBlendMask`).
fn auto_threshold(l: &[f32], w: usize, h: usize) -> f32 {
    // Stage 1: scan non-overlapping 80×80 tiles. Stop as soon as one has variance/mean of 1 or
    // less.
    let mut best = (f32::INFINITY, 0, 0);
    for y in (0..h / 80).map(|i| i * 80) {
        for x in (0..w / 80).map(|j| j * 80) {
            let v = usable_variance(l, w, y, x, 80);
            if v < best.0 {
                best = (v, y, x);
            }
        }
    }
    if best.0 <= 1.0 {
        return contrast_threshold_of_tile(l, w, best.1, best.2, 80);
    }
    // Stage 2: scan 40×40 tiles in 10-pixel steps, then rescan ±10 pixels around the best one in
    // 1-pixel steps.
    let (tiles_h, tiles_w) = ((h / 10).saturating_sub(3), (w / 10).saturating_sub(3));
    let mut best = (f32::INFINITY, 0, 0);
    for y in (0..tiles_h).map(|i| i * 10) {
        for x in (0..tiles_w).map(|j| j * 10) {
            let v = usable_variance(l, w, y, x, 40);
            if v < best.0 {
                best = (v, y, x);
            }
        }
    }
    if h < 40 || w < 40 {
        return 0.0;
    }
    let (min_y, min_x) = (best.1, best.2);
    let (y_start, x_start) = (min_y.saturating_sub(10), min_x.saturating_sub(10));
    let (y_end, x_end) = ((min_y + 10).min(h - 40), (min_x + 10).min(w - 40));
    let mut best = (f32::INFINITY, 0, 0);
    for y in y_start..=y_end {
        for x in x_start..=x_end {
            let v = usable_variance(l, w, y, x, 40);
            if v < best.0 {
                best = (v, y, x);
            }
        }
    }
    if best.0 <= 8.0 {
        contrast_threshold_of_tile(l, w, best.1, best.2, 40)
    } else {
        0.0
    }
}

/// Blend mask (0..1): near 1 on distinct edges and near 0 on flat areas, multiplied by highlight
/// protection and then blurred with σ 2 (RawTherapee `buildBlendMask`). With a threshold of 0 it is
/// 1 regardless of contrast, but unlike RawTherapee highlight protection is still applied.
pub fn blend_mask(l: &[f32], guard: &[f32], w: usize, h: usize, threshold: f32) -> Vec<f32> {
    if threshold == 0.0 || w < 5 || h < 5 {
        return gaussian_blur(guard, w, h, 2.0);
    }
    let mut blend = vec![0f32; w * h];
    for j in 2..h - 2 {
        for i in 2..w - 2 {
            blend[j * w + i] = guard[j * w + i] * blend_factor(contrast_at(l, w, j, i), threshold);
        }
    }
    // The outer 2 pixels copy the adjacent inner values.
    for j in 0..2 {
        for i in 2..w - 2 {
            blend[j * w + i] = blend[2 * w + i];
            blend[(h - 1 - j) * w + i] = blend[(h - 3) * w + i];
        }
    }
    for j in 0..h {
        let row = &mut blend[j * w..(j + 1) * w];
        row[0] = row[2];
        row[1] = row[2];
        row[w - 2] = row[w - 3];
        row[w - 1] = row[w - 3];
    }
    gaussian_blur(&blend, w, h, 2.0)
}

/// Separable Gaussian blur (radius 3σ, edges repeat the last pixel).
fn gaussian_blur(plane: &[f32], w: usize, h: usize, sigma: f32) -> Vec<f32> {
    let r = (3.0 * sigma).ceil() as isize;
    let weights: Vec<f32> = (-r..=r)
        .map(|x| (-(x * x) as f32 / (2.0 * sigma * sigma)).exp())
        .collect();
    let total: f32 = weights.iter().sum();
    let weights: Vec<f32> = weights.iter().map(|v| v / total).collect();
    let clamp = |v: isize, n: usize| v.clamp(0, n as isize - 1) as usize;
    let mut across = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0f32;
            for (k, weight) in weights.iter().enumerate() {
                sum += weight * plane[y * w + clamp(x as isize + k as isize - r, w)];
            }
            across[y * w + x] = sum;
        }
    }
    let mut out = vec![0f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0f32;
            for (k, weight) in weights.iter().enumerate() {
                sum += weight * across[clamp(y as isize + k as isize - r, h) * w + x];
            }
            out[y * w + x] = sum;
        }
    }
    out
}

/// Inputs computed once per photo. Not recomputed when the capture sharpening settings change.
pub struct Prepared {
    pub width: usize,
    pub height: usize,
    /// Luminance to sharpen (RawTherapee units, Rec.2020 weights).
    y: Vec<f32>,
    /// L* for contrast measurement (RawTherapee units).
    l: Vec<f32>,
    /// Highlight protection (output area).
    guard: Vec<f32>,
    /// Radius and contrast threshold derived from the photo (used when no value is set).
    pub auto_radius: f32,
    pub auto_threshold: f32,
}

/// Computes the capture sharpening inputs. `cam` is camera RGB cropped to the output area, `wb` the
/// camera white balance, and `sensor` the full-sensor CFA of the same photo.
pub fn prepare(cam: &crate::Image, wb: [f32; 3], sensor: &Sensor) -> Prepared {
    use rayon::prelude::*;
    let (w, h) = (cam.width, cam.height);
    assert_eq!(
        (sensor.crop.2, sensor.crop.3),
        (w, h),
        "sensor output area vs. image size"
    );
    let s = rt_scale(wb);
    let (y, l): (Vec<f32>, Vec<f32>) = (0..w * h)
        .into_par_iter()
        .map(|i| {
            let [r, g, b] = [0, 1, 2].map(|c| cam.planes[c][i] * s[c]);
            let y = 0.2627 * r.max(0.0) + 0.6780 * g.max(0.0) + 0.0593 * b.max(0.0);
            (y, lightness_at(r, g, b))
        })
        .unzip();
    let auto_threshold = auto_threshold(&l, w, h);
    Prepared {
        width: w,
        height: h,
        y,
        l,
        guard: highlight_guard(sensor),
        auto_radius: auto_radius(sensor, wb),
        auto_threshold,
    }
}

/// Capture-sharpened camera RGB and the blend mask used (0..1, for the sharpening area view). When
/// disabled, the image is unchanged and the mask is 0.
pub fn sharpen(
    cam: &crate::Image,
    prepared: &Prepared,
    capture: &Capture,
) -> (crate::Image, Vec<f32>) {
    use rayon::prelude::*;
    let (w, h) = (prepared.width, prepared.height);
    if !capture.enabled {
        return (cam.clone(), vec![0.0; w * h]);
    }
    let radius = capture.radius.unwrap_or(prepared.auto_radius);
    let threshold = capture.threshold.unwrap_or(prepared.auto_threshold);
    let blend = blend_mask(&prepared.l, &prepared.guard, w, h, threshold);
    let y_new = deconvolve(&prepared.y, &blend, w, h, radius, capture);
    // Change luminance only: multiply all three channels by the same ratio.
    let factor: Vec<f32> = y_new
        .par_iter()
        .zip(&prepared.y)
        .map(|(new, old)| new / old.max(1e-5))
        .collect();
    let mut out = cam.clone();
    for plane in &mut out.planes {
        plane.par_iter_mut().zip(&factor).for_each(|(v, f)| *v *= f);
    }
    (out, blend)
}

/// Radius of the 32×32 tile with top-left `tile`. Includes the corner boost: it grows with distance
/// from the image center, reaching `radius + corner` (at most 2.0) in the corners (RawTherapee
/// CaptureDeconvSharpening).
fn tile_sigma(radius: f32, corner: f32, w: usize, h: usize, (ty, tx): (usize, usize)) -> f32 {
    let corner_radius = (radius + corner).min(MAX_SIGMA);
    let corner_distance = ((w as f32 * 0.5).powi(2) + (h as f32 * 0.5).powi(2)).sqrt();
    let factor = (corner_radius - radius) / corner_distance;
    let dy = ty as i64 + 16 - (h / 2) as i64;
    let dx = tx as i64 + 16 - (w / 2) as i64;
    radius + factor * ((dy * dy + dx * dx) as f64).sqrt() as f32
}

/// A kernel flattened into its nonzero coefficients and their offsets (in row-major position).
struct Taps {
    radius: usize,
    taps: Vec<(isize, f32)>,
}

impl Taps {
    fn new(sigma: f32, radius: usize, w: usize) -> Self {
        let n = 2 * radius + 1;
        let kernel = kernel_of(sigma, radius);
        let r = radius as isize;
        let taps = (0..n * n)
            .filter(|&k| kernel[k] != 0.0)
            .map(|k| {
                let (dy, dx) = ((k / n) as isize - r, (k % n) as isize - r);
                (dy * w as isize + dx, kernel[k])
            })
            .collect();
        Self { radius, taps }
    }

    fn convolve(&self, plane: &[f32], i: usize) -> f32 {
        self.taps
            .iter()
            .map(|&(offset, weight)| weight * plane[i.wrapping_add_signed(offset)])
            .sum()
    }
}

/// New luminance with blur undone by Richardson–Lucy iterations and blended by the mask
/// (RawTherapee CaptureDeconvSharpening applied to the whole image). The outer 5–8 pixels of the
/// image and tiles whose mask stays below 0.01 are left unchanged.
fn deconvolve(
    y: &[f32],
    blend: &[f32],
    w: usize,
    h: usize,
    radius: f32,
    capture: &Capture,
) -> Vec<f32> {
    use rayon::prelude::*;
    const TILE: usize = 32;
    const MIN_BLEND: f32 = 0.01;
    let iterations = capture.iterations as usize;
    let fixed = capture.corner == 0.0;
    let border = if fixed && kernel_radius(radius) <= 3 {
        if iterations <= 30 { 5 } else { 7 }
    } else {
        8
    };
    let mut out = y.to_vec();
    if iterations == 0 || w <= 2 * border || h <= 2 * border {
        return out;
    }

    // Choose a kernel per tile. Tiles with a radius below 0.4 (when the corner radius is reduced)
    // are not deconvolved.
    let rows: Vec<usize> = (border..h - border).step_by(TILE).collect();
    let cols: Vec<usize> = (border..w - border).step_by(TILE).collect();
    let mut kernels: Vec<Taps> = Vec::new();
    let mut tile_kernel = Vec::with_capacity(rows.len() * cols.len());
    let mut active = Vec::with_capacity(rows.len() * cols.len());
    for &ty in &rows {
        for &tx in &cols {
            let sigma = if fixed {
                radius
            } else {
                tile_sigma(radius, capture.corner, w, h, (ty, tx))
            };
            let r = if fixed {
                Some(kernel_radius(radius))
            } else if sigma < 0.4 {
                None
            } else if sigma > 1.5 {
                Some(6)
            } else if sigma > 1.15 {
                Some(4)
            } else if sigma > 0.84 {
                Some(3)
            } else {
                Some(2)
            };
            let index = r.map(|r| {
                if fixed && !kernels.is_empty() {
                    0
                } else {
                    kernels.push(Taps::new(sigma, r, w));
                    kernels.len() - 1
                }
            });
            let (y1, x1) = ((ty + TILE).min(h - border), (tx + TILE).min(w - border));
            let busy = (ty..y1).any(|j| {
                blend[j * w + tx..j * w + x1]
                    .iter()
                    .any(|&b| b >= MIN_BLEND)
            });
            tile_kernel.push(index);
            active.push(index.is_some() && busy);
        }
    }
    if !active.contains(&true) {
        return out;
    }
    // Each pixel's tile kernel. The border band uses the nearest tile's kernel.
    let kernel_at = |j: usize, i: usize| -> Option<&Taps> {
        let tr = (j.saturating_sub(border) / TILE).min(rows.len() - 1);
        let tc = (i.saturating_sub(border) / TILE).min(cols.len() - 1);
        tile_kernel[tr * cols.len() + tc].map(|k| &kernels[k])
    };
    let fits = |taps: &Taps, j: usize, i: usize| {
        let r = taps.radius;
        j >= r && j + r < h && i >= r && i + r < w
    };

    // u: deblurred estimate, t: observed / (blurred estimate). Outside the kernel's reach,
    // u = observed and t = 1.
    let mut u = y.to_vec();
    let mut t = vec![1f32; w * h];
    let mut stopped = vec![false; active.len()];
    let region = |k: usize| {
        let (ty, tx) = (rows[k / cols.len()], cols[k % cols.len()]);
        (
            ty,
            (ty + TILE).min(h - border),
            tx,
            (tx + TILE).min(w - border),
        )
    };
    let settle = |out: &mut [f32], u: &[f32], k: usize| {
        let (y0, y1, x0, x1) = region(k);
        for j in y0..y1 {
            for i in x0..x1 {
                let p = j * w + i;
                out[p] = blend[p] * (u[p] - y[p]) + y[p];
            }
        }
    };
    for k in 0..iterations {
        t.par_chunks_mut(w).enumerate().for_each(|(j, row)| {
            for (i, v) in row.iter_mut().enumerate() {
                if let Some(taps) = kernel_at(j, i).filter(|taps| fits(taps, j, i)) {
                    *v = y[j * w + i] / taps.convolve(&u, j * w + i).max(1e-5);
                }
            }
        });
        u.par_chunks_mut(w).enumerate().for_each(|(j, row)| {
            for (i, v) in row.iter_mut().enumerate() {
                if let Some(taps) = kernel_at(j, i).filter(|taps| fits(taps, j, i)) {
                    *v *= taps.convolve(&t, j * w + i);
                }
            }
        });
        // Tiles that overshoot (estimate falls below half the observed value) stop at this
        // iteration's values.
        if capture.auto_stop && k + 1 < iterations {
            for tile in 0..active.len() {
                if !active[tile] || stopped[tile] {
                    continue;
                }
                let (y0, y1, x0, x1) = region(tile);
                let overshoot = (y0..y1).any(|j| {
                    (x0..x1).any(|i| {
                        let p = j * w + i;
                        u[p] < y[p] * blend[p] * 0.5
                    })
                });
                if overshoot {
                    stopped[tile] = true;
                    settle(&mut out, &u, tile);
                }
            }
            if active.iter().zip(&stopped).all(|(&a, &s)| !a || s) {
                return out;
            }
        }
    }
    for tile in 0..active.len() {
        if active[tile] && !stopped[tile] {
            settle(&mut out, &u, tile);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::borrow::Cow;

    fn sensor(cfa: &[f32], width: usize, height: usize) -> Sensor<'_> {
        Sensor {
            cfa: Cow::Borrowed(cfa),
            width,
            height,
            red: (0, 0),
            crop: (0, 0, width, height),
        }
    }

    /// A gray scene with a slightly tilted vertical edge blurred by σ using the error function.
    /// Being gray, the CFA equals the scene values.
    fn slanted_edge(w: usize, h: usize, sigma: f32, dark: f32, bright: f32) -> Vec<f32> {
        let mut out = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = x as f64 - w as f64 / 2.0 - 0.3 - 0.37 * (y as f64 - h as f64 / 2.0);
                let t = 0.5 * (1.0 + erf(d / (sigma as f64 * std::f64::consts::SQRT_2)));
                out[y * w + x] = dark + (bright - dark) * t as f32;
            }
        }
        out
    }

    /// Error function (Abramowitz–Stegun 7.1.26, error 1.5e-7).
    fn erf(x: f64) -> f64 {
        let t = 1.0 / (1.0 + 0.3275911 * x.abs());
        let y = 1.0
            - (((((1.061405429 * t - 1.453152027) * t) + 1.421413741) * t - 0.284496736) * t
                + 0.254829592)
                * t
                * (-x * x).exp();
        if x >= 0.0 { y } else { -y }
    }

    #[test]
    fn the_defaults_match_the_camera_jpeg() {
        // Iterations and radius (auto) are RawTherapee's defaults; the contrast threshold is 0.4,
        // chosen by measurement.
        let capture = Capture::default();
        assert!(capture.enabled && capture.auto_stop && capture.is_default());
        assert_eq!(capture.iterations, 20);
        assert_eq!(
            (capture.radius, capture.threshold, capture.corner),
            (None, Some(0.4), 0.0)
        );
    }

    #[test]
    fn each_kernel_is_normalised_symmetric_and_chosen_by_radius() {
        for (sigma, r) in [(0.5, 1), (0.7, 2), (1.0, 3), (1.3, 4), (1.8, 6)] {
            let (radius, k) = kernel(sigma);
            assert_eq!(radius, r, "σ {sigma}");
            let n = 2 * r + 1;
            assert!((k.iter().sum::<f32>() - 1.0).abs() < 1e-5, "σ {sigma}");
            for y in 0..n {
                for x in 0..n {
                    assert_eq!(k[y * n + x], k[x * n + y]);
                    assert_eq!(k[y * n + x], k[(n - 1 - y) * n + x]);
                }
            }
        }
        // 5×5 has its four corners at 0, and (2, 1) remains (support i² + j² ≤ 6.35).
        let (_, k5) = kernel(0.7);
        assert_eq!(k5[0], 0.0);
        assert!(k5[1] > 0.0);
    }

    #[test]
    fn a_clipped_photosite_protects_its_neighbourhood() {
        let (w, h) = (20, 16);
        let mut cfa = vec![0.3f32; w * h];
        cfa[8 * w + 10] = 0.97;
        let guard = highlight_guard(&sensor(&cfa, w, h));
        assert_eq!(guard[8 * w + 10], 0.0);
        assert_eq!(guard[6 * w + 10], 0.0, "(0, -2)");
        assert_eq!(guard[6 * w + 8], 1.0, "(-2, -2) corner remains");
        assert_eq!(guard[8 * w + 13], 1.0, "3 pixels away");
        assert_eq!(guard.iter().filter(|&&v| v == 0.0).count(), 21);
        // Crop to the output area.
        let cropped = Sensor {
            crop: (4, 3, 10, 8),
            ..sensor(&cfa, w, h)
        };
        let guard = highlight_guard(&cropped);
        assert_eq!(guard.len(), 80);
        assert_eq!(guard[(8 - 3) * 10 + (10 - 4)], 0.0);
    }

    #[test]
    fn the_automatic_radius_follows_the_formula_on_a_single_bright_green() {
        // One green pixel at 0.4 on a 0.1 background: max ratio 4 to diagonal greens
        // → σ = sqrt(1 / ln 4).
        let (w, h) = (32, 24);
        let mut cfa = vec![0.1f32; w * h];
        cfa[10 * w + 11] = 0.4; // In RGGB, (even row, odd column) is green
        let found = auto_radius(&sensor(&cfa, w, h), [1.0; 3]);
        assert!((found - (1.0 / 4f32.ln()).sqrt()).abs() < 1e-4, "{found}");
        // Flat means ratio 1 → capped at 2.0
        let flat = vec![0.1f32; w * h];
        assert_eq!(auto_radius(&sensor(&flat, w, h), [1.0; 3]), 2.0);
        // Large ratios at the edge of a saturated band are not used: RawTherapee checks whether
        // the bright pixel's "other" diagonal green is saturated, which is always true at the band
        // edge.
        let (w, h) = (40, 32);
        let mut band = vec![0.1f32; w * h];
        band[8 * w..16 * w].fill(1.0);
        assert_eq!(auto_radius(&sensor(&band, w, h), [1.0; 3]), 2.0);
        band[24 * w + 25] = 0.4; // One bright green away from the band
        let found = auto_radius(&sensor(&band, w, h), [1.0; 3]);
        assert!((found - (1.0 / 4f32.ln()).sqrt()).abs() < 1e-4, "{found}");
        // A limitation kept from the original: at the corner of a saturated block, the bright
        // pixel's other diagonal neighbors are all outside it, so that ratio (10) is used.
        let mut block = vec![0.1f32; w * h];
        for y in 8..16 {
            block[y * w + 8..y * w + 16].fill(1.0);
        }
        let found = auto_radius(&sensor(&block, w, h), [1.0; 3]);
        assert!((found - (1.0 / 10f32.ln()).sqrt()).abs() < 1e-4, "{found}");
    }

    #[test]
    fn the_automatic_radius_grows_with_the_blur() {
        // RawTherapee's method is an approximation, so the values don't equal σ
        // (2026-11-20 Python simulation: σ 0.6/0.8/1.1/1.5 → 0.72/0.80/0.91/1.05). Only the
        // increasing order and the range are checked.
        let found: Vec<f32> = [0.6f32, 0.8, 1.1, 1.5]
            .iter()
            .map(|&sigma| {
                let cfa = slanted_edge(96, 80, sigma, 0.05, 0.7);
                auto_radius(&sensor(&cfa, 96, 80), [1.0; 3])
            })
            .collect();
        assert!(found.windows(2).all(|p| p[0] < p[1]), "{found:?}");
        assert!(found.iter().all(|r| (0.5..1.3).contains(r)), "{found:?}");
    }
    /// L in RawTherapee units: the left half is noise with a standard deviation of about 80 around
    /// L 10000 (so tile variance/mean is 0.5–1 and the auto threshold is not 0), the right half is
    /// stripes 6 pixels wide.
    fn striped_with_noise(w: usize, h: usize) -> Vec<f32> {
        let mut state = 12345u32;
        let mut uniform = move || {
            state = state.wrapping_mul(1103515245).wrapping_add(12345);
            (state >> 8) as f32 / (1u32 << 24) as f32
        };
        let mut l = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                l[y * w + x] = if x < w / 2 {
                    // Sum of 12 uniform samples − 6 ≈ standard normal
                    let normal: f32 = (0..12).map(|_| uniform()).sum::<f32>() - 6.0;
                    10000.0 + 80.0 * normal
                } else if (x / 6) % 2 == 0 {
                    6000.0
                } else {
                    14000.0
                };
            }
        }
        l
    }

    #[test]
    fn the_sigmoid_is_half_at_the_threshold() {
        assert!((blend_factor(0.2, 0.2) - 0.5).abs() < 1e-6);
        assert!(blend_factor(0.0, 0.2) < 0.001);
        assert!(blend_factor(2.0, 0.2) > 0.99);
    }

    #[test]
    fn lightness_follows_cie_l_in_rawtherapee_units() {
        // White (y = 65535) is L* 100 → 32768, black is 0, and very dark values are on the linear
        // segment.
        let l = lightness(&[[65535.0, 65535.0, 65535.0], [0.0; 3], [100.0; 3]]);
        assert!((l[0] - 32768.0).abs() < 0.5, "{}", l[0]);
        assert_eq!(l[1], 0.0);
        assert!(
            (l[2] - 327.68 * (24389.0 / 27.0) * 100.0 / 65535.0).abs() < 0.01,
            "{}",
            l[2]
        );
    }

    #[test]
    fn edges_are_sharpened_and_flat_noise_is_left_alone() {
        let (w, h) = (240, 160);
        let l = striped_with_noise(w, h);
        let guard = vec![1.0; w * h];
        let threshold = auto_threshold(&l, w, h);
        assert!(threshold > 0.0);
        let mask = blend_mask(&l, &guard, w, h, threshold);
        let mean = |x0: usize, x1: usize| {
            let (mut s, mut n) = (0.0, 0.0);
            for y in 10..h - 10 {
                for x in x0..x1 {
                    s += mask[y * w + x];
                    n += 1.0;
                }
            }
            s / n
        };
        assert!(
            mean(10, w / 2 - 10) < 0.05,
            "flat area {}",
            mean(10, w / 2 - 10)
        );
        assert!(
            mean(w / 2 + 10, w - 10) > 0.5,
            "stripes {}",
            mean(w / 2 + 10, w - 10)
        );
    }

    #[test]
    fn highlights_stay_protected_even_at_threshold_zero() {
        let (w, h) = (64, 48);
        let l = vec![10000.0; w * h];
        let mut guard = vec![1.0; w * h];
        for y in 20..28 {
            guard[y * w + 30..y * w + 38].fill(0.0);
        }
        let mask = blend_mask(&l, &guard, w, h, 0.0);
        assert!(mask[24 * w + 34] < 0.2, "{}", mask[24 * w + 34]);
        assert!(mask[5 * w + 5] > 0.99);
    }

    #[test]
    fn a_photo_without_a_suitable_flat_area_gets_threshold_zero() {
        // If everything is too dark (L < 2000) or too bright (L > 20000), the auto threshold is 0.
        let (w, h) = (200, 160);
        assert_eq!(auto_threshold(&vec![1000.0; w * h], w, h), 0.0);
        assert_eq!(auto_threshold(&vec![25000.0; w * h], w, h), 0.0);
    }
    fn grey_image(w: usize, h: usize, v: f32) -> crate::Image {
        crate::Image {
            width: w,
            height: h,
            planes: [0, 1, 2].map(|_| vec![v; w * h]),
        }
    }

    /// A sensor at 0.3 with no saturation (output area = full sensor).
    fn flat_sensor(w: usize, h: usize) -> Vec<f32> {
        vec![0.3; w * h]
    }

    /// A gray image with a vertical edge blurred by σ (dark side `dark`, bright side `bright`).
    fn edge_image(w: usize, h: usize, sigma: f32, dark: f32, bright: f32) -> crate::Image {
        let mut plane = vec![0f32; w * h];
        for y in 0..h {
            for x in 0..w {
                let d = x as f64 - w as f64 / 2.0 + 0.5;
                let t = 0.5 * (1.0 + erf(d / (sigma as f64 * std::f64::consts::SQRT_2)));
                plane[y * w + x] = dark + (bright - dark) * t as f32;
            }
        }
        crate::Image {
            width: w,
            height: h,
            planes: [plane.clone(), plane.clone(), plane],
        }
    }

    fn blurred_edge(w: usize, h: usize, sigma: f32) -> crate::Image {
        edge_image(w, h, sigma, 0.1, 0.5)
    }

    /// A blurred colored edge with stronger red.
    fn coloured_blurred_edge(w: usize, h: usize) -> crate::Image {
        let mut image = blurred_edge(w, h, 1.0);
        image.planes[0].iter_mut().for_each(|v| *v *= 1.6);
        image.planes[2].iter_mut().for_each(|v| *v *= 0.7);
        image
    }

    /// A scene that actually triggers auto-stop: a nearly sharp edge between a very dark side and a
    /// bright side.
    fn hard_edge_with_slight_blur(w: usize, h: usize) -> crate::Image {
        edge_image(w, h, 0.5, 0.02, 0.8)
    }

    fn luminance_of(image: &crate::Image) -> Vec<f32> {
        (0..image.width * image.height)
            .map(|i| {
                0.2627 * image.planes[0][i]
                    + 0.6780 * image.planes[1][i]
                    + 0.0593 * image.planes[2][i]
            })
            .collect()
    }

    /// Largest difference between neighboring pixels in the green of the middle row.
    fn edge_slope(image: &crate::Image, row: usize) -> f32 {
        let w = image.width;
        (1..w)
            .map(|x| (image.planes[1][row * w + x] - image.planes[1][row * w + x - 1]).abs())
            .fold(0.0, f32::max)
    }

    /// How far below the dark side (0.02) values dip, excluding the outer 8 pixels.
    fn undershoot(image: &crate::Image) -> f32 {
        let (w, h) = (image.width, image.height);
        let mut min = f32::MAX;
        for y in 8..h - 8 {
            for x in 8..w - 8 {
                min = min.min(image.planes[1][y * w + x]);
            }
        }
        0.02 - min
    }

    fn max_abs_diff(a: &crate::Image, b: &crate::Image) -> f32 {
        a.planes
            .iter()
            .zip(&b.planes)
            .flat_map(|(p, q)| p.iter().zip(q))
            .map(|(x, y)| (x - y).abs())
            .fold(0.0, f32::max)
    }

    #[test]
    fn a_flat_photo_and_a_zero_mask_are_left_unchanged() {
        let flat = grey_image(64, 48, 0.3);
        let cfa = flat_sensor(64, 48);
        let prepared = prepare(&flat, [2.0, 1.0, 1.5], &sensor(&cfa, 64, 48));
        let (out, _) = sharpen(&flat, &prepared, &Capture::default());
        assert!(
            max_abs_diff(&out, &flat) < 1e-5,
            "{}",
            max_abs_diff(&out, &flat)
        );

        // When disabled, the image is unchanged and the mask to display is 0.
        let edge = blurred_edge(64, 48, 1.0);
        let prepared = prepare(&edge, [1.0; 3], &sensor(&cfa, 64, 48));
        let (out, mask) = sharpen(
            &edge,
            &prepared,
            &Capture {
                enabled: false,
                ..Capture::default()
            },
        );
        assert!(mask.iter().all(|&m| m == 0.0));
        assert_eq!(max_abs_diff(&out, &edge), 0.0);

        // With a zero blend mask, luminance is unchanged too.
        let y = luminance_of(&edge);
        let out = deconvolve(&y, &vec![0.0; 64 * 48], 64, 48, 1.0, &Capture::default());
        assert_eq!(out, y);
    }

    #[test]
    fn a_blurred_edge_gets_steeper_with_more_iterations() {
        let edge = blurred_edge(96, 64, 1.0);
        let cfa = flat_sensor(96, 64);
        let prepared = prepare(&edge, [1.0; 3], &sensor(&cfa, 96, 64));
        let mut last = edge_slope(&edge, 32);
        for iterations in [5, 20, 60] {
            let capture = Capture {
                iterations,
                radius: Some(1.0),
                threshold: Some(0.0),
                auto_stop: false,
                ..Capture::default()
            };
            let (out, _) = sharpen(&edge, &prepared, &capture);
            let now = edge_slope(&out, 32);
            assert!(now > last * 1.05, "{iterations}: {now} <= {last}");
            last = now;
        }
    }

    #[test]
    fn only_luminance_changes() {
        let edge = coloured_blurred_edge(64, 48);
        let cfa = flat_sensor(64, 48);
        let prepared = prepare(&edge, [2.0, 1.0, 1.5], &sensor(&cfa, 64, 48));
        let (out, _) = sharpen(
            &edge,
            &prepared,
            &Capture {
                threshold: Some(0.0),
                ..Capture::default()
            },
        );
        assert!(max_abs_diff(&out, &edge) > 1e-3, "must sharpen at all");
        for i in (0..64 * 48).step_by(17) {
            let before = edge.planes[0][i] / edge.planes[1][i];
            let after = out.planes[0][i] / out.planes[1][i];
            assert!(
                (before - after).abs() < 1e-4 * before.max(1.0),
                "{i}: {before} {after}"
            );
        }
    }

    #[test]
    fn the_outer_border_is_not_sharpened() {
        let edge = blurred_edge(64, 48, 1.0);
        let cfa = flat_sensor(64, 48);
        let prepared = prepare(&edge, [1.0; 3], &sensor(&cfa, 64, 48));
        let (out, _) = sharpen(
            &edge,
            &prepared,
            &Capture {
                threshold: Some(0.0),
                radius: Some(1.3),
                ..Capture::default()
            },
        );
        for x in 0..64 {
            for y in [0, 7, 40, 47] {
                assert_eq!(
                    out.planes[1][y * 64 + x],
                    edge.planes[1][y * 64 + x],
                    "({x}, {y})"
                );
            }
        }
        assert!(max_abs_diff(&out, &edge) > 1e-3);
    }

    #[test]
    fn the_corner_boost_grows_the_radius_towards_the_corners() {
        let sigma = |corner: f32, tile: (usize, usize)| tile_sigma(1.0, corner, 640, 480, tile);
        assert_eq!(sigma(0.0, (0, 0)), 1.0);
        assert!(
            sigma(0.5, (0, 0)) > sigma(0.5, (224, 304)),
            "corner > center"
        );
        assert!((sigma(0.5, (224, 304)) - 1.0).abs() < 1e-6);
        assert!(sigma(-0.5, (0, 0)) < 1.0);
        // With the corner boost, the corner side changes more.
        let edge = blurred_edge(160, 128, 1.0);
        let cfa = flat_sensor(160, 128);
        let prepared = prepare(&edge, [1.0; 3], &sensor(&cfa, 160, 128));
        let plain = Capture {
            threshold: Some(0.0),
            radius: Some(0.8),
            auto_stop: false,
            ..Capture::default()
        };
        let boosted = Capture {
            corner: 0.5,
            ..plain
        };
        let (a, _) = sharpen(&edge, &prepared, &plain);
        let (b, _) = sharpen(&edge, &prepared, &boosted);
        assert!(
            edge_slope(&b, 12) > edge_slope(&a, 12) * 1.02,
            "corner-side row"
        );
    }

    #[test]
    fn the_automatic_stop_keeps_a_ringing_tile_from_running_on() {
        let edge = hard_edge_with_slight_blur(96, 64);
        let cfa = flat_sensor(96, 64);
        let prepared = prepare(&edge, [1.0; 3], &sensor(&cfa, 96, 64));
        let free = Capture {
            iterations: 100,
            radius: Some(1.5),
            threshold: Some(0.0),
            auto_stop: false,
            ..Capture::default()
        };
        let stopped = Capture {
            auto_stop: true,
            ..free
        };
        let (a, _) = sharpen(&edge, &prepared, &free);
        let (b, _) = sharpen(&edge, &prepared, &stopped);
        assert!(
            undershoot(&b) < undershoot(&a),
            "{} {}",
            undershoot(&b),
            undershoot(&a)
        );
    }
}
