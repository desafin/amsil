//! Portrait skin retouch (experimental): finds skin automatically and softens blemishes and uneven
//! tone only within it.
//!
//! Finding skin is done by the MediaPipe selfie multiclass model (256×256 grid). The retouch is not
//! a neural network but the difference of two blurs: fine texture (pores, fine hair) and broad
//! shading (the face's form) are kept, and only blemishes of the size in between are reduced.
//! Distinct edges such as eyes and lips differ too much to be touched.

use crate::develop::box_blur;
use crate::lang::{Text, text};
use crate::mask::Low;
use crate::preview::Pixels;
use crate::{Image, Result};
use ort::session::Session;
use ort::value::TensorRef;
use rayon::prelude::*;
use std::path::Path;

/// Location of the skin model inside the model folder.
pub const MODEL: &str = "selfie-multiclass/model.onnx";

/// One side of the model's input and output grid (pixels).
const SIDE: usize = 256;
/// Number of classes the model outputs per pixel: background, hair, body skin, face skin, clothes,
/// other.
const CLASSES: usize = 6;
/// Those among them that are skin.
const SKIN: [usize; 2] = [2, 3];

/// Fits the whole image to 256×256 (stretched or shrunk without keeping the aspect ratio, as the
/// model was trained).
/// Values are 0–1, laid out per pixel in R, G, B order.
fn preprocess(picture: &Pixels) -> Vec<f32> {
    let (w, h) = (picture.width, picture.height);
    let mut input = vec![0.0; SIDE * SIDE * 3];
    for oy in 0..SIDE {
        let (y0, y1) = (oy * h / SIDE, ((oy + 1) * h / SIDE).max(oy * h / SIDE + 1));
        for ox in 0..SIDE {
            let (x0, x1) = (ox * w / SIDE, ((ox + 1) * w / SIDE).max(ox * w / SIDE + 1));
            let count = ((x1 - x0) * (y1 - y0)) as f32;
            for c in 0..3 {
                let sum: u32 = (y0..y1)
                    .flat_map(|y| (x0..x1).map(move |x| (y.min(h - 1) * w + x.min(w - 1)) * 4 + c))
                    .map(|i| picture.rgba[i] as u32)
                    .sum();
                input[(oy * SIDE + ox) * 3 + c] = sum as f32 / count / 255.0;
            }
        }
    }
    input
}

/// Computes the "is it skin" logit from one pixel's class scores (6 logits).
fn skin_logit(scores: &[f32]) -> f32 {
    let top = scores.iter().copied().fold(f32::MIN, f32::max);
    let total: f32 = scores.iter().map(|score| (score - top).exp()).sum();
    let skin: f32 = SKIN.iter().map(|&class| (scores[class] - top).exp()).sum();
    let p = (skin / total).clamp(1e-5, 1.0 - 1e-5);
    (p / (1.0 - p)).ln()
}

/// A model that finds skin (face and body) in a photo.
pub struct SkinFinder {
    session: Session,
}

impl SkinFinder {
    pub fn load(model: &Path) -> Result<Self> {
        Ok(Self {
            session: crate::gpu::cpu_session(model)?,
        })
    }

    /// Returns the likelihood of skin as logits on a 256×256 grid. The grid covers the whole image.
    pub fn find(&mut self, picture: &Pixels) -> Result<Low> {
        let input = preprocess(picture);
        let input = TensorRef::from_array_view(([1, SIDE, SIDE, 3], input.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, scores) = outputs[0].try_extract_tensor::<f32>()?;
        if scores.len() != SIDE * SIDE * CLASSES {
            return Err(text!(
                "피부를 찾는 모델의 출력 크기가 예상과 다릅니다",
                "Unexpected output size from the skin model",
            )
            .into());
        }
        Ok(Low {
            width: SIDE,
            height: SIDE,
            logits: scores.chunks_exact(CLASSES).map(skin_logit).collect(),
        })
    }
}

/// Loads the skin model from the model folder. If the model file is missing, says how to get it.
pub fn load_finder() -> Result<SkinFinder> {
    match missing() {
        Some(problem) => Err(problem.into()),
        None => SkinFinder::load(&crate::gpu::model_dir().join(MODEL)),
    }
}

/// If the skin model is missing, a message saying so and how to get it. None if present.
pub fn missing() -> Option<Text> {
    let path = crate::gpu::model_dir().join(MODEL);
    if path.exists() {
        return None;
    }
    let path = path.display();
    Some(text!(
        format!(
            "피부 보정 모델이 없습니다: {path}\nfetch-models.sh를 실행해 받으세요 (실행 파일 옆이나 저장소의 scripts 폴더에 있습니다)."
        ),
        format!(
            "The skin model is missing: {path}\nRun fetch-models.sh to download it (next to the program, or in the scripts folder of the repository)."
        ),
    ))
}

/// Softens mid-sized blemishes by `strength` (0–1) within the skin area (`alpha`, 0–1).
// ponytail: a simple method that subtracts the difference of two blurs. A faint halo may remain
// right next to distinct edges.
// If it bothers, switch to an edge-preserving filter (guided filter etc.).
pub fn smooth(image: &mut Image, alpha: &[f32], strength: f32) {
    /// The largest size seen as a blemish (ratio to the square root of the skin area).
    const COARSE: f32 = 0.06;
    /// Size of the fine texture kept as is (a fraction of the larger size).
    const FINE: f32 = 1.0 / 8.0;
    /// A lightness difference larger than this is an edge (eyes, lips, hair), not a blemish.
    const EDGE: f32 = 0.06;

    let (w, h) = (image.width, image.height);
    // The model gives faint values (a few %) even where there is no person. Count only sure areas
    // as skin: below 0.4 is 0, above 0.8 is 1.
    let alpha: Vec<f32> = alpha
        .par_iter()
        .map(|alpha| ((alpha - 0.4) / 0.4).clamp(0.0, 1.0))
        .collect();
    let skin: f32 = alpha.par_iter().sum::<f32>() / alpha.len().max(1) as f32;
    if strength <= 0.0 || skin <= 0.0 {
        return;
    }
    // A face shot large has large blemishes too. Gauge the size from the area skin covers.
    let coarse = (COARSE * skin.sqrt() * w.min(h) as f32).round().max(2.0) as usize;
    let fine = (coarse as f32 * FINE).round().max(1.0) as usize;
    // Only mid-sized variation: lightly blurred - heavily blurred
    let band = |plane: &[f32]| -> Vec<f32> {
        let mut band = box_blur(plane, w, h, fine, 2);
        let wide = box_blur(plane, w, h, coarse, 2);
        band.par_iter_mut()
            .zip(wide.par_iter())
            .for_each(|(band, wide)| *band -= wide);
        band
    };
    // Pick only places with small variation in lightness (it must apply equally to all three
    // channels so colours don't shift).
    let luma: Vec<f32> = (0..w * h)
        .into_par_iter()
        .map(|i| {
            0.2126 * image.planes[0][i] + 0.7152 * image.planes[1][i] + 0.0722 * image.planes[2][i]
        })
        .collect();
    let amount: Vec<f32> = band(&luma)
        .par_iter()
        .zip(alpha.par_iter())
        .map(|(band, alpha)| strength * alpha * (-(band / EDGE).powi(2)).exp())
        .collect();
    drop(luma);
    for plane in &mut image.planes {
        let band = band(plane);
        plane
            .par_iter_mut()
            .zip(band.par_iter())
            .zip(amount.par_iter())
            .for_each(|((value, band), amount)| *value -= amount * band);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_whole_picture_is_stretched_to_the_model_size() {
        // A wide image, white on the left half and black on the right half
        let (w, h) = (512, 100);
        let mut picture = Pixels {
            width: w,
            height: h,
            rgba: vec![0; w * h * 4],
        };
        for y in 0..h {
            for x in 0..w / 2 {
                picture.rgba[(y * w + x) * 4..(y * w + x) * 4 + 3].fill(255);
            }
        }
        let input = preprocess(&picture);
        assert_eq!(input.len(), SIDE * SIDE * 3);
        let pixel = |x: usize, y: usize| &input[(y * SIDE + x) * 3..(y * SIDE + x) * 3 + 3];
        assert_eq!(pixel(0, 0), [1.0; 3]);
        assert_eq!(
            pixel(127, 255),
            [1.0; 3],
            "stretched, not cropped: white up to the middle"
        );
        assert_eq!(pixel(128, 0), [0.0; 3]);
        assert_eq!(pixel(255, 255), [0.0; 3]);
    }

    #[test]
    fn skin_is_face_plus_body_against_everything_else() {
        // Order: background, hair, body skin, face skin, clothes, other
        assert!(skin_logit(&[0.0, 0.0, 0.0, 9.0, 0.0, 0.0]) > 4.0, "face");
        assert!(skin_logit(&[0.0, 0.0, 9.0, 0.0, 0.0, 0.0]) > 4.0, "body");
        assert!(
            skin_logit(&[9.0, 0.0, 0.0, 0.0, 0.0, 0.0]) < -4.0,
            "background"
        );
        assert!(skin_logit(&[0.0, 9.0, 0.0, 0.0, 0.0, 0.0]) < -4.0, "hair");
        // Split between face and body, but clearly skin
        assert!(skin_logit(&[0.0, 0.0, 9.0, 9.0, 0.0, 0.0]) > 4.0);
        // Half and half gives 0
        assert!(skin_logit(&[5.0, 0.0, -20.0, 5.0, -20.0, -20.0]).abs() < 0.1);
        assert!(skin_logit(&[-50.0, -50.0, 50.0, 50.0, -50.0, -50.0]).is_finite());
    }

    /// Test face skin: an even base with fine texture, a mid-sized blemish, and a distinct edge.
    fn patch() -> Image {
        let (w, h) = (200, 200);
        let plane: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                // Fine texture: alternating ±0.01 per pixel
                let grain = if (i % w + i / w) % 2 == 0 {
                    0.01
                } else {
                    -0.01
                };
                // Blemish: a dark spot at (60, 100), radius about 5, depth 0.04
                let d2 = (x - 60.0).powi(2) + (y - 100.0).powi(2);
                let blotch = -0.04 * (-d2 / (2.0 * 3.0 * 3.0)).exp();
                // Edge: much darker from column 150 (eyebrow or hair)
                let edge = if x >= 150.0 { -0.3 } else { 0.0 };
                0.6 + grain + blotch + edge
            })
            .collect();
        Image {
            width: w,
            height: h,
            planes: [plane.clone(), plane.clone(), plane],
        }
    }

    /// Mean around a point (radius 1). Removes the fine texture to see the lightness there.
    fn level(image: &Image, x: usize, y: usize) -> f32 {
        let mut sum = 0.0;
        for dy in 0..3 {
            for dx in 0..3 {
                sum += image.planes[1][(y + dy - 1) * image.width + x + dx - 1];
            }
        }
        sum / 9.0
    }

    #[test]
    fn smoothing_fades_blotches_but_keeps_grain_and_edges() {
        let before = patch();
        let mut after = patch();
        smooth(&mut after, &vec![1.0; 200 * 200], 1.0);

        // Blemish: depth relative to the surroundings (60, 70) drops below half.
        let depth = |image: &Image| level(image, 60, 70) - level(image, 60, 100);
        assert!(depth(&before) > 0.03, "{}", depth(&before));
        assert!(
            depth(&after) < depth(&before) * 0.5,
            "{} → {}",
            depth(&before),
            depth(&after)
        );
        // Fine texture: the difference between neighbouring pixels (0.02) is nearly unchanged.
        let grain =
            |image: &Image| (image.planes[1][30 * 200 + 30] - image.planes[1][30 * 200 + 31]).abs();
        assert!(grain(&after) > grain(&before) * 0.9, "{}", grain(&after));
        // Edge: the difference between columns 149 and 150 (0.3) is nearly unchanged.
        let step = |image: &Image| level(image, 147, 40) - level(image, 152, 40);
        assert!(
            step(&after) > step(&before) * 0.9,
            "{} → {}",
            step(&before),
            step(&after)
        );
    }

    #[test]
    fn smoothing_stays_inside_the_skin_and_follows_the_strength() {
        let before = patch();
        // Non-skin areas are unchanged.
        let mut outside = patch();
        let alpha: Vec<f32> = (0..200 * 200)
            .map(|i| if i / 200 < 80 { 1.0 } else { 0.0 })
            .collect();
        smooth(&mut outside, &alpha, 1.0);
        assert_eq!(
            outside.planes[0][100 * 200 + 60],
            before.planes[0][100 * 200 + 60]
        );
        // The faint value the model gives where there is no person (30% skin likelihood) changes
        // nothing.
        let mut faint = patch();
        smooth(&mut faint, &vec![0.3; 200 * 200], 1.0);
        assert_eq!(faint.planes, before.planes);
        // Strength 0 changes nothing.
        let mut none = patch();
        smooth(&mut none, &vec![1.0; 200 * 200], 0.0);
        assert_eq!(none.planes, before.planes);
        // Half strength reduces only about half.
        let (mut half, mut full) = (patch(), patch());
        smooth(&mut half, &vec![1.0; 200 * 200], 0.5);
        smooth(&mut full, &vec![1.0; 200 * 200], 1.0);
        let moved = |image: &Image| level(image, 60, 100) - level(&before, 60, 100);
        assert!(
            (moved(&half) - moved(&full) / 2.0).abs() < 0.002,
            "{} vs {}",
            moved(&half),
            moved(&full)
        );
    }

    #[test]
    fn real_photos_show_skin_only_where_people_are() {
        // Runs only when the model and sample photos are present.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM");
        let Ok(mut finder) = load_finder() else {
            eprintln!("no skin model, skipping");
            return;
        };
        if !dir.exists() {
            eprintln!("no sample photos, skipping");
            return;
        }
        let share = |file: &str, finder: &mut SkinFinder| {
            let picture = crate::preview::thumbnail(&dir.join(file), 1024)
                .unwrap()
                .expect("no preview");
            let started = std::time::Instant::now();
            let low = finder.find(&picture).unwrap();
            eprintln!("{file}: skin detection {:.0?}", started.elapsed());
            assert_eq!((low.width, low.height), (SIDE, SIDE));
            low.logits.iter().filter(|&&v| v > 0.0).count() as f32 / low.logits.len() as f32
        };
        let person = share("10161107/DSC04367.ARW", &mut finder);
        let buildings = share("10361110/DSC04551.ARW", &mut finder);
        // A full-body photo, so skin (face and arms) is about 1% of the frame.
        assert!(
            (0.003..0.3).contains(&person),
            "skin ratio of the portrait {person}"
        );
        assert!(
            buildings < person / 3.0,
            "skin ratio of the building photo {buildings} (portrait: {person})"
        );
    }

    /// Leaves before/after images in `target/ui/` for checking by eye.
    /// `cargo test -- --ignored render_skin`
    #[test]
    #[ignore = "needs the model and sample photos, and leaves image files"]
    fn render_skin() {
        use crate::export::{Areas, Look};
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui");
        std::fs::create_dir_all(&out).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10161107");
        let finder = std::sync::Mutex::new(load_finder().unwrap());
        for file in ["DSC04290.ARW", "DSC04367.ARW"] {
            let loaded = crate::export::load(
                &dir.join(file),
                None,
                &crate::capture::Capture::off(),
                &|_| {},
            )
            .unwrap();
            let params = crate::edit::Params::default();
            let look = Look {
                skin: 1.0,
                skin_finder: Some(&finder),
                ..Look::new(&params)
            };
            let areas = loaded.areas(look).unwrap();
            let before = loaded.develop(&loaded.cam, Look::new(&params), &Areas::default());
            let started = std::time::Instant::now();
            let after = loaded.develop(&loaded.cam, look, &areas);
            eprintln!(
                "{file}: develop with skin retouch {:.2?}",
                started.elapsed()
            );
            // Crop 600×600 centred on a clear skin area and place before and after side by side
            // (searched in the top half of the photo).
            let alpha = crate::mask::upscale(areas.skin.as_ref().unwrap(), &before, false);
            let (w, h) = (before.width, before.height);
            let (mut sx, mut sy, mut total) = (0.0f64, 0.0f64, 0.0f64);
            for (i, a) in alpha.iter().enumerate().take(w * h / 2) {
                if *a > 0.8 {
                    sx += (i % w) as f64 * *a as f64;
                    sy += (i / w) as f64 * *a as f64;
                    total += *a as f64;
                }
            }
            assert!(total > 0.0, "{file}: no skin found");
            let (cx, cy) = ((sx / total) as usize, (sy / total) as usize);
            let side = 600;
            let (x0, y0) = (
                cx.saturating_sub(side / 2).min(w - side),
                cy.saturating_sub(side / 2).min(h - side),
            );
            let mut rgb = vec![0u8; side * 2 * side * 3];
            for y in 0..side {
                for x in 0..side * 2 {
                    let image = if x < side { &before } else { &after };
                    let i = (y0 + y) * w + x0 + x % side;
                    for c in 0..3 {
                        rgb[(y * side * 2 + x) * 3 + c] =
                            (image.planes[c][i].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                    }
                }
            }
            image::save_buffer(
                out.join(format!("skin-{}.png", file.trim_end_matches(".ARW"))),
                &rgb,
                (side * 2) as u32,
                side as u32,
                image::ColorType::Rgb8,
            )
            .unwrap();
        }
    }
}
