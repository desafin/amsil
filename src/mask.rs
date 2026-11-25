//! AI masks: picks out the object at the clicked spot in a photo and adds adjustments only to that
//! area (SAM 2.1).
//!
//! The encoder runs once per photo and its result (`Embedding`) kept; the decoder runs per click.
//! The decoder's mask is a 256×256 grid, so it is scaled up to the original size following the
//! image's edges.

use crate::edit::Params;
use crate::lang::{Text, text};
use crate::preview::Pixels;
use crate::{Image, Result};
use ort::session::Session;
use ort::value::TensorRef;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Folder holding the mask model (encoder and decoder) inside the model folder.
pub const MODEL: &str = "mask-object-sam21-small";

/// One side of the encoder input (pixels). The long side of the image fed in must not exceed this.
pub const INPUT: usize = 1024;
/// One side of the mask grid the decoder outputs. A quarter of the input.
const GRID: usize = 256;

/// A clicked spot in the photo. Top left of the upright photo is (0, 0), bottom right is (1, 1).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    /// Whether this spot selects (true) or excludes (false).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub include: bool,
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// One mask: the area selected by the clicked spots, and the adjustments added only to it.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mask {
    pub points: Vec<Point>,
    /// Adds the adjustments outside the selected area.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
    #[serde(skip_serializing_if = "Params::is_neutral")]
    pub adjust: Params,
}

/// Builds the encoder input: places the image at the top left and normalizes with the ImageNet mean
/// and standard deviation (channel-first).
/// The remaining area is 0. Also returns the image size.
fn preprocess(picture: &Pixels) -> (Vec<f32>, (usize, usize)) {
    const MEAN: [f32; 3] = [123.675, 116.28, 103.53];
    const STD: [f32; 3] = [58.395, 57.12, 57.375];
    let (w, h) = (picture.width, picture.height);
    assert!(
        w <= INPUT && h <= INPUT,
        "the mask model takes images with a long side of at most {INPUT}"
    );
    let mut input = vec![0.0; 3 * INPUT * INPUT];
    for (c, plane) in input.chunks_exact_mut(INPUT * INPUT).enumerate() {
        for y in 0..h {
            for x in 0..w {
                plane[y * INPUT + x] =
                    (picture.rgba[(y * w + x) * 4 + c] as f32 - MEAN[c]) / STD[c];
            }
        }
    }
    (input, (w, h))
}

/// The encoder output for one photo (about 17MB). Reused on every click.
pub struct Embedding {
    embed: Vec<f32>,
    feats0: Vec<f32>,
    feats1: Vec<f32>,
    /// Size of the image fed to the encoder.
    size: (usize, usize),
}

/// The part of the decoder's low-resolution mask covered by the photo. Values are logits (above 0
/// is selected).
#[derive(Clone)]
pub struct Low {
    pub width: usize,
    pub height: usize,
    pub logits: Vec<f32>,
}

/// A model that picks out the object at the clicked spot (SAM 2.1 Hiera Small).
pub struct Segmenter {
    encoder: Session,
    decoder: Session,
}

impl Segmenter {
    // ponytail: runs on CPU only (the encoder takes 1–2 s per photo). Move to the GPU if the wait
    // after clicking becomes annoying.
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            encoder: crate::gpu::cpu_session(&dir.join("encoder.onnx"))?,
            decoder: crate::gpu::cpu_session(&dir.join("decoder.onnx"))?,
        })
    }

    /// Feeds the photo to the encoder. `picture` is the unadjusted photo shrunk to a long side of
    /// at most 1024.
    pub fn embed(&mut self, picture: &Pixels) -> Result<Embedding> {
        let (input, size) = preprocess(picture);
        let input = TensorRef::from_array_view(([1, 3, INPUT, INPUT], input.as_slice()))?;
        let outputs = self.encoder.run(ort::inputs![input])?;
        let take = |name: &str| -> Result<Vec<f32>> {
            Ok(outputs[name].try_extract_tensor::<f32>()?.1.to_vec())
        };
        Ok(Embedding {
            embed: take("image_embed")?,
            feats0: take("high_res_feats_0")?,
            feats1: take("high_res_feats_1")?,
            size,
        })
    }

    /// Selects an area from the clicked spots. Uses the decoder candidate the model trusts most.
    // ponytail: feeds all clicked spots at once and runs once. Does not feed back earlier results
    // to refine them.
    pub fn select(&mut self, embedding: &Embedding, points: &[Point]) -> Result<Low> {
        let (w, h) = embedding.size;
        let (width, height) = (w.div_ceil(INPUT / GRID), h.div_ceil(INPUT / GRID));
        if points.is_empty() {
            return Ok(Low {
                width,
                height,
                logits: vec![-10.0; width * height],
            });
        }
        // The model expects one padding point at (0, 0) with label -1 appended at the end.
        let coords: Vec<f32> = points
            .iter()
            .flat_map(|point| [point.x * w as f32, point.y * h as f32])
            .chain([0.0, 0.0])
            .collect();
        let labels: Vec<f32> = points
            .iter()
            .map(|point| point.include as u8 as f32)
            .chain([-1.0])
            .collect();
        let count = points.len() + 1;
        let (previous, has_previous) = (vec![0.0f32; GRID * GRID], [0.0f32]);
        let outputs = self.decoder.run(ort::inputs![
            TensorRef::from_array_view(([1, 256, 64, 64], embedding.embed.as_slice()))?,
            TensorRef::from_array_view(([1, 32, 256, 256], embedding.feats0.as_slice()))?,
            TensorRef::from_array_view(([1, 64, 128, 128], embedding.feats1.as_slice()))?,
            TensorRef::from_array_view(([1, count, 2], coords.as_slice()))?,
            TensorRef::from_array_view(([1, count], labels.as_slice()))?,
            TensorRef::from_array_view(([1, 1, GRID, GRID], previous.as_slice()))?,
            TensorRef::from_array_view(([1], has_previous.as_slice()))?,
        ])?;
        let (_, masks) = outputs["masks"].try_extract_tensor::<f32>()?;
        let (_, scores) = outputs["iou_predictions"].try_extract_tensor::<f32>()?;
        let best = (0..scores.len())
            .max_by(|&a, &b| scores[a].total_cmp(&scores[b]))
            .ok_or_else(|| {
                text!(
                    "마스크 모델이 결과를 내지 않았습니다",
                    "The mask model returned nothing",
                )
            })?;
        let grid = masks
            .get(best * GRID * GRID..(best + 1) * GRID * GRID)
            .ok_or_else(|| {
                text!(
                    "마스크 모델의 출력 크기가 예상과 다릅니다",
                    "Unexpected mask model output size",
                )
            })?;
        // Keep only the top-left part of the grid covered by the photo.
        let logits = grid
            .chunks_exact(GRID)
            .take(height)
            .flat_map(|row| &row[..width])
            .copied()
            .collect();
        Ok(Low {
            width,
            height,
            logits,
        })
    }
}

/// Loads the mask model from the model folder. If the files are missing, says how to get them.
pub fn load_segmenter() -> Result<Segmenter> {
    match missing() {
        Some(problem) => Err(problem.into()),
        None => Segmenter::load(&crate::gpu::model_dir().join(MODEL)),
    }
}

/// If the mask model is missing, a message saying so and how to get it. None if present.
pub fn missing() -> Option<Text> {
    let dir = crate::gpu::model_dir().join(MODEL);
    if dir.join("encoder.onnx").exists() && dir.join("decoder.onnx").exists() {
        return None;
    }
    let dir = dir.display();
    Some(text!(
        format!(
            "마스크 모델이 없습니다: {dir}\nfetch-models.sh를 실행해 받으세요 (실행 파일 옆이나 저장소의 scripts 폴더에 있습니다)."
        ),
        format!(
            "The mask model is missing: {dir}\nRun fetch-models.sh to download it (next to the program, or in the scripts folder of the repository)."
        ),
    ))
}

/// Scales a low-resolution mask up to the size of `guide`. The result is 0–1, inverted if `invert`.
/// When blending the four surrounding grid points, it follows those of similar lightness more, so
/// the boundary sticks to the image's edges.
pub fn upscale(low: &Low, guide: &Image, invert: bool) -> Vec<f32> {
    /// A lightness difference about this large counts as a different object.
    const SIGMA: f32 = 0.1;
    let (w, h) = (guide.width, guide.height);
    let luma = |i: usize| {
        0.2126 * guide.planes[0][i] + 0.7152 * guide.planes[1][i] + 0.0722 * guide.planes[2][i]
    };
    // How much of the grid one image pixel spans (the two ends aligned with each other)
    let step = |low: usize, full: usize| {
        if full > 1 {
            (low - 1) as f32 / (full - 1) as f32
        } else {
            0.0
        }
    };
    let (step_x, step_y) = (step(low.width, w), step(low.height, h));
    // Where the grid points fall in the image
    let back = |cell: usize, step: f32, full: usize| {
        if step > 0.0 {
            ((cell as f32 / step).round() as usize).min(full - 1)
        } else {
            0
        }
    };
    let mut alpha = vec![0.0; w * h];
    alpha.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let fy = y as f32 * step_y;
        let y0 = (fy as usize).min(low.height - 1);
        let y1 = (y0 + 1).min(low.height - 1);
        let ty = fy - y0 as f32;
        for (x, out) in row.iter_mut().enumerate() {
            let fx = x as f32 * step_x;
            let x0 = (fx as usize).min(low.width - 1);
            let x1 = (x0 + 1).min(low.width - 1);
            let tx = fx - x0 as f32;
            let here = luma(y * w + x);
            let (mut sum, mut total, mut plain) = (0.0, 0.0, 0.0);
            for (cx, cy, weight) in [
                (x0, y0, (1.0 - tx) * (1.0 - ty)),
                (x1, y0, tx * (1.0 - ty)),
                (x0, y1, (1.0 - tx) * ty),
                (x1, y1, tx * ty),
            ] {
                let there = luma(back(cy, step_y, h) * w + back(cx, step_x, w));
                let alike = (-(here - there).powi(2) / (2.0 * SIGMA * SIGMA)).exp();
                let value = low.logits[cy * low.width + cx];
                sum += value * weight * alike;
                total += weight * alike;
                plain += value * weight;
            }
            // If all four points differ entirely in lightness, blend without regard to lightness.
            let logit = if total > 1e-6 { sum / total } else { plain };
            let inside = 1.0 / (1.0 + (-logit).exp());
            *out = if invert { 1.0 - inside } else { inside };
        }
    });
    alpha
}

/// Moves `base` toward `other` by `alpha` (per pixel).
pub fn blend(base: &mut Image, other: &Image, alpha: &[f32]) {
    for (to, from) in base.planes.iter_mut().zip(&other.planes) {
        to.par_iter_mut()
            .zip(from.par_iter())
            .zip(alpha.par_iter())
            .for_each(|((to, from), alpha)| *to += (from - *to) * alpha);
    }
}

/// Tints the selected area a noticeable red (for checking the area on screen).
pub fn tint(image: &mut Image, alpha: &[f32]) {
    /// How much red takes up at the strongest spot.
    const STRENGTH: f32 = 0.45;
    for (plane, red) in image.planes.iter_mut().zip([1.0, 0.1, 0.1]) {
        plane
            .par_iter_mut()
            .zip(alpha.par_iter())
            .for_each(|(value, alpha)| *value += (red - *value) * alpha * STRENGTH);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat(width: usize, height: usize, value: u8) -> Pixels {
        Pixels {
            width,
            height,
            rgba: [value, value, value, 255].repeat(width * height),
        }
    }

    /// An image that is `left` up to column `split` and `right` beyond it (same value in all three
    /// channels).
    fn halves(width: usize, height: usize, split: usize, left: f32, right: f32) -> Image {
        let plane: Vec<f32> = (0..width * height)
            .map(|i| if i % width < split { left } else { right })
            .collect();
        Image {
            width,
            height,
            planes: [plane.clone(), plane.clone(), plane],
        }
    }

    #[test]
    fn the_picture_goes_in_the_top_left_corner_normalised_and_the_rest_stays_zero() {
        let (input, size) = preprocess(&flat(1024, 512, 128));
        assert_eq!(size, (1024, 512));
        assert_eq!(input.len(), 3 * INPUT * INPUT);
        // ImageNet mean and standard deviation: (128 - 123.675) / 58.395
        assert!((input[0] - 0.074_064).abs() < 1e-4, "{}", input[0]);
        assert!((input[INPUT * INPUT] - 0.205_182).abs() < 1e-4);
        assert!((input[2 * INPUT * INPUT] - 0.426_492).abs() < 1e-4);
        assert!(
            (input[511 * INPUT + 1023] - 0.074_064).abs() < 1e-4,
            "the image's last pixel"
        );
        assert_eq!(input[512 * INPUT], 0.0, "below the image is left at 0");
    }

    #[test]
    fn a_picture_larger_than_the_input_is_refused() {
        // Shrinking the long side to at most 1024 before feeding it in is the caller's job.
        let result = std::panic::catch_unwind(|| preprocess(&flat(2000, 100, 0)));
        assert!(result.is_err());
    }

    #[test]
    fn upscaling_follows_the_edges_of_the_picture() {
        // Low-resolution mask: the left half is selected. The boundary is mid-way (column 5).
        let low = Low {
            width: 2,
            height: 2,
            logits: vec![8.0, -8.0, 8.0, -8.0],
        };
        // In an image of even lightness, the boundary transitions smoothly at the middle.
        let smooth = upscale(&low, &halves(10, 4, 10, 0.5, 0.5), false);
        assert_eq!(smooth.len(), 40);
        assert!(smooth[0] > 0.99 && smooth[9] < 0.01);
        assert!(smooth[4] > 0.5 && smooth[5] < 0.5, "{smooth:?}");

        // If the image has an edge at column 7, the mask boundary sticks to it too.
        let snapped = upscale(&low, &halves(10, 4, 7, 0.9, 0.1), false);
        assert!(snapped[6] > 0.9, "inside the edge is selected: {snapped:?}");
        assert!(snapped[7] < 0.1, "outside the edge is not: {snapped:?}");

        let inverted = upscale(&low, &halves(10, 4, 10, 0.5, 0.5), true);
        assert!(inverted[0] < 0.01 && inverted[9] > 0.99);
    }

    #[test]
    fn blending_moves_only_the_masked_part_towards_the_other_picture() {
        let mut base = halves(4, 1, 4, 0.2, 0.2);
        let other = halves(4, 1, 4, 1.0, 1.0);
        blend(&mut base, &other, &[0.0, 0.5, 1.0, 0.25]);
        for (got, wanted) in base.planes[1].iter().zip([0.2, 0.6, 1.0, 0.4]) {
            assert!((got - wanted).abs() < 1e-6, "{:?}", base.planes[1]);
        }
    }

    #[test]
    fn a_mask_is_written_compactly_and_read_back() {
        let mask = Mask {
            points: vec![
                Point {
                    x: 0.5,
                    y: 0.25,
                    include: true,
                },
                Point {
                    x: 0.1,
                    y: 0.9,
                    include: false,
                },
            ],
            invert: false,
            adjust: Params {
                exposure: 0.5,
                ..Params::default()
            },
        };
        let text = toml::to_string(&mask).unwrap();
        assert!(!text.contains("invert"), "defaults are not written: {text}");
        assert!(!text.contains("include = true"), "{text}");
        assert!(
            text.contains("include = false") && text.contains("exposure = 0.5"),
            "{text}"
        );
        assert_eq!(toml::from_str::<Mask>(&text).unwrap(), mask);
    }

    #[test]
    fn a_click_on_a_real_photo_selects_the_thing_under_it() {
        // Runs only when the model and sample photos are present.
        let photo =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110/DSC04551.ARW");
        let Ok(mut segmenter) = load_segmenter() else {
            eprintln!("no mask model, skipping");
            return;
        };
        if !photo.exists() {
            eprintln!("no sample photos, skipping");
            return;
        }
        let picture = crate::preview::thumbnail(&photo, INPUT)
            .unwrap()
            .expect("no preview");
        let started = std::time::Instant::now();
        let embedding = segmenter.embed(&picture).unwrap();
        let embedded = started.elapsed();
        // Click the glass building in the middle.
        let tower = Point {
            x: 0.48,
            y: 0.35,
            include: true,
        };
        let started = std::time::Instant::now();
        let low = segmenter.select(&embedding, &[tower]).unwrap();
        eprintln!("encoder {embedded:.2?}, decoder {:.2?}", started.elapsed());
        assert_eq!(
            (low.width, low.height),
            (picture.width.div_ceil(4), picture.height.div_ceil(4))
        );
        let at = |x: f32, y: f32| {
            let (col, row) = (
                (x * low.width as f32) as usize,
                (y * low.height as f32) as usize,
            );
            low.logits[row * low.width + col]
        };
        assert!(
            at(0.48, 0.35) > 0.0,
            "the clicked spot is inside the selected area"
        );
        assert!(at(0.1, 0.1) < 0.0, "the sky is not");
        assert!(at(0.5, 0.95) < 0.0, "nor the lawn");
        let share =
            low.logits.iter().filter(|&&v| v > 0.0).count() as f32 / low.logits.len() as f32;
        assert!((0.03..0.3).contains(&share), "selected area {share}");

        // With no clicked spots, nothing is selected.
        let nothing = segmenter.select(&embedding, &[]).unwrap();
        assert!(nothing.logits.iter().all(|&v| v < 0.0));
    }
}
