//! AI 노이즈 제거(RawNIND)의 전처리와 후처리: Bayer 묶기, 타일 나누기와 이어붙이기, 밝기 맞추기.
//!
//! 규칙은 darktable-ai의 `rawdenoise-nind` 데모와 같다. 모델 실행 자체는 `infer` 함수로 받는다.

use crate::{Image, Result};
use rayon::prelude::*;

/// 모델 입력 타일의 한 변 (화소, 반해상도 기준). 모델에 고정된 값이다.
pub const TILE: usize = 512;
/// 타일 가장자리에서 버리는 폭. 이웃 타일과 겹치는 부분이라 이음매가 생기지 않는다.
const OVERLAP: usize = 64;
/// 모델은 입력의 2배 해상도로 출력한다 (디모자익 포함).
const SCALE: usize = 2;

/// [R, G1, G2, B] 순서의 반해상도 4채널.
pub struct Packed {
    pub width: usize,
    pub height: usize,
    pub planes: [Vec<f32>; 4],
}

/// Bayer 모자이크를 4채널로 묶는다. `(ox, oy)`는 모자이크에서 첫 R 화소의 위치(0 또는 1)다.
pub fn pack_rggb(mosaic: &[f32], width: usize, height: usize, ox: usize, oy: usize) -> Packed {
    let (w, h) = ((width - ox) / 2, (height - oy) / 2);
    let plane = |dx: usize, dy: usize| {
        let mut out = Vec::with_capacity(w * h);
        for y in 0..h {
            let start = (oy + 2 * y + dy) * width + ox + dx;
            out.extend((0..w).map(|x| mosaic[start + 2 * x]));
        }
        out
    };
    Packed {
        width: w,
        height: h,
        planes: [plane(0, 0), plane(1, 0), plane(0, 1), plane(1, 1)],
    }
}

/// 묶은 Bayer 데이터에 노이즈 제거 모델을 돌려 센서 전체 크기의 카메라 RGB를 만든다.
///
/// `infer`는 4×512×512 입력(채널 우선)을 받아 3×1024×1024 출력을 돌려준다.
pub fn run(packed: &Packed, infer: impl FnMut(&[f32]) -> Result<Vec<f32>>) -> Result<Image> {
    let mut out = run_tiled(packed, infer)?;
    // 모델 출력의 배율은 임의라서 평균을 센서 데이터에 맞춘다. darktable-ai 데모는 4채널 평균에 맞추지만
    // 그러면 G가 두 번 세어져 실제 RGB보다 밝아진다. 여기서는 RGB 평균(G는 두 채널의 평균)에 맞춘다.
    let mean = |c: usize| {
        packed.planes[c].par_iter().map(|&v| v as f64).sum::<f64>() / packed.planes[c].len() as f64
    };
    let rgb_mean = (mean(0) + (mean(1) + mean(2)) / 2.0 + mean(3)) / 3.0;
    match_gain(&mut out, rgb_mean as f32);
    Ok(out)
}

/// `run`이 모델을 부르는 횟수.
pub fn tile_count(packed: &Packed) -> usize {
    let step = TILE - 2 * OVERLAP;
    packed.width.div_ceil(step) * packed.height.div_ceil(step)
}

/// 타일마다 `infer`를 불러 2배 해상도의 3채널 이미지를 만든다.
///
/// `infer`는 4×512×512 입력(채널 우선)을 받아 3×1024×1024 출력을 돌려준다.
pub fn run_tiled(
    packed: &Packed,
    mut infer: impl FnMut(&[f32]) -> Result<Vec<f32>>,
) -> Result<Image> {
    let (w, h) = (packed.width, packed.height);
    // 각 타일은 가운데 step×step만 결과에 쓰고, 둘레 OVERLAP은 문맥으로만 쓴다.
    let step = TILE - 2 * OVERLAP;
    let (out_w, out_side) = (w * SCALE, TILE * SCALE);
    let mut planes = [0, 1, 2].map(|_| vec![0f32; out_w * h * SCALE]);
    let mut tile = vec![0f32; 4 * TILE * TILE];

    for core_y in (0..h).step_by(step) {
        let core_h = step.min(h - core_y);
        let ys: Vec<usize> = (0..TILE)
            .map(|i| mirror((core_y + i) as isize - OVERLAP as isize, h))
            .collect();
        for core_x in (0..w).step_by(step) {
            let core_w = step.min(w - core_x);
            let xs: Vec<usize> = (0..TILE)
                .map(|i| mirror((core_x + i) as isize - OVERLAP as isize, w))
                .collect();

            for (c, plane) in packed.planes.iter().enumerate() {
                for (ty, &sy) in ys.iter().enumerate() {
                    let src = &plane[sy * w..][..w];
                    let dst = &mut tile[(c * TILE + ty) * TILE..][..TILE];
                    dst.iter_mut().zip(&xs).for_each(|(d, &sx)| *d = src[sx]);
                }
            }

            let out = infer(&tile)?;
            if out.len() != 3 * out_side * out_side {
                return Err(format!("모델 출력 크기가 예상과 다릅니다 ({})", out.len()).into());
            }
            for (c, plane) in planes.iter_mut().enumerate() {
                for y in 0..core_h * SCALE {
                    let src = (c * out_side + OVERLAP * SCALE + y) * out_side + OVERLAP * SCALE;
                    let dst = (core_y * SCALE + y) * out_w + core_x * SCALE;
                    plane[dst..][..core_w * SCALE].copy_from_slice(&out[src..][..core_w * SCALE]);
                }
            }
        }
    }
    Ok(Image {
        width: out_w,
        height: h * SCALE,
        planes,
    })
}

/// 이미지 밖의 좌표를 거울처럼 접어 안쪽 좌표로 바꾼다 (가장자리 화소는 반복하지 않는다).
fn mirror(i: isize, n: usize) -> usize {
    if n == 1 {
        return 0;
    }
    let period = 2 * (n as isize - 1);
    let m = i.rem_euclid(period);
    (if m < n as isize { m } else { period - m }) as usize
}

/// 노이즈 제거 강도. 0이면 `plain` 그대로, 1이면 `denoised` 그대로, 그 사이는 섞는다.
pub fn mix(mut plain: Image, denoised: &Image, strength: f32) -> Image {
    for (a, b) in plain.planes.iter_mut().zip(&denoised.planes) {
        a.par_iter_mut()
            .zip(b)
            .for_each(|(a, &b)| *a += strength * (b - *a));
    }
    plain
}

/// 이미지 전체에 같은 배율을 곱해 평균을 `target_mean`으로 맞춘다. 모델 출력의 배율이 임의이기 때문이다.
pub fn match_gain(img: &mut Image, target_mean: f32) {
    let count = (img.width * img.height * 3) as f64;
    let sum: f64 = img
        .planes
        .par_iter()
        .map(|p| p.par_iter().map(|&v| v as f64).sum::<f64>())
        .sum();
    let mean = sum / count;
    if mean.abs() < 1e-12 {
        return;
    }
    let gain = (target_mean as f64 / mean) as f32;
    img.planes
        .par_iter_mut()
        .for_each(|p| p.par_iter_mut().for_each(|v| *v *= gain));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packs_rggb_mosaic_into_four_half_size_planes() {
        //  0  1  2  3
        //  4  5  6  7
        //  8  9 10 11
        // 12 13 14 15
        let mosaic: Vec<f32> = (0..16).map(|v| v as f32).collect();
        let p = pack_rggb(&mosaic, 4, 4, 0, 0);
        assert_eq!((p.width, p.height), (2, 2));
        assert_eq!(p.planes[0], [0.0, 2.0, 8.0, 10.0]); // R
        assert_eq!(p.planes[1], [1.0, 3.0, 9.0, 11.0]); // G1
        assert_eq!(p.planes[2], [4.0, 6.0, 12.0, 14.0]); // G2
        assert_eq!(p.planes[3], [5.0, 7.0, 13.0, 15.0]); // B
    }

    #[test]
    fn packing_starts_at_the_first_red_pixel() {
        // G R G R G R     R이 (1, 0)에 있는 GRBG 센서. 첫 열을 건너뛰어 RGGB로 맞춘다.
        // B G B G B G
        let mosaic: Vec<f32> = (0..12).map(|v| v as f32).collect();
        let p = pack_rggb(&mosaic, 6, 2, 1, 0);
        assert_eq!((p.width, p.height), (2, 1));
        assert_eq!(p.planes[0], [1.0, 3.0]); // R
        assert_eq!(p.planes[1], [2.0, 4.0]); // G1
        assert_eq!(p.planes[2], [7.0, 9.0]); // G2
        assert_eq!(p.planes[3], [8.0, 10.0]); // B
    }

    /// 화소마다 값이 다른 4채널 이미지.
    fn numbered(width: usize, height: usize) -> Packed {
        let plane = |c: usize| (0..width * height).map(|i| (i * 4 + c) as f32).collect();
        Packed {
            width,
            height,
            planes: [plane(0), plane(1), plane(2), plane(3)],
        }
    }

    /// 가짜 모델: 출력 채널 c의 (x, y)는 입력 채널 c의 (x/2 + dx, y/2 + dy).
    fn shifted_copy(dx: isize, dy: isize) -> impl FnMut(&[f32]) -> Result<Vec<f32>> {
        move |tile| {
            let out_side = TILE * SCALE;
            let mut out = vec![f32::NAN; 3 * out_side * out_side];
            for c in 0..3 {
                for y in 0..out_side {
                    for x in 0..out_side {
                        let (sx, sy) = ((x / 2) as isize + dx, (y / 2) as isize + dy);
                        if (0..TILE as isize).contains(&sx) && (0..TILE as isize).contains(&sy) {
                            out[(c * out_side + y) * out_side + x] =
                                tile[(c * TILE + sy as usize) * TILE + sx as usize];
                        }
                    }
                }
            }
            Ok(out)
        }
    }

    #[test]
    fn stitched_tiles_reproduce_the_whole_image_without_seams() {
        // 타일 여러 개가 필요하고 타일 간격의 배수가 아닌 크기
        let packed = numbered(900, 600);
        let mut calls = 0;
        let mut model = shifted_copy(0, 0);
        let out = run_tiled(&packed, |tile| {
            calls += 1;
            model(tile)
        })
        .unwrap();

        assert_eq!((out.width, out.height), (1800, 1200));
        assert_eq!(calls, 3 * 2);
        assert_eq!(tile_count(&packed), calls);
        for c in 0..3 {
            for y in 0..out.height {
                for x in 0..out.width {
                    let want = packed.planes[c][(y / 2) * packed.width + x / 2];
                    assert_eq!(
                        out.planes[c][y * out.width + x],
                        want,
                        "채널 {c} ({x}, {y})"
                    );
                }
            }
        }
    }

    #[test]
    fn image_borders_are_mirror_padded() {
        let packed = numbered(600, 520);
        // 왼쪽 위 이웃을 읽는 모델: 이미지 밖은 거울처럼 접힌 화소여야 한다.
        let out = run_tiled(&packed, shifted_copy(-1, -1)).unwrap();
        let at = |x: usize, y: usize| out.planes[0][y * out.width + x];
        let src = |x: usize, y: usize| packed.planes[0][y * packed.width + x];
        assert_eq!(at(0, 0), src(1, 1)); // (-1, -1) → (1, 1)
        assert_eq!(at(10, 0), src(4, 1)); // (4, -1) → (4, 1)
        assert_eq!(at(10, 10), src(4, 4)); // 안쪽은 그대로 한 칸 이동

        // 오른쪽 아래 이웃을 읽는 모델
        let out = run_tiled(&packed, shifted_copy(1, 1)).unwrap();
        let at = |x: usize, y: usize| out.planes[0][y * out.width + x];
        assert_eq!(at(1199, 1039), src(598, 518)); // (600, 520) → (598, 518)
    }

    #[test]
    fn output_brightness_follows_the_sensor_data_whatever_the_model_gain() {
        // 화이트밸런스 전의 균일한 색: R 0.1, G 0.2, B 0.15
        let (w, h) = (600, 520);
        let flat = |v: f32| vec![v; w * h];
        let packed = Packed {
            width: w,
            height: h,
            planes: [flat(0.1), flat(0.2), flat(0.2), flat(0.15)],
        };
        // 가짜 모델: 색은 맞지만 배율이 제멋대로 (×1000)
        let model = |tile: &[f32]| {
            let side = TILE * SCALE;
            let mut out = vec![0f32; 3 * side * side];
            for (c, src) in [0, 1, 3].into_iter().enumerate() {
                for y in 0..side {
                    for x in 0..side {
                        out[(c * side + y) * side + x] =
                            1000.0 * tile[(src * TILE + y / 2) * TILE + x / 2];
                    }
                }
            }
            Ok(out)
        };

        let out = run(&packed, model).unwrap();

        assert_eq!((out.width, out.height), (w * 2, h * 2));
        for (c, want) in [0.1, 0.2, 0.15].into_iter().enumerate() {
            let got = out.planes[c][out.width * 500 + 700];
            assert!((got - want).abs() < 1e-4, "채널 {c}: {got}");
        }
    }

    #[test]
    fn strength_blends_between_plain_and_denoised() {
        let image = |v: f32| Image {
            width: 2,
            height: 1,
            planes: [vec![v, v], vec![v, v], vec![v, v]],
        };
        assert_eq!(mix(image(0.2), &image(0.6), 0.0).planes[1], [0.2, 0.2]);
        assert_eq!(mix(image(0.2), &image(0.6), 1.0).planes[1], [0.6, 0.6]);
        let half = mix(image(0.2), &image(0.6), 0.5);
        assert!((half.planes[2][0] - 0.4).abs() < 1e-6);
    }

    #[test]
    fn gain_matching_sets_the_mean() {
        let mut img = Image {
            width: 2,
            height: 1,
            planes: [vec![100.0, 300.0], vec![200.0, 200.0], vec![50.0, 350.0]],
        };
        match_gain(&mut img, 0.25);
        let mean = img.planes.iter().flatten().sum::<f32>() / 6.0;
        assert!((mean - 0.25).abs() < 1e-6, "{mean}");
        assert!(
            (img.planes[0][1] / img.planes[0][0] - 3.0).abs() < 1e-5,
            "비율은 유지된다"
        );
    }

    #[test]
    fn gain_matching_leaves_an_all_zero_image_alone() {
        let mut img = Image {
            width: 1,
            height: 1,
            planes: [vec![0.0], vec![0.0], vec![0.0]],
        };
        match_gain(&mut img, 0.25);
        assert_eq!(img.planes[0][0], 0.0);
    }
}
