//! Basic development: camera RGB → white balance → color matrix → tone curve → sRGB.

use crate::Image;
use crate::edit::{self, Params};
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

// ponytail: fixed tone curve. Fitted to the brightness distribution of two A900 camera JPGs
// (luminance RMSE 3.6/255). Since it may differ by camera or taste, exposure and contrast controls
// are added in the preset stage (M5).
/// Tone curve contrast. Higher makes shadows darker and highlights brighter.
const CONTRAST: f32 = 1.55;
/// Tone curve brightness. Lower makes the whole image brighter.
const TONE_K: f32 = 0.10;

/// How far red and blue move in opposite directions at the end of the temperature slider (stops).
const TEMP_STOPS: f32 = 0.5;
/// How far green moves at the end of the tint slider (stops).
const TINT_STOPS: f32 = 0.3;
/// Radius used by sharpen and clarity (relative to the long side). Kept as a ratio so the look is
/// the same at any resolution.
const SHARPEN_RADIUS: f32 = 0.0004;
const CLARITY_RADIUS: f32 = 0.012;
const SHARPEN_GAIN: f32 = 1.0;
const CLARITY_GAIN: f32 = 0.6;
/// Relative brightness change at the corners at the end of the vignette slider.
const VIGNETTE_GAIN: f32 = 0.5;

/// Converts camera RGB (linear, sensor saturation = 1.0) to sRGB (gamma-encoded, 0..1).
///
/// `wb` is the per-channel white balance multipliers, `xyz_to_cam` the D65 XYZ → camera matrix, and
/// `params` the adjustments.
pub fn develop(cam: &Image, wb: [f32; 3], xyz_to_cam: M3, params: &Params) -> Image {
    // Temperature and tint are folded into the white balance multipliers.
    let wb = [
        wb[0] * 2f32.powf(params.temp * TEMP_STOPS),
        wb[1] * 2f32.powf(-params.tint * TINT_STOPS),
        wb[2] * 2f32.powf(-params.temp * TEMP_STOPS),
    ];
    // Scale so the smallest multiplier is 1. Then clipping at 1.0 alone makes saturated areas
    // neutral.
    let min_wb = wb.iter().copied().fold(f32::INFINITY, f32::min);
    let wb = wb.map(|v| v / min_wb);
    // Normalizing rows to sum to 1 makes white-balanced white the working-space white (as dcraw
    // does).
    let cam_to_rec2020 = invert(&normalize_rows(&mul(&xyz_to_cam, &REC2020_TO_XYZ)));
    let gain = 2f32.powf(params.exposure);
    let spatial = params.clarity != 0.0 || params.sharpen != 0.0 || params.vignette != 0.0;
    // With color adjustments or spatial filters, go through Oklab. Otherwise go straight to sRGB
    // (faster).
    let through_oklab = params.changes_colour() || spatial;

    let (w, h) = (cam.width, cam.height);
    let mut out = [0, 1, 2].map(|_| vec![0f32; w * h]);
    for_each_pixel(&mut out, w, |i, px| {
        let balanced = [0, 1, 2].map(|c| (cam.planes[c][i] * wb[c]).min(1.0));
        let toned = apply(&cam_to_rec2020, balanced).map(|v| tone(v * gain));
        let linear = apply(&REC2020_TO_SRGB, toned);
        *px = if through_oklab {
            edit::adjust(edit::to_oklab(linear.map(|v| v.clamp(0.0, 1.0))), params)
        } else {
            linear.map(srgb_gamma)
        };
    });
    if through_oklab {
        if spatial {
            spatial_filters(&mut out[0], w, h, params);
        }
        for_each_pixel(&mut out, w, |_, px| {
            *px = edit::from_oklab(*px).map(srgb_gamma)
        });
    }
    Image {
        width: w,
        height: h,
        planes: out,
    }
}

/// Passes each pixel at the same position in the three planes (parallel by row).
/// `f(pixel index, pixel)`.
fn for_each_pixel(
    planes: &mut [Vec<f32>; 3],
    width: usize,
    f: impl Fn(usize, &mut [f32; 3]) + Sync,
) {
    let [p0, p1, p2] = planes;
    (
        p0.par_chunks_mut(width),
        p1.par_chunks_mut(width),
        p2.par_chunks_mut(width),
    )
        .into_par_iter()
        .enumerate()
        .for_each(|(y, (r0, r1, r2))| {
            for x in 0..width {
                let mut px = [r0[x], r1[x], r2[x]];
                f(y * width + x, &mut px);
                (r0[x], r1[x], r2[x]) = (px[0], px[1], px[2]);
            }
        });
}

/// Applies the neighborhood adjustments (sharpen, clarity, vignette) to the lightness plane.
fn spatial_filters(l: &mut [f32], w: usize, h: usize, p: &Params) {
    let long = w.max(h) as f32;
    let radius = |fraction: f32| ((long * fraction).round() as usize).max(1);
    if p.sharpen != 0.0 {
        let blurred = box_blur(l, w, h, radius(SHARPEN_RADIUS), 1);
        l.par_iter_mut()
            .zip(&blurred)
            .for_each(|(v, b)| *v += p.sharpen * SHARPEN_GAIN * (*v - b));
    }
    if p.clarity != 0.0 {
        let blurred = box_blur(l, w, h, radius(CLARITY_RADIUS), 2);
        l.par_iter_mut().zip(&blurred).for_each(|(v, b)| {
            // Touch very dark or very bright areas less.
            let midtone = 4.0 * *v * (1.0 - *v);
            *v += p.clarity * CLARITY_GAIN * (*v - b) * midtone.clamp(0.0, 1.0);
        });
    }
    if p.vignette != 0.0 {
        let (cx, cy) = ((w as f32 - 1.0) / 2.0, (h as f32 - 1.0) / 2.0);
        l.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let dy = if cy > 0.0 { (y as f32 - cy) / cy } else { 0.0 };
            for (x, v) in row.iter_mut().enumerate() {
                let dx = if cx > 0.0 { (x as f32 - cx) / cx } else { 0.0 };
                // 0 at the center, 1 at the corners
                let distance = (dx * dx + dy * dy) / 2.0;
                *v *= 1.0 - p.vignette * VIGNETTE_GAIN * distance;
            }
        });
    }
    l.par_iter_mut().for_each(|v| *v = v.clamp(0.0, 1.0));
}

/// Box blur. At the edges the window shrinks to stay inside the image. Repeating it `passes` times
/// approximates a Gaussian.
pub fn box_blur(src: &[f32], w: usize, h: usize, radius: usize, passes: usize) -> Vec<f32> {
    let mut current = src.to_vec();
    let mut tmp = vec![0f32; src.len()];
    for _ in 0..passes {
        // Horizontal
        tmp.par_chunks_mut(w)
            .zip(current.par_chunks(w))
            .for_each(|(out, row)| {
                let mut sum: f64 = row[..=radius.min(w - 1)].iter().map(|&v| v as f64).sum();
                for x in 0..w {
                    let (lo, hi) = (x.saturating_sub(radius), (x + radius).min(w - 1));
                    out[x] = (sum / (hi - lo + 1) as f64) as f32;
                    if x + radius + 1 < w {
                        sum += row[x + radius + 1] as f64;
                    }
                    if x >= radius {
                        sum -= row[x - radius] as f64;
                    }
                }
            });
        // Vertical: keep a running window sum per column and walk down the rows.
        let mut sums = vec![0f64; w];
        for row in tmp.chunks(w).take(radius.min(h - 1) + 1) {
            sums.iter_mut().zip(row).for_each(|(s, &v)| *s += v as f64);
        }
        for y in 0..h {
            let (lo, hi) = (y.saturating_sub(radius), (y + radius).min(h - 1));
            let count = (hi - lo + 1) as f64;
            let out = &mut current[y * w..(y + 1) * w];
            out.iter_mut()
                .zip(&sums)
                .for_each(|(o, s)| *o = (s / count) as f32);
            if y + radius + 1 < h {
                let row = &tmp[(y + radius + 1) * w..(y + radius + 2) * w];
                sums.iter_mut().zip(row).for_each(|(s, &v)| *s += v as f64);
            }
            if y >= radius {
                let row = &tmp[(y - radius) * w..(y - radius + 1) * w];
                sums.iter_mut().zip(row).for_each(|(s, &v)| *s -= v as f64);
            }
        }
    }
    current
}

/// S-shaped tone curve. 0 → 0, 1 → 1.
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

/// Rotates pixels upright according to the EXIF orientation. Flips first, then transposes (swaps
/// x and y).
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

    /// The Sony A900 D65 matrix (from rawler's camera data).
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

    /// The RAW values this camera would record for a linear sRGB color.
    fn shoot(srgb: [f32; 3]) -> [f32; 3] {
        mul(&XYZ_TO_CAM, mul(&SRGB_TO_XYZ, srgb))
    }

    /// Reciprocal of the RAW values for white = this scene's white balance (G = 1).
    fn wb() -> [f32; 3] {
        let w = shoot([1.0; 3]);
        [w[1] / w[0], 1.0, w[1] / w[2]]
    }

    fn develop_px(cam: [f32; 3]) -> [f32; 3] {
        develop_px_with(cam, &Params::default())
    }

    fn develop_px_with(cam: [f32; 3], params: &Params) -> [f32; 3] {
        let img = Image {
            width: 1,
            height: 1,
            planes: cam.map(|v| vec![v]),
        };
        let out = develop(&img, wb(), XYZ_TO_CAM, params);
        [out.planes[0][0], out.planes[1][0], out.planes[2][0]]
    }

    /// A photo of a uniform middle gray.
    fn grey_image(width: usize, height: usize) -> Image {
        let cam = shoot([0.18; 3]);
        Image {
            width,
            height,
            planes: cam.map(|v| vec![v; width * height]),
        }
    }

    #[test]
    fn exposure_brightens_by_stops() {
        let grey = shoot([0.1; 3]);
        let normal = develop_px(grey)[1];
        let brighter = develop_px_with(
            grey,
            &Params {
                exposure: 1.0,
                ..Params::default()
            },
        )[1];
        let darker = develop_px_with(
            grey,
            &Params {
                exposure: -1.0,
                ..Params::default()
            },
        )[1];
        assert!(
            darker < normal && normal < brighter,
            "{darker} {normal} {brighter}"
        );
        // +1 EV equals shooting a scene with twice the light.
        assert!((brighter - develop_px(shoot([0.2; 3]))[1]).abs() < 2e-3);
    }

    #[test]
    fn temperature_warms_and_tint_shifts_towards_magenta() {
        let grey = shoot([0.18; 3]);
        let warm = develop_px_with(
            grey,
            &Params {
                temp: 0.5,
                ..Params::default()
            },
        );
        assert!(
            warm[0] > warm[1] && warm[1] > warm[2],
            "warm: red > green > blue {warm:?}"
        );
        let cool = develop_px_with(
            grey,
            &Params {
                temp: -0.5,
                ..Params::default()
            },
        );
        assert!(cool[0] < cool[2], "{cool:?}");
        let magenta = develop_px_with(
            grey,
            &Params {
                tint: 0.5,
                ..Params::default()
            },
        );
        assert!(
            magenta[1] < magenta[0] && magenta[1] < magenta[2],
            "magenta: green is lowest {magenta:?}"
        );
    }

    #[test]
    fn colour_parameters_reach_the_pixels() {
        let red = shoot([0.3, 0.02, 0.02]);
        let grey = develop_px_with(
            red,
            &Params {
                saturation: -1.0,
                ..Params::default()
            },
        );
        assert_neutral(grey);
        let mono = develop_px_with(
            red,
            &Params {
                mono: 1.0,
                ..Params::default()
            },
        );
        assert_neutral(mono);
        let vivid = develop_px_with(
            red,
            &Params {
                saturation: 0.5,
                ..Params::default()
            },
        );
        let plain = develop_px(red);
        assert!(
            vivid[0] - vivid[1] > plain[0] - plain[1],
            "raising saturation widens the red-green difference"
        );
    }

    #[test]
    fn an_unrelated_parameter_does_not_disturb_neutral_pixels() {
        // Values must not drift even through the color adjustment path (Oklab round trip).
        let grey = shoot([0.18; 3]);
        let through_colour_path = develop_px_with(
            grey,
            &Params {
                vibrance: 0.5,
                ..Params::default()
            },
        );
        let plain = develop_px(grey);
        for c in 0..3 {
            assert!(
                (through_colour_path[c] - plain[c]).abs() < 2e-3,
                "{through_colour_path:?} vs {plain:?}"
            );
        }
    }

    #[test]
    fn vignette_darkens_the_corners_and_leaves_the_centre() {
        let (w, h) = (33, 33);
        let plain = develop(&grey_image(w, h), wb(), XYZ_TO_CAM, &Params::default());
        let dark = develop(
            &grey_image(w, h),
            wb(),
            XYZ_TO_CAM,
            &Params {
                vignette: 1.0,
                ..Params::default()
            },
        );
        let (centre, corner) = (16 * w + 16, 0);
        assert!((dark.planes[1][centre] - plain.planes[1][centre]).abs() < 2e-3);
        assert!(dark.planes[1][corner] < plain.planes[1][corner] - 0.05);
        let light = develop(
            &grey_image(w, h),
            wb(),
            XYZ_TO_CAM,
            &Params {
                vignette: -1.0,
                ..Params::default()
            },
        );
        assert!(light.planes[1][corner] > plain.planes[1][corner] + 0.03);
    }

    /// A photo whose left half is dark and right half is bright.
    fn step_image(width: usize, height: usize) -> Image {
        let (dark, bright) = (shoot([0.08; 3]), shoot([0.3; 3]));
        let planes = [0, 1, 2].map(|c| {
            (0..width * height)
                .map(|i| {
                    if i % width < width / 2 {
                        dark[c]
                    } else {
                        bright[c]
                    }
                })
                .collect()
        });
        Image {
            width,
            height,
            planes,
        }
    }

    #[test]
    fn sharpening_exaggerates_an_edge_without_touching_flat_areas() {
        let (w, h) = (64, 8);
        let plain = develop(&step_image(w, h), wb(), XYZ_TO_CAM, &Params::default());
        let sharp = develop(
            &step_image(w, h),
            wb(),
            XYZ_TO_CAM,
            &Params {
                sharpen: 1.0,
                ..Params::default()
            },
        );
        let row = 4 * w;
        let (left_of_edge, right_of_edge) = (row + w / 2 - 1, row + w / 2);
        assert!(
            sharp.planes[1][left_of_edge] < plain.planes[1][left_of_edge] - 0.01,
            "the dark side of the edge gets darker"
        );
        assert!(
            sharp.planes[1][right_of_edge] > plain.planes[1][right_of_edge] + 0.01,
            "the bright side of the edge gets brighter"
        );
        assert!(
            (sharp.planes[1][row + 4] - plain.planes[1][row + 4]).abs() < 2e-3,
            "flat areas stay unchanged"
        );
    }

    #[test]
    fn clarity_strengthens_broad_contrast_and_negative_clarity_softens_it() {
        let (w, h) = (256, 8);
        let spread = |image: &Image| {
            let row = &image.planes[1][4 * w..5 * w];
            // Right next to the edge (within the clarity radius)
            row[w / 2 + 1] - row[w / 2 - 2]
        };
        let plain = spread(&develop(
            &step_image(w, h),
            wb(),
            XYZ_TO_CAM,
            &Params::default(),
        ));
        let clear = spread(&develop(
            &step_image(w, h),
            wb(),
            XYZ_TO_CAM,
            &Params {
                clarity: 1.0,
                ..Params::default()
            },
        ));
        let soft = spread(&develop(
            &step_image(w, h),
            wb(),
            XYZ_TO_CAM,
            &Params {
                clarity: -1.0,
                ..Params::default()
            },
        ));
        assert!(
            clear > plain + 0.01 && soft < plain - 0.01,
            "{soft} {plain} {clear}"
        );
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
        // A gray whose three channels all become 1.0 after white balance
        let w = wb();
        let out = develop_px([1.0 / w[0], 1.0, 1.0 / w[2]]);
        assert!(out.iter().all(|&v| v > 0.995), "{out:?}");
    }

    #[test]
    fn clipped_highlight_has_no_colour_cast() {
        // All three channels at sensor saturation. White balance makes R and B larger than G, but
        // no color cast may appear.
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
