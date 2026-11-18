//! 기본 현상: 카메라 RGB → 화이트밸런스 → 색행렬 → 톤 커브 → sRGB.

use crate::Image;
use rayon::prelude::*;

type M3 = [[f32; 3]; 3];

const REC2020_TO_XYZ: M3 = [
    [0.636958, 0.144617, 0.168881],
    [0.262700, 0.677998, 0.059302],
    [0.000000, 0.028073, 1.060985],
];
const REC2020_TO_SRGB: M3 = [
    [1.660491, -0.587641, -0.072850],
    [-0.124550, 1.1329, -0.008349],
    [-0.018151, -0.100579, 1.11873],
];

// ponytail: 고정 톤 커브. A900 카메라 JPG 두 장의 밝기 분포에 맞춘 값이다 (휘도 RMSE 3.6/255).
// 다른 카메라나 취향에 따라 달라질 수 있어서 프리셋 단계(M5)에서 노출·대비 조절이 붙는다.
/// 톤 커브의 대비. 클수록 어두운 곳은 더 어둡고 밝은 곳은 더 밝다.
const CONTRAST: f32 = 1.55;
/// 톤 커브의 밝기. 작을수록 전체가 밝아진다.
const TONE_K: f32 = 0.10;

/// 카메라 RGB(선형, 센서 포화 = 1.0)를 sRGB(감마 적용, 0..1)로 바꾼다.
///
/// `wb`는 채널별 화이트밸런스 배율, `xyz_to_cam`은 D65 기준 XYZ → 카메라 행렬.
pub fn develop(cam: &Image, wb: [f32; 3], xyz_to_cam: M3) -> Image {
    // 배율이 가장 작은 채널을 1로 맞춘다. 그러면 1.0에서 자르는 것만으로 포화 영역이 무채색이 된다.
    let min_wb = wb.iter().copied().fold(f32::INFINITY, f32::min);
    let wb = wb.map(|v| v / min_wb);
    // 행의 합을 1로 맞추면 화이트밸런스를 거친 흰색이 작업 공간의 흰색이 된다 (dcraw 방식).
    let cam_to_rec2020 = invert(&normalize_rows(&mul(&xyz_to_cam, &REC2020_TO_XYZ)));

    let (w, n) = (cam.width, cam.width * cam.height);
    let mut out = [vec![0f32; n], vec![0f32; n], vec![0f32; n]];
    let [o0, o1, o2] = &mut out;
    (
        o0.par_chunks_mut(w),
        o1.par_chunks_mut(w),
        o2.par_chunks_mut(w),
    )
        .into_par_iter()
        .enumerate()
        .for_each(|(y, (r0, r1, r2))| {
            for x in 0..w {
                let i = y * w + x;
                let balanced = [0, 1, 2].map(|c| (cam.planes[c][i] * wb[c]).min(1.0));
                let toned = apply(&cam_to_rec2020, balanced).map(tone);
                let srgb = apply(&REC2020_TO_SRGB, toned).map(srgb_gamma);
                (r0[x], r1[x], r2[x]) = (srgb[0], srgb[1], srgb[2]);
            }
        });
    Image {
        width: cam.width,
        height: cam.height,
        planes: out,
    }
}

/// S자 톤 커브. 0 → 0, 1 → 1.
fn tone(v: f32) -> f32 {
    let p = v.max(0.0).powf(CONTRAST);
    (1.0 + TONE_K) * p / (p + TONE_K)
}

fn srgb_gamma(v: f32) -> f32 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.0031308 {
        12.92 * v
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn apply(m: &M3, v: [f32; 3]) -> [f32; 3] {
    [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
}

fn mul(a: &M3, b: &M3) -> M3 {
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j]))
}

fn normalize_rows(m: &M3) -> M3 {
    m.map(|row| row.map(|v| v / (row[0] + row[1] + row[2])))
}

fn invert(m: &M3) -> M3 {
    let c = |i: usize, j: usize| {
        let (a, b) = ((i + 1) % 3, (i + 2) % 3);
        let (p, q) = ((j + 1) % 3, (j + 2) % 3);
        m[a][p] * m[b][q] - m[a][q] * m[b][p]
    };
    let det = m[0][0] * c(0, 0) + m[0][1] * c(0, 1) + m[0][2] * c(0, 2);
    [0, 1, 2].map(|i| [0, 1, 2].map(|j| c(j, i) / det))
}

/// EXIF 방향대로 픽셀을 돌려 바로 세운다. 뒤집기를 먼저 하고 전치(x, y 교환)를 나중에 한다.
pub fn orient(img: Image, transpose: bool, flip_h: bool, flip_v: bool) -> Image {
    if !(transpose || flip_h || flip_v) {
        return img;
    }
    let (w, h) = (img.width, img.height);
    let (out_w, out_h) = if transpose { (h, w) } else { (w, h) };
    let planes = img.planes.map(|src| {
        let mut dst = vec![0f32; src.len()];
        dst.par_chunks_mut(out_w).enumerate().for_each(|(oy, row)| {
            for (ox, v) in row.iter_mut().enumerate() {
                let (x, y) = if transpose { (oy, ox) } else { (ox, oy) };
                let x = if flip_h { w - 1 - x } else { x };
                let y = if flip_v { h - 1 - y } else { y };
                *v = src[y * w + x];
            }
        });
        dst
    });
    Image {
        width: out_w,
        height: out_h,
        planes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sony A900의 D65 행렬 (rawler의 카메라 데이터).
    const XYZ_TO_CAM: [[f32; 3]; 3] = [
        [0.5209, -0.1072, -0.0397],
        [-0.8845, 1.6121, 0.2919],
        [-0.1618, 0.1802, 0.8654],
    ];
    const SRGB_TO_XYZ: [[f32; 3]; 3] = [
        [0.4124564, 0.3575761, 0.1804375],
        [0.2126729, 0.7151522, 0.0721750],
        [0.0193339, 0.119192, 0.9503041],
    ];

    fn mul(m: &[[f32; 3]; 3], v: [f32; 3]) -> [f32; 3] {
        [0, 1, 2].map(|i| m[i][0] * v[0] + m[i][1] * v[1] + m[i][2] * v[2])
    }

    /// 선형 sRGB 색을 이 카메라가 찍었을 때의 RAW 값.
    fn shoot(srgb: [f32; 3]) -> [f32; 3] {
        mul(&XYZ_TO_CAM, mul(&SRGB_TO_XYZ, srgb))
    }

    /// 흰색을 찍은 RAW 값의 역수 = 이 장면의 화이트밸런스 (G = 1).
    fn wb() -> [f32; 3] {
        let w = shoot([1.0; 3]);
        [w[1] / w[0], 1.0, w[1] / w[2]]
    }

    fn develop_px(cam: [f32; 3]) -> [f32; 3] {
        let img = Image {
            width: 1,
            height: 1,
            planes: cam.map(|v| vec![v]),
        };
        let out = develop(&img, wb(), XYZ_TO_CAM);
        [out.planes[0][0], out.planes[1][0], out.planes[2][0]]
    }

    fn assert_neutral(out: [f32; 3]) {
        assert!(
            (out[0] - out[1]).abs() < 2e-3 && (out[1] - out[2]).abs() < 2e-3,
            "{out:?}"
        );
    }

    #[test]
    fn gray_stays_neutral() {
        for level in [0.02, 0.18, 0.5, 0.9] {
            assert_neutral(develop_px(shoot([level; 3])));
        }
    }

    #[test]
    fn black_is_black_and_brightest_gray_is_white() {
        assert_eq!(develop_px([0.0; 3]), [0.0; 3]);
        // 화이트밸런스 뒤 세 채널이 모두 1.0이 되는 회색
        let w = wb();
        let out = develop_px([1.0 / w[0], 1.0, 1.0 / w[2]]);
        assert!(out.iter().all(|&v| v > 0.995), "{out:?}");
    }

    #[test]
    fn clipped_highlight_has_no_colour_cast() {
        // 세 채널 모두 센서 포화. 화이트밸런스를 곱하면 R, B가 G보다 커지는데 색이 끼면 안 된다.
        let out = develop_px([1.0; 3]);
        assert_neutral(out);
        assert!(out[1] > 0.995, "{out:?}");
    }

    #[test]
    fn brighter_scene_gives_brighter_pixel() {
        let levels = [0.01, 0.05, 0.18, 0.4, 0.7, 0.95];
        let out: Vec<f32> = levels
            .iter()
            .map(|&l| develop_px(shoot([l; 3]))[1])
            .collect();
        assert!(out.windows(2).all(|p| p[0] < p[1]), "{out:?}");
    }

    #[test]
    fn red_object_stays_red() {
        let out = develop_px(shoot([0.3, 0.02, 0.02]));
        assert!(out[0] > 2.0 * out[1] && out[0] > 2.0 * out[2], "{out:?}");
    }

    fn numbered(width: usize, height: usize) -> Image {
        let plane: Vec<f32> = (0..width * height).map(|i| i as f32).collect();
        Image {
            width,
            height,
            planes: [plane.clone(), plane.clone(), plane],
        }
    }

    #[test]
    fn exif_rotate90_turns_image_clockwise() {
        // 0 1 2        3 0
        // 3 4 5   →    4 1
        //              5 2
        let out = orient(numbered(3, 2), true, false, true);
        assert_eq!((out.width, out.height), (2, 3));
        assert_eq!(out.planes[0], [3.0, 0.0, 4.0, 1.0, 5.0, 2.0]);
        assert_eq!(out.planes[2], out.planes[0]);
    }

    #[test]
    fn exif_rotate180_reverses_pixels() {
        let out = orient(numbered(3, 2), false, true, true);
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(out.planes[1], [5.0, 4.0, 3.0, 2.0, 1.0, 0.0]);
    }

    #[test]
    fn normal_orientation_is_untouched() {
        let out = orient(numbered(3, 2), false, false, false);
        assert_eq!(out.planes[0], [0.0, 1.0, 2.0, 3.0, 4.0, 5.0]);
    }
}
