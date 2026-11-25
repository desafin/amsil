//! RCD (Ratio Corrected Demosaicing) demosaic.
//!
//! Original: Luis Sanz Rodríguez, "RATIO CORRECTED DEMOSAICING", Release 2.3 @ 171125
//! (<https://github.com/LuisSR/RCD-Demosaicing>, commit 0aa9d6e234f5), GPL-3.0.
//! The original C code is ported to Rust as is, down to the order of operations, so interior pixels
//! match the original exactly (verified against the reference data in `scripts/rcd-reference`). It
//! differs from the original in two ways.
//! - Start column: in the original, operator precedence makes `4 + FC(row, 0) & 1` evaluate as
//!   `(4 + FC) & 1`, so it also processes columns 0–3 and reads the end of the previous row (on row
//!   4 it reads before the start of the buffer). Here it starts at column 4 or 5 as intended. The
//!   effect reaches up to column 8.
//! - Borders: the original fills the outer 4 pixels with a 3×3 same-color average. Because its
//!   buffer starts at zero, the pixels just inside (4–9) come out dark, so here the outer 10 pixels
//!   are filled the same way.

use crate::Image;

/// Width (pixels) of the outer band filled with the 3×3 same-color average.
pub const BORDER: usize = 10;

/// Demosaics a Bayer CFA to RGB with RCD. `cfa` holds 0..1 values for the full sensor
/// (width × height) and must already be multiplied by white balance and clipped to [0, 1] (the same
/// state the original receives from dcraw). `red` is the position (x, y) of the first R pixel, each
/// 0 or 1. The result is also 0..1 and still has white balance applied.
pub fn demosaic(cfa: &[f32], width: usize, height: usize, red: (usize, usize)) -> Image {
    assert_eq!(cfa.len(), width * height);
    let mut rgb = [0, 1, 2].map(|_| vec![0f32; width * height]);
    for y in 0..height {
        for x in 0..width {
            let i = y * width + x;
            rgb[colour_at(x, y, red)][i] = cfa[i];
        }
    }
    // The original algorithm only runs at least 4 pixels in from every edge. Anything smaller only
    // gets the border fill.
    if width > 8 && height > 8 {
        let rcd = Rcd {
            cfa,
            width,
            height,
            red,
        };
        let vh_dir = rcd.vh_direction();
        let lpf = rcd.low_pass();
        rcd.green_at_red_blue(&vh_dir, &lpf, &mut rgb);
        let pq_dir = rcd.pq_direction();
        rcd.red_blue_at_blue_red(&pq_dir, &mut rgb);
        rcd.red_blue_at_green(&vh_dir, &mut rgb);
    }
    fill_border(cfa, width, height, red, &mut rgb);
    Image {
        width,
        height,
        planes: rgb,
    }
}

/// Color of pixel (x, y): 0 = R, 1 = G, 2 = B.
pub fn colour_at(x: usize, y: usize, red: (usize, usize)) -> usize {
    match ((x + red.0).is_multiple_of(2), (y + red.1).is_multiple_of(2)) {
        (true, true) => 0,
        (false, false) => 2,
        _ => 1,
    }
}

/// The original's epsilon (to avoid division by zero).
const EPS: f32 = 1e-5;
const EPSSQ: f32 = 1e-10;

/// The 40 terms of the quadratic used for direction detection: (coef, a, b) → coef·x_a·x_b, where
/// x_k is the value k steps from the center. Listed in the same order as the original (RCD:57-58,
/// 131-132). They must be summed in this order to match the original bit for bit.
#[rustfmt::skip]
const TERMS: [(f32, i8, i8); 40] = [
    (-18.0, 0, -1), (-18.0, 0, 1), (-36.0, 0, -2), (-36.0, 0, 2), (18.0, 0, -3), (18.0, 0, 3),
    (-2.0, 0, -4), (-2.0, 0, 4), (38.0, 0, 0),
    (-70.0, -1, 1), (-12.0, -1, -2), (24.0, -1, 2), (-38.0, -1, -3), (16.0, -1, 3), (12.0, -1, -4),
    (-6.0, -1, 4), (46.0, -1, -1),
    (24.0, 1, -2), (-12.0, 1, 2), (16.0, 1, -3), (-38.0, 1, 3), (-6.0, 1, -4), (12.0, 1, 4),
    (46.0, 1, 1),
    (14.0, -2, 2), (-12.0, -2, 3), (-2.0, -2, -4), (2.0, -2, 4), (11.0, -2, -2),
    (-12.0, 2, -3), (2.0, 2, -4), (-2.0, 2, 4), (11.0, 2, 2),
    (2.0, -3, 3), (-6.0, -3, -4), (10.0, -3, -3), (-6.0, 3, 4), (10.0, 3, 3),
    (1.0, -4, -4), (1.0, 4, 4),
];

/// Local high-frequency strength of 9 pixels along one direction (−4..4 from the center). The
/// original's V_Stat and friends (including `MAX(…, epssq)`).
fn stat(x: impl Fn(isize) -> f32) -> f32 {
    // C's `a - 18.f * b * c` is bit-identical to `a + (-18.f * b) * c` (negation is exact).
    let mut sum = 0f32;
    for (coefficient, a, b) in TERMS {
        sum += (coefficient * x(a as isize)) * x(b as isize);
    }
    sum.max(EPSSQ)
}

/// The original's gradient: `eps + fabs(d1) + fabs(d2) + …`. `fabs` returns double, so the sum is
/// taken in double and then stored as float.
fn gradient(differences: &[f32]) -> f32 {
    let mut sum = EPS as f64;
    for d in differences {
        sum += d.abs() as f64;
    }
    sum as f32
}

/// Whichever of the center value and the mean of its four diagonal neighbors is farther from 0.5
/// (the original's `*_Disc`).
fn discriminate(central: f32, neighbours: [f32; 4]) -> f32 {
    let neighbourhood = 0.25 * (neighbours[0] + neighbours[1] + neighbours[2] + neighbours[3]);
    if (0.5 - central).abs() < (0.5 - neighbourhood).abs() {
        neighbourhood
    } else {
        central
    }
}

struct Rcd<'a> {
    cfa: &'a [f32],
    width: usize,
    height: usize,
    red: (usize, usize),
}

impl Rcd<'_> {
    fn colour(&self, x: usize, y: usize) -> usize {
        colour_at(x, y, self.red)
    }

    /// Column where R/B pixels start in this row (at least `from`). What the original meant by
    /// `from + FC(row, 0) & 1`.
    fn red_blue_start(&self, y: usize, from: usize) -> usize {
        from + usize::from(self.colour(0, y) == 1)
    }

    /// Column where green pixels start in this row (at least 4).
    fn green_start(&self, y: usize) -> usize {
        4 + usize::from(self.colour(1, y) == 1)
    }

    fn at(&self, i: usize, offset: isize) -> f32 {
        self.cfa[i.wrapping_add_signed(offset)]
    }

    /// Step 1.1: vertical/horizontal discrimination (RCD:54-63).
    fn vh_direction(&self) -> Vec<f32> {
        let (w, h) = (self.width, self.height);
        let w1 = w as isize;
        let mut vh_dir = vec![0f32; w * h];
        for y in 4..h - 4 {
            for x in 4..w - 4 {
                let i = y * w + x;
                let v = stat(|k| self.at(i, k * w1));
                let hh = stat(|k| self.at(i, k));
                vh_dir[i] = v / (v + hh);
            }
        }
        vh_dir
    }

    /// Step 2.1: low-pass filter at R/B sites (RCD:71-79).
    fn low_pass(&self) -> Vec<f32> {
        let (w, h) = (self.width, self.height);
        let w1 = w as isize;
        let mut lpf = vec![0f32; w * h];
        for y in 2..h - 2 {
            for x in (self.red_blue_start(y, 2)..w - 2).step_by(2) {
                let i = y * w + x;
                let c = |k| self.at(i, k);
                lpf[i] = 0.25 * c(0)
                    + 0.125 * (c(-w1) + c(w1) + c(-1) + c(1))
                    + 0.0625 * (c(-w1 - 1) + c(-w1 + 1) + c(w1 - 1) + c(w1 + 1));
            }
        }
        lpf
    }

    /// Step 3.1: green at R/B sites (RCD:87-116).
    fn green_at_red_blue(&self, vh_dir: &[f32], lpf: &[f32], rgb: &mut [Vec<f32>; 3]) {
        let (w, h) = (self.width, self.height);
        let (w1, w2, w3, w4) = (w as isize, 2 * w as isize, 3 * w as isize, 4 * w as isize);
        for y in 4..h - 4 {
            for x in (self.red_blue_start(y, 4)..w - 4).step_by(2) {
                let i = y * w + x;
                let c = |k| self.at(i, k);
                let l = |k: isize| lpf[i.wrapping_add_signed(k)];
                let d = |k: isize| vh_dir[i.wrapping_add_signed(k)];
                let disc = discriminate(d(0), [d(-w1 - 1), d(-w1 + 1), d(w1 - 1), d(w1 + 1)]);

                let n_grad = gradient(&[
                    c(-w1) - c(w1),
                    c(0) - c(-w2),
                    c(-w1) - c(-w3),
                    c(-w2) - c(-w4),
                ]);
                let s_grad =
                    gradient(&[c(w1) - c(-w1), c(0) - c(w2), c(w1) - c(w3), c(w2) - c(w4)]);
                let w_grad = gradient(&[c(-1) - c(1), c(0) - c(-2), c(-1) - c(-3), c(-2) - c(-4)]);
                let e_grad = gradient(&[c(1) - c(-1), c(0) - c(2), c(1) - c(3), c(2) - c(4)]);

                let estimate =
                    |k: isize, k2: isize| c(k) * (1.0 + (l(0) - l(k2)) / (EPS + l(0) + l(k2)));
                let n_est = estimate(-w1, -w2);
                let s_est = estimate(w1, w2);
                let w_est = estimate(-1, -2);
                let e_est = estimate(1, 2);

                let v_est = (s_grad * n_est + n_grad * s_est) / (n_grad + s_grad);
                let h_est = (w_grad * e_est + e_grad * w_est) / (e_grad + w_grad);
                rgb[1][i] = (disc * h_est + (1.0 - disc) * v_est).clamp(0.0, 1.0);
            }
        }
    }

    /// Step 4.1: diagonal P(↘)/Q(↙) discrimination (RCD:126-137).
    fn pq_direction(&self) -> Vec<f32> {
        let (w, h) = (self.width, self.height);
        let w1 = w as isize;
        let mut pq_dir = vec![0f32; w * h];
        for y in 4..h - 4 {
            for x in (self.red_blue_start(y, 4)..w - 4).step_by(2) {
                let i = y * w + x;
                let p = stat(|k| self.at(i, k * (w1 + 1)));
                let q = stat(|k| self.at(i, -k * (w1 - 1)));
                pq_dir[i] = p / (p + q);
            }
        }
        pq_dir
    }

    /// Step 4.2: R at B sites, B at R sites (RCD:140-169).
    fn red_blue_at_blue_red(&self, pq_dir: &[f32], rgb: &mut [Vec<f32>; 3]) {
        let (w, h) = (self.width, self.height);
        let (w1, w2, w3) = (w as isize, 2 * w as isize, 3 * w as isize);
        for y in 4..h - 4 {
            let start = self.red_blue_start(y, 4);
            let c = 2 - self.colour(start, y);
            for x in (start..w - 4).step_by(2) {
                let i = y * w + x;
                let at = |plane: &Vec<f32>, k: isize| plane[i.wrapping_add_signed(k)];
                let (o, g) = (&rgb[c], &rgb[1]);
                let d = |k: isize| pq_dir[i.wrapping_add_signed(k)];
                let disc = discriminate(d(0), [d(-w1 - 1), d(-w1 + 1), d(w1 - 1), d(w1 + 1)]);

                let nw_grad = gradient(&[
                    at(o, -w1 - 1) - at(o, w1 + 1),
                    at(o, -w1 - 1) - at(o, -w3 - 3),
                    at(g, 0) - at(g, -w2 - 2),
                ]);
                let ne_grad = gradient(&[
                    at(o, -w1 + 1) - at(o, w1 - 1),
                    at(o, -w1 + 1) - at(o, -w3 + 3),
                    at(g, 0) - at(g, -w2 + 2),
                ]);
                let sw_grad = gradient(&[
                    at(o, w1 - 1) - at(o, -w1 + 1),
                    at(o, w1 - 1) - at(o, w3 - 3),
                    at(g, 0) - at(g, w2 - 2),
                ]);
                let se_grad = gradient(&[
                    at(o, w1 + 1) - at(o, -w1 - 1),
                    at(o, w1 + 1) - at(o, w3 + 3),
                    at(g, 0) - at(g, w2 + 2),
                ]);

                let difference = |k: isize| at(o, k) - at(g, k);
                let (nw_est, ne_est) = (difference(-w1 - 1), difference(-w1 + 1));
                let (sw_est, se_est) = (difference(w1 - 1), difference(w1 + 1));

                let p_est = (nw_grad * se_est + se_grad * nw_est) / (nw_grad + se_grad);
                let q_est = (ne_grad * sw_est + sw_grad * ne_est) / (ne_grad + sw_grad);
                let value = (at(g, 0) + (1.0 - disc) * p_est + disc * q_est).clamp(0.0, 1.0);
                rgb[c][i] = value;
            }
        }
    }

    /// Step 4.3: R and B at green sites (RCD:174-206).
    fn red_blue_at_green(&self, vh_dir: &[f32], rgb: &mut [Vec<f32>; 3]) {
        let (w, h) = (self.width, self.height);
        let (w1, w2, w3) = (w as isize, 2 * w as isize, 3 * w as isize);
        for y in 4..h - 4 {
            for x in (self.green_start(y)..w - 4).step_by(2) {
                let i = y * w + x;
                let d = |k: isize| vh_dir[i.wrapping_add_signed(k)];
                let disc = discriminate(d(0), [d(-w1 - 1), d(-w1 + 1), d(w1 - 1), d(w1 + 1)]);
                for c in [0, 2] {
                    let at = |plane: &Vec<f32>, k: isize| plane[i.wrapping_add_signed(k)];
                    let (o, g) = (&rgb[c], &rgb[1]);
                    let n_grad = gradient(&[
                        at(g, 0) - at(g, -w2),
                        at(o, -w1) - at(o, w1),
                        at(o, -w1) - at(o, -w3),
                    ]);
                    let s_grad = gradient(&[
                        at(g, 0) - at(g, w2),
                        at(o, w1) - at(o, -w1),
                        at(o, w1) - at(o, w3),
                    ]);
                    let w_grad = gradient(&[
                        at(g, 0) - at(g, -2),
                        at(o, -1) - at(o, 1),
                        at(o, -1) - at(o, -3),
                    ]);
                    let e_grad = gradient(&[
                        at(g, 0) - at(g, 2),
                        at(o, 1) - at(o, -1),
                        at(o, 1) - at(o, 3),
                    ]);

                    let difference = |k: isize| at(o, k) - at(g, k);
                    let (n_est, s_est) = (difference(-w1), difference(w1));
                    let (w_est, e_est) = (difference(-1), difference(1));

                    let v_est = (n_grad * s_est + s_grad * n_est) / (n_grad + s_grad);
                    let h_est = (e_grad * w_est + w_grad * e_est) / (e_grad + w_grad);
                    let value = (at(g, 0) + (1.0 - disc) * v_est + disc * h_est).clamp(0.0, 1.0);
                    rgb[c][i] = value;
                }
            }
        }
    }
}

/// For each pixel in the outer `BORDER`-pixel band, fills the channels other than its own color
/// with the average of same-color pixels in the 3×3 neighborhood (same rule as dcraw's
/// `border_interpolate`).
fn fill_border(
    cfa: &[f32],
    width: usize,
    height: usize,
    red: (usize, usize),
    rgb: &mut [Vec<f32>; 3],
) {
    for y in 0..height {
        let inner = y >= BORDER && y + BORDER < height;
        for x in 0..width {
            if inner && x >= BORDER && x + BORDER < width {
                continue;
            }
            let (mut sum, mut count) = ([0f32; 3], [0u32; 3]);
            for sy in y.saturating_sub(1)..(y + 2).min(height) {
                for sx in x.saturating_sub(1)..(x + 2).min(width) {
                    let c = colour_at(sx, sy, red);
                    sum[c] += cfa[sy * width + sx];
                    count[c] += 1;
                }
            }
            let native = colour_at(x, y, red);
            for c in 0..3 {
                if c != native && count[c] > 0 {
                    rgb[c][y * width + x] = sum[c] / count[c] as f32;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One reference case: size, R position, input, and the original's output.
    struct Reference {
        width: usize,
        height: usize,
        red: (usize, usize),
        cfa: Vec<u16>,
        rgb: Vec<u16>,
    }

    impl Reference {
        fn input(&self) -> Vec<f32> {
            self.cfa.iter().map(|&v| v as f32 / 65535.0).collect()
        }
    }

    fn reference(name: &str) -> Reference {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(format!("tests/data/rcd/{name}.bin"));
        let bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[..4], b"RCD1");
        let word = |i: usize| u32::from_le_bytes(bytes[4 + 4 * i..8 + 4 * i].try_into().unwrap());
        let (width, height, filters) = (word(0) as usize, word(1) as usize, word(2));
        let red = match filters {
            0x94949494 => (0, 0),
            0x16161616 => (1, 1),
            0x61616161 => (1, 0),
            0x49494949 => (0, 1),
            other => panic!("{other:x}"),
        };
        let values: Vec<u16> = bytes[16..]
            .chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect();
        let (cfa, rgb) = values.split_at(width * height);
        assert_eq!(rgb.len(), width * height * 3);
        Reference {
            width,
            height,
            red,
            cfa: cfa.to_vec(),
            rgb: rgb.to_vec(),
        }
    }

    /// The original's output conversion: `(ushort)CLIP(65535.f * v)`, i.e. clip to 0..65535 and
    /// truncate.
    fn quantise(v: f32) -> u16 {
        ((65535.0 * v) as i32).clamp(0, 65535) as u16
    }

    #[test]
    fn colours_follow_the_bayer_layout() {
        // RGGB: even rows are R G R G, odd rows are G B G B
        assert_eq!(
            [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(x, y)| colour_at(x, y, (0, 0))),
            [0, 1, 1, 2]
        );
        // GBRG: first R is at (0, 1)
        assert_eq!(
            [(0, 0), (1, 0), (0, 1), (1, 1)].map(|(x, y)| colour_at(x, y, (0, 1))),
            [1, 2, 0, 1]
        );
        assert_eq!(colour_at(3, 3, (1, 1)), 0);
        assert_eq!(colour_at(2, 2, (1, 1)), 2);
    }

    #[test]
    fn matches_the_original_inside_the_border() {
        for layout in ["rggb", "bggr", "grbg", "gbrg"] {
            for content in ["random", "edges"] {
                let name = format!("{layout}-{content}");
                let r = reference(&name);
                let out = demosaic(&r.input(), r.width, r.height, r.red);
                let mut differ = Vec::new();
                for y in BORDER..r.height - BORDER {
                    for x in BORDER..r.width - BORDER {
                        for c in 0..3 {
                            let i = y * r.width + x;
                            let (ours, theirs) = (quantise(out.planes[c][i]), r.rgb[i * 3 + c]);
                            if ours != theirs {
                                differ.push((x, y, c, ours, theirs));
                            }
                        }
                    }
                }
                assert!(
                    differ.is_empty(),
                    "{name}: {} pixels differ. First few (x, y, channel, ours, original): {:?}",
                    differ.len(),
                    &differ[..differ.len().min(5)]
                );
            }
        }
    }

    #[test]
    fn native_samples_are_kept_and_a_grey_mosaic_stays_grey() {
        let (w, h) = (40, 30);
        let grey = vec![0.4f32; w * h];
        let out = demosaic(&grey, w, h, (0, 0));
        for plane in &out.planes {
            assert!(plane.iter().all(|&v| (v - 0.4).abs() < 1e-6));
        }
        let r = reference("rggb-random");
        let cfa = r.input();
        let out = demosaic(&cfa, r.width, r.height, r.red);
        for y in 0..r.height {
            for x in 0..r.width {
                let i = y * r.width + x;
                assert_eq!(out.planes[colour_at(x, y, r.red)][i], cfa[i], "({x}, {y})");
            }
        }
    }

    #[test]
    fn the_border_ring_comes_from_same_colour_neighbours() {
        let r = reference("gbrg-edges");
        let cfa = r.input();
        let out = demosaic(&cfa, r.width, r.height, r.red);
        for (x, y) in [
            (0, 0),
            (r.width - 1, 0),
            (5, r.height - 1),
            (r.width - 1, 20),
            (9, 9),
            (20, 3),
        ] {
            for c in 0..3 {
                if c == colour_at(x, y, r.red) {
                    continue; // Own color is unchanged (test above)
                }
                let mut samples = Vec::new();
                for sy in y.saturating_sub(1)..(y + 2).min(r.height) {
                    for sx in x.saturating_sub(1)..(x + 2).min(r.width) {
                        if colour_at(sx, sy, r.red) == c {
                            samples.push(cfa[sy * r.width + sx]);
                        }
                    }
                }
                let mean = samples.iter().sum::<f32>() / samples.len() as f32;
                let got = out.planes[c][y * r.width + x];
                assert!((got - mean).abs() < 1e-6, "({x}, {y}) {c}: {got} {mean}");
            }
        }
    }
}
