//! 캡처 샤픈: 렌즈·로우패스 필터·회절로 흐려진 것을 RAW 단계에서 되돌린다.
//!
//! RawTherapee의 Capture Sharpening(Ingo Weyrich, GPL-3.0-or-later)을 옮긴 것이다.
//! 기준 판: `rtengine/capturesharpening.cc`·`boxblur.cc` 커밋 c6d04960e14f,
//! `rtengine/rt_algo.cc` 커밋 1c2b3f33772e (<https://github.com/RawTherapee/RawTherapee>).
//! 밝기에만 Richardson–Lucy 디컨볼루션을 하고, 경계가 뚜렷한 곳만 섞는다.
//!
//! RawTherapee와 다른 점:
//! - 32×32 구역으로 나눠 따로 계산하지 않고 그림 전체에 반복한다 (구역 분할은 속도를 위한 것이다).
//!   자동 멈춤 판단만 구역마다 한다.
//! - 대비 문턱이 0이어도 하이라이트 보호를 유지한다 (RawTherapee는 이때 보호 마스크를 버린다).
//! - 끝 구역을 두 번 섞지 않는다. 모서리 반경 키우기에서 앞 구역의 값을 쓰지 않는다.
//! - 샤픈 전·후의 노이즈 제거와 X-Trans 센서는 옮기지 않았다.

use crate::raw::Sensor;
use serde::{Deserialize, Serialize};
use std::ops::RangeInclusive;

/// 사진마다 저장하는 캡처 샤픈 값 (프로젝트 파일의 `capture`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Capture {
    /// 캡처 샤픈을 할지.
    pub enabled: bool,
    /// 반복 횟수. 많을수록 흐림을 더 되돌린다 (세기 역할).
    pub iterations: u32,
    /// 되돌릴 흐림의 반경(σ, 화소). 없으면 사진에서 구한다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub radius: Option<f32>,
    /// 대비 문턱. 높을수록 뚜렷한 경계만 샤픈한다. 없으면 사진에서 구한다.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub threshold: Option<f32>,
    /// 모서리로 갈수록 반경에 더하는 값.
    pub corner: f32,
    /// 넘침이 생기면 그 구역의 반복을 멈춘다.
    pub auto_stop: bool,
}

impl Default for Capture {
    fn default() -> Self {
        Self {
            enabled: true,
            iterations: 20,
            radius: None,
            threshold: None,
            corner: 0.0,
            auto_stop: true,
        }
    }
}

impl Capture {
    pub fn is_default(&self) -> bool {
        *self == Self::default()
    }

    /// 캡처 샤픈을 하지 않는 값 (전후 비교의 보정 전 그림 등).
    pub fn off() -> Self {
        Self {
            enabled: false,
            ..Self::default()
        }
    }
}

/// 고를 수 있는 반복 횟수.
pub const ITERATIONS: RangeInclusive<u32> = 1..=100;
/// 고를 수 있는 반경.
pub const RADIUS: RangeInclusive<f32> = 0.4..=2.0;
/// 고를 수 있는 대비 문턱.
pub const THRESHOLD: RangeInclusive<f32> = 0.0..=2.0;
/// 고를 수 있는 모서리 반경.
pub const CORNER: RangeInclusive<f32> = -0.5..=0.5;

/// 자동 반경의 상한 (RawTherapee의 `maxSigma`).
const MAX_SIGMA: f32 = 2.0;
/// 이 비율 이상인 센서 값은 포화한 것으로 보고 둘레를 보호한다 (RawTherapee의 `clipLimit`).
const CLIP_LIMIT: f32 = 0.95;

/// 반경 σ에 맞는 커널의 반지름. 모서리 키우기가 없을 때의 선택이다 (RawTherapee CaptureDeconvSharpening).
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

/// 반지름 `r`인 가우스 커널 ((2r+1)² 계수, 줄 우선). RawTherapee `compute{3,5,7,9,13}x…kernel2`와 같은 계산이다:
/// 지지 영역 밖은 0, `exp`는 double, 합은 float.
fn kernel_of(sigma: f32, r: usize) -> Vec<f32> {
    // 지지 영역: i² + j² ≤ limit. 3×3은 제한이 없다.
    let limit = match r {
        1 => f64::INFINITY,
        2 => (3.0f64 * 0.84).powi(2),
        3 => (3.0f64 * 1.15).powi(2),
        4 => (3.0f64 * 1.5).powi(2),
        6 => (3.0f64 * 2.0).powi(2),
        _ => unreachable!("커널 반지름 {r}"),
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

/// 반경 σ에 맞는 커널: (반지름, 계수).
#[cfg(test)]
fn kernel(sigma: f32) -> (usize, Vec<f32>) {
    let r = kernel_radius(sigma);
    (r, kernel_of(sigma, r))
}

/// 하이라이트 보호: 센서 값이 포화(0.95 이상)인 화소 둘레 21화소(5×5에서 네 귀퉁이를 뺀 것)를 0으로,
/// 나머지를 1로 한 마스크. 센서 전체에서 만든 뒤 출력 영역으로 자른다 (RawTherapee `buildClipMaskBayer`).
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

/// 카메라 RGB를 RawTherapee의 단위로 바꾸는 채널별 배율: 화이트밸런스를 곱하고, 배율이 가장 큰 채널의
/// 포화가 65535가 되게 한다 (RawTherapee `scale_mul`).
fn rt_scale(wb: [f32; 3]) -> [f32; 3] {
    let k = 65535.0 / wb.iter().copied().fold(f32::MIN, f32::max);
    wb.map(|c| c * k)
}

/// 자동 반경: 대각으로 이웃한 초록 화소끼리의 최대 비율 R에서 σ = sqrt(1 / ln R)
/// (RawTherapee `calcRadiusBayer`). 상한은 2.0.
fn auto_radius(sensor: &Sensor, wb: [f32; 3]) -> f32 {
    let (w, h) = (sensor.width, sensor.height);
    let green = rt_scale(wb)[1];
    let raw = |row: usize, col: usize| sensor.cfa[row * w + col] * green;
    let (lower, upper) = (1000f32, green);
    let mut max_ratio = 1f32;
    for row in 4..h.saturating_sub(4) {
        // 초록 화소가 있는 열부터: 이 행이 초록으로 시작하면 짝수 열(6부터), 아니면 홀수 열(5부터).
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
                    // 이웃의 포화한 초록이 끼어든 것인지 본다.
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

/// 대비 측정에 쓰는 CIE L*, RawTherapee 단위 (0..32768 = L* × 327.68). 입력은 RawTherapee 단위의 RGB
/// (`Color::RGB2L`, 밝기 y는 BT.709 가중치).
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

/// 대비를 섞는 비율로 바꾸는 시그모이드. 문턱에서 0.5 (RawTherapee `calcBlendFactor`).
fn blend_factor(contrast: f32, threshold: f32) -> f32 {
    let x = -16.0 + (16.0 / threshold) * contrast;
    0.5 * (1.0 + x / (1.0 + x * x).sqrt())
}

/// (j, i) 화소의 국소 대비: 1·2칸 떨어진 위아래·좌우 차이 (RawTherapee `buildBlendMask`의 대비 계산).
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

/// 구역의 분산을 평균으로 나눈 값 (RawTherapee `tileVariance`).
fn tile_variance(l: &[f32], w: usize, y0: usize, x0: usize, size: usize, average: f32) -> f32 {
    let mut sum = 0f32;
    for y in y0..y0 + size {
        for v in &l[y * w + x0..y * w + x0 + size] {
            sum += (v - average) * (v - average);
        }
    }
    sum / ((size * size) as f32 * average)
}

/// 구역의 분산/평균. 너무 어둡거나 밝거나, 너무 고르면(노이즈조차 없으면) 쓰지 않는다 (무한대).
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

/// 평평한 구역에서, 섞는 비율의 평균이 1% 이하가 되는 가장 작은 문턱 + 0.01 (RawTherapee `calcContrastThreshold`).
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

/// 자동 대비 문턱: 사진에서 가장 평평한(노이즈만 있는) 구역을 찾아, 그 구역이 거의 샤픈되지 않는 문턱을
/// 구한다. 알맞은 구역이 없으면 0 (RawTherapee `buildBlendMask`의 autoContrast).
fn auto_threshold(l: &[f32], w: usize, h: usize) -> f32 {
    // 1단계: 80×80 구역을 겹치지 않게 훑는다. 분산/평균이 1 이하인 곳이 있으면 거기서 끝낸다.
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
    // 2단계: 40×40 구역을 10화소씩 옮기며 훑고, 가장 나은 곳 둘레 ±10화소를 1화소씩 다시 훑는다.
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

/// 섞을 때 쓰는 마스크 (0..1): 경계가 뚜렷한 곳은 1, 평평한 곳은 0에 가깝고, 하이라이트 보호를 곱한 뒤
/// σ 2로 흐린다 (RawTherapee `buildBlendMask`). 문턱이 0이면 대비와 상관없이 1이지만, RawTherapee와 달리
/// 하이라이트 보호는 그대로 곱한다.
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
    // 가장자리 2화소는 안쪽 값을 그대로 쓴다.
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

/// 분리형 가우스 흐림 (반경 3σ, 가장자리는 끝 화소를 되풀이).
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

/// 사진에서 한 번 구해 두는 재료. 캡처 샤픈 값을 바꿔도 다시 구하지 않는다.
pub struct Prepared {
    pub width: usize,
    pub height: usize,
    /// 샤픈할 밝기 (RawTherapee 단위, Rec.2020 가중치).
    y: Vec<f32>,
    /// 대비 측정용 L* (RawTherapee 단위).
    l: Vec<f32>,
    /// 하이라이트 보호 (출력 영역).
    guard: Vec<f32>,
    /// 사진에서 구한 반경과 대비 문턱 (값을 정하지 않았을 때 쓴다).
    pub auto_radius: f32,
    pub auto_threshold: f32,
}

/// 캡처 샤픈의 재료를 구한다. `cam`은 출력 영역으로 잘린 카메라 RGB, `wb`는 카메라 화이트밸런스,
/// `sensor`는 같은 사진의 센서 전체 CFA다.
pub fn prepare(cam: &crate::Image, wb: [f32; 3], sensor: &Sensor) -> Prepared {
    use rayon::prelude::*;
    let (w, h) = (cam.width, cam.height);
    assert_eq!(
        (sensor.crop.2, sensor.crop.3),
        (w, h),
        "센서의 출력 영역과 그림의 크기"
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

/// 캡처 샤픈한 카메라 RGB와, 섞을 때 쓴 마스크(0..1, 샤픈 영역 보기용). 꺼져 있으면 그대로이고 마스크는 0이다.
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
    // 밝기만 바꾼다: 세 채널에 같은 비율을 곱한다.
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

/// 32×32 구역(왼쪽 위 `tile`)의 반경. 모서리 반경 키우기를 반영한다: 그림 가운데에서 멀수록 커져
/// 귀퉁이에서 `radius + corner`(최대 2.0)가 된다 (RawTherapee CaptureDeconvSharpening).
fn tile_sigma(radius: f32, corner: f32, w: usize, h: usize, (ty, tx): (usize, usize)) -> f32 {
    let corner_radius = (radius + corner).min(MAX_SIGMA);
    let corner_distance = ((w as f32 * 0.5).powi(2) + (h as f32 * 0.5).powi(2)).sqrt();
    let factor = (corner_radius - radius) / corner_distance;
    let dy = ty as i64 + 16 - (h / 2) as i64;
    let dx = tx as i64 + 16 - (w / 2) as i64;
    radius + factor * ((dy * dy + dx * dx) as f64).sqrt() as f32
}

/// 커널을 0이 아닌 계수와 그 자리(줄 우선 위치의 차이)로 편 것.
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

/// Richardson–Lucy 반복으로 밝기의 흐림을 되돌리고 마스크만큼 섞은 새 밝기 (RawTherapee
/// CaptureDeconvSharpening을 그림 전체로). 그림의 바깥 5~8화소와 마스크가 0.01에 못 미치는 구역은 그대로다.
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

    // 구역마다 커널을 정한다. 반경이 0.4보다 작은 구역(모서리 반경을 줄였을 때)은 되돌리지 않는다.
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
    // 화소마다 그 구역의 커널. 가장자리 띠는 가장 가까운 구역의 것을 쓴다.
    let kernel_at = |j: usize, i: usize| -> Option<&Taps> {
        let tr = (j.saturating_sub(border) / TILE).min(rows.len() - 1);
        let tc = (i.saturating_sub(border) / TILE).min(cols.len() - 1);
        tile_kernel[tr * cols.len() + tc].map(|k| &kernels[k])
    };
    let fits = |taps: &Taps, j: usize, i: usize| {
        let r = taps.radius;
        j >= r && j + r < h && i >= r && i + r < w
    };

    // u: 흐림을 되돌린 추정, t: 관측 / (추정을 흐린 것). 커널이 닿지 않는 바깥은 u = 관측, t = 1로 둔다.
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
        // 넘침(추정이 관측의 절반 아래로 떨어짐)이 생긴 구역은 이 반복의 값으로 멈춘다.
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

    /// 세로에서 약간 기운 경계를 오차함수로 σ만큼 흐린 회색 장면. 회색이라 CFA는 장면 값 그대로다.
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

    /// 오차함수 (Abramowitz–Stegun 7.1.26, 오차 1.5e-7).
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
    fn the_defaults_are_rawtherapees() {
        let capture = Capture::default();
        assert!(capture.enabled && capture.auto_stop && capture.is_default());
        assert_eq!(capture.iterations, 20);
        assert_eq!(
            (capture.radius, capture.threshold, capture.corner),
            (None, None, 0.0)
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
        // 5×5는 네 귀퉁이가 0이고, (2, 1)은 남는다 (지지 영역 i² + j² ≤ 6.35).
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
        assert_eq!(guard[6 * w + 8], 1.0, "(-2, -2) 귀퉁이는 남는다");
        assert_eq!(guard[8 * w + 13], 1.0, "3칸 떨어진 곳");
        assert_eq!(guard.iter().filter(|&&v| v == 0.0).count(), 21);
        // 출력 영역으로 자른다.
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
        // 배경 0.1 위의 초록 화소 하나가 0.4: 대각 초록과의 최대 비율 4 → σ = sqrt(1 / ln 4).
        let (w, h) = (32, 24);
        let mut cfa = vec![0.1f32; w * h];
        cfa[10 * w + 11] = 0.4; // RGGB에서 (짝수 행, 홀수 열)은 초록
        let found = auto_radius(&sensor(&cfa, w, h), [1.0; 3]);
        assert!((found - (1.0 / 4f32.ln()).sqrt()).abs() < 1e-4, "{found}");
        // 평평하면 비율이 1 → 상한 2.0
        let flat = vec![0.1f32; w * h];
        assert_eq!(auto_radius(&sensor(&flat, w, h), [1.0; 3]), 2.0);
        // 포화한 띠의 경계에서 생기는 큰 비율은 쓰지 않는다: RawTherapee는 밝은 쪽 화소의
        // "다른" 대각 초록이 포화했는지 보는데, 띠의 경계에서는 늘 그렇다.
        let (w, h) = (40, 32);
        let mut band = vec![0.1f32; w * h];
        band[8 * w..16 * w].fill(1.0);
        assert_eq!(auto_radius(&sensor(&band, w, h), [1.0; 3]), 2.0);
        band[24 * w + 25] = 0.4; // 띠에서 떨어진 곳의 밝은 초록 하나
        let found = auto_radius(&sensor(&band, w, h), [1.0; 3]);
        assert!((found - (1.0 / 4f32.ln()).sqrt()).abs() < 1e-4, "{found}");
        // 원본 그대로의 한계: 포화한 덩어리의 귀퉁이에서는 밝은 화소의 다른 대각 이웃이 모두 밖이라
        // 그 비율(10)이 쓰인다.
        let mut block = vec![0.1f32; w * h];
        for y in 8..16 {
            block[y * w + 8..y * w + 16].fill(1.0);
        }
        let found = auto_radius(&sensor(&block, w, h), [1.0; 3]);
        assert!((found - (1.0 / 10f32.ln()).sqrt()).abs() < 1e-4, "{found}");
    }

    #[test]
    fn the_automatic_radius_grows_with_the_blur() {
        // RawTherapee의 방식은 근사라 값이 σ와 같지는 않다
        // (2026-11-20 파이썬 모의 실험: σ 0.6/0.8/1.1/1.5 → 0.72/0.80/0.91/1.05). 커지는 순서와 범위만 본다.
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
    /// RawTherapee 단위의 L: 왼쪽 절반은 L 10000 둘레에 표준편차 약 80의 노이즈
    /// (구역의 분산/평균이 0.5~1이 되어 자동 문턱이 0이 되지 않게), 오른쪽 절반은 폭 6화소의 줄무늬.
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
                    // 균등 분포 12개의 합 − 6 ≈ 표준 정규 분포
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
        // 흰색(y = 65535)은 L* 100 → 32768, 검정은 0, 아주 어두운 곳은 직선 구간.
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
            "평평한 곳 {}",
            mean(10, w / 2 - 10)
        );
        assert!(
            mean(w / 2 + 10, w - 10) > 0.5,
            "줄무늬 {}",
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
        // 너무 어둡거나(L < 2000) 너무 밝은(L > 20000) 곳뿐이면 자동 문턱은 0이다.
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

    /// 포화가 없는 0.3짜리 센서 (출력 영역 = 센서 전체).
    fn flat_sensor(w: usize, h: usize) -> Vec<f32> {
        vec![0.3; w * h]
    }

    /// 세로 경계를 σ만큼 흐린 회색 그림 (어두운 쪽 `dark`, 밝은 쪽 `bright`).
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

    /// 빨강이 더 강한 색 있는 흐린 경계.
    fn coloured_blurred_edge(w: usize, h: usize) -> crate::Image {
        let mut image = blurred_edge(w, h, 1.0);
        image.planes[0].iter_mut().for_each(|v| *v *= 1.6);
        image.planes[2].iter_mut().for_each(|v| *v *= 0.7);
        image
    }

    /// 자동 멈춤이 실제로 걸리는 장면: 아주 어두운 쪽과 밝은 쪽 사이의 거의 날카로운 경계.
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

    /// 가운데 줄의 초록에서 이웃 화소 사이의 가장 큰 차이.
    fn edge_slope(image: &crate::Image, row: usize) -> f32 {
        let w = image.width;
        (1..w)
            .map(|x| (image.planes[1][row * w + x] - image.planes[1][row * w + x - 1]).abs())
            .fold(0.0, f32::max)
    }

    /// 가장자리 8화소를 뺀 안쪽에서 어두운 쪽(0.02)보다 얼마나 더 내려갔는가.
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

        // 꺼져 있으면 그대로이고, 보여 줄 마스크는 0이다.
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

        // 섞는 마스크가 0이면 밝기도 그대로다.
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
        assert!(max_abs_diff(&out, &edge) > 1e-3, "샤픈이 되기는 해야 한다");
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
            "귀퉁이 > 가운데"
        );
        assert!((sigma(0.5, (224, 304)) - 1.0).abs() < 1e-6);
        assert!(sigma(-0.5, (0, 0)) < 1.0);
        // 모서리 키우기를 쓰면 귀퉁이 쪽이 더 많이 바뀐다.
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
            "귀퉁이 쪽 줄"
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
