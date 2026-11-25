//! Preset suggestions: CLIP guesses what a photo is of.
//!
//! The vector from running the photo through the CLIP image encoder is compared with text vectors
//! precomputed per category (`assets/clip/categories.txt`). The closest category is the answer.

use crate::Result;
use crate::lang::{t, text};
use crate::preset::Preset;
use crate::preview::Pixels;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;

/// Location of the classification model inside the model folder.
pub const MODEL: &str = "clip-vit-b32/vision_model_fp16.onnx";

/// One side of the model input (pixels).
const SIDE: usize = 224;
/// Length of the embedding.
const EMBEDDING: usize = 512;
/// Below this confidence, nothing is suggested.
const CONFIDENT: f32 = 0.6;

/// The kind of photo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Food,
    Portrait,
    Landscape,
    Night,
    Architecture,
    Other,
}

impl Category {
    /// Same as the row order of the table (`categories.txt`).
    pub const ALL: [Category; 6] = [
        Category::Food,
        Category::Portrait,
        Category::Landscape,
        Category::Night,
        Category::Architecture,
        Category::Other,
    ];

    /// Name used in the table and in the preset files' `category`.
    pub fn id(self) -> &'static str {
        match self {
            Category::Food => "food",
            Category::Portrait => "portrait",
            Category::Landscape => "landscape",
            Category::Night => "night",
            Category::Architecture => "architecture",
            Category::Other => "other",
        }
    }

    /// Name shown on screen.
    pub fn label(self) -> &'static str {
        match self {
            Category::Food => t("음식", "Food"),
            Category::Portrait => t("인물", "Portrait"),
            Category::Landscape => t("풍경", "Landscape"),
            Category::Night => t("야경", "Night"),
            Category::Architecture => t("건축", "Architecture"),
            Category::Other => t("기타", "Other"),
        }
    }
}

/// A guess at the kind of photo and its confidence (0–1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guess {
    pub category: Category,
    pub confidence: f32,
}

impl Guess {
    /// Whether it is certain enough to show with trust.
    pub fn is_confident(&self) -> bool {
        self.confidence >= CONFIDENT
    }
}

/// The preset matching the guess. Nothing is suggested if confidence is low or the category has no
/// preset.
pub fn preset_for<'a>(presets: &'a [Preset], guess: &Guess) -> Option<&'a Preset> {
    presets
        .iter()
        .find(|preset| preset.category.as_deref() == Some(guess.category.id()))
        .filter(|_| guess.is_confident())
}

/// Text vectors per category. Reads the table kept in the repository.
fn text_table() -> Vec<(Category, Vec<f32>)> {
    let rows: Vec<Vec<f32>> = include_str!("../assets/clip/categories.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .skip(1)
                .map(|value| {
                    value
                        .parse()
                        .expect("a number in the category table is invalid")
                })
                .collect()
        })
        .collect();
    assert_eq!(
        rows.len(),
        Category::ALL.len(),
        "the category table has the wrong row count"
    );
    assert!(
        rows.iter().all(|row| row.len() == EMBEDDING),
        "a vector in the category table has the wrong length"
    );
    Category::ALL.into_iter().zip(rows).collect()
}

/// Fits the image's centre square to 224×224 and normalizes with CLIP's mean and standard deviation
/// (channel-first).
fn preprocess(pixels: &Pixels) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
    // CLIP's values (0.26862954, 0.26130258, 0.27577711), written to the digits an f32 can hold.
    const STD: [f32; 3] = [0.26862954, 0.2613026, 0.2757771];
    let (w, h) = (pixels.width, pixels.height);
    let square = w.min(h);
    let (left, top) = ((w - square) / 2, (h - square) / 2);
    let mut input = vec![0.0; 3 * SIDE * SIDE];
    for oy in 0..SIDE {
        // Average of the source area one output pixel covers. If the source is smaller, the single
        // nearest pixel.
        let (y0, y1) = (
            oy * square / SIDE,
            ((oy + 1) * square / SIDE).max(oy * square / SIDE + 1),
        );
        for ox in 0..SIDE {
            let (x0, x1) = (
                ox * square / SIDE,
                ((ox + 1) * square / SIDE).max(ox * square / SIDE + 1),
            );
            let count = ((x1 - x0) * (y1 - y0)) as f32;
            for c in 0..3 {
                let sum: u32 = (y0..y1)
                    .flat_map(|y| (x0..x1).map(move |x| ((top + y) * w + left + x) * 4 + c))
                    .map(|i| pixels.rgba[i] as u32)
                    .sum();
                input[c * SIDE * SIDE + oy * SIDE + ox] =
                    (sum as f32 / count / 255.0 - MEAN[c]) / STD[c];
            }
        }
    }
    input
}

/// Picks the category closest to the image vector. Confidence works as in CLIP (softmax of cosine
/// similarity × 100).
fn pick(embedding: &[f32], table: &[(Category, Vec<f32>)]) -> Guess {
    let norm = embedding
        .iter()
        .map(|v| v * v)
        .sum::<f32>()
        .sqrt()
        .max(1e-12);
    let logits: Vec<f32> = table
        .iter()
        .map(|(_, text)| 100.0 * text.iter().zip(embedding).map(|(a, b)| a * b).sum::<f32>() / norm)
        .collect();
    let best = (0..logits.len())
        .max_by(|&a, &b| logits[a].total_cmp(&logits[b]))
        .expect("there are no categories");
    let total: f32 = logits
        .iter()
        .map(|logit| (logit - logits[best]).exp())
        .sum();
    Guess {
        category: table[best].0,
        confidence: 1.0 / total,
    }
}

/// Photo classifier (CLIP ViT-B/32 image encoder).
pub struct Classifier {
    session: Session,
    table: Vec<(Category, Vec<f32>)>,
}

impl Classifier {
    // ponytail: runs on CPU only (tens of ms per photo). The GPU is left to noise reduction.
    pub fn load(model: &Path) -> Result<Self> {
        Ok(Self {
            session: crate::gpu::cpu_session(model)?,
            table: text_table(),
        })
    }

    /// Guesses what the image is of. A small image with a long side of about 300 pixels is enough.
    pub fn classify(&mut self, pixels: &Pixels) -> Result<Guess> {
        let input = preprocess(pixels);
        let input = TensorRef::from_array_view(([1, 3, SIDE, SIDE], input.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, embedding) = outputs["image_embeds"].try_extract_tensor::<f32>()?;
        Ok(pick(embedding, &self.table))
    }
}

/// Loads the classification model from the model folder. If the model file is missing, says how to
/// get it.
pub fn load_classifier() -> Result<Classifier> {
    let path = crate::gpu::model_dir().join(MODEL);
    if !path.exists() {
        let path = path.display();
        return Err(text!(
            format!("사진 분류 모델이 없습니다: {path}\nfetch-models.sh를 실행해 받으세요 (실행 파일 옆이나 저장소의 scripts 폴더에 있습니다)."),
            format!("The photo classification model is missing: {path}\nRun fetch-models.sh to download it (next to the program, or in the scripts folder of the repository)."),
        )
        .into());
    }
    Classifier::load(&path)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An image filled with one colour.
    fn flat(width: usize, height: usize, rgb: [u8; 3]) -> Pixels {
        Pixels {
            width,
            height,
            rgba: [rgb[0], rgb[1], rgb[2], 255].repeat(width * height),
        }
    }

    /// A unit vector with 1 only at index `hot`.
    fn axis(hot: usize) -> Vec<f32> {
        (0..EMBEDDING).map(|i| (i == hot) as u8 as f32).collect()
    }

    fn table() -> Vec<(Category, Vec<f32>)> {
        Category::ALL
            .into_iter()
            .enumerate()
            .map(|(i, category)| (category, axis(i)))
            .collect()
    }

    #[test]
    fn the_input_is_the_centre_square_scaled_to_224_and_normalised() {
        let white = preprocess(&flat(448, 300, [255, 255, 255]));
        assert_eq!(white.len(), 3 * SIDE * SIDE);
        // Normalized with CLIP's mean and standard deviation: (1 - 0.48145466) / 0.26862954
        assert!((white[0] - 1.930_336).abs() < 1e-4, "{}", white[0]);
        assert!((white[SIDE * SIDE] - 2.074_875).abs() < 1e-4);
        assert!((white[2 * SIDE * SIDE] - 2.145_897).abs() < 1e-4);

        // A wide image: only the centre square is used. The red strip at the left edge (outside the
        // middle 300) is not included.
        let mut wide = flat(448, 300, [0, 0, 0]);
        for y in 0..300 {
            for x in 0..70 {
                wide.rgba[(y * 448 + x) * 4] = 255;
            }
        }
        let input = preprocess(&wide);
        let black = (0.0 - 0.48145466) / 0.26862954;
        assert!(
            input[..SIDE * SIDE]
                .iter()
                .all(|v| (v - black).abs() < 1e-4),
            "outside the centre square (74..374) must be cropped"
        );
    }

    #[test]
    fn a_small_picture_is_enlarged_rather_than_refused() {
        assert_eq!(
            preprocess(&flat(100, 60, [10, 20, 30])).len(),
            3 * SIDE * SIDE
        );
    }

    #[test]
    fn the_closest_category_wins_and_confidence_tells_how_clearly() {
        // An embedding clearly leaning toward landscape
        let mut clear = axis(2);
        clear[0] = 0.2;
        let guess = pick(&clear, &table());
        assert_eq!(guess.category, Category::ALL[2]);
        assert!(guess.confidence > 0.99, "{}", guess.confidence);

        // Right between two categories: no telling which.
        let mut torn = axis(0);
        torn[1] = 1.0;
        let guess = pick(&torn, &table());
        assert!(
            (guess.confidence - 0.5).abs() < 0.01,
            "{}",
            guess.confidence
        );

        // The result is the same even at a different magnitude (unnormalized embedding).
        let scaled: Vec<f32> = clear.iter().map(|v| v * 37.0).collect();
        assert_eq!(pick(&scaled, &table()).category, Category::ALL[2]);
    }

    #[test]
    fn the_committed_table_has_a_unit_vector_for_every_category() {
        let table = text_table();
        let categories: Vec<Category> = table.iter().map(|(category, _)| *category).collect();
        assert_eq!(categories, Category::ALL);
        for (category, vector) in &table {
            assert_eq!(vector.len(), EMBEDDING, "{category:?}");
            let norm: f32 = vector.iter().map(|v| v * v).sum::<f32>().sqrt();
            assert!((norm - 1.0).abs() < 1e-3, "{category:?}: {norm}");
        }
    }

    #[test]
    fn categories_name_the_preset_they_recommend() {
        assert_eq!(Category::Food.id(), "food");
        assert_eq!(Category::Night.id(), "night");
        assert_eq!(Category::Landscape.label(), "풍경");
        crate::lang::set(crate::lang::Lang::En);
        assert_eq!(Category::Landscape.label(), "Landscape");
        assert_eq!(Category::Architecture.label(), "Architecture");
    }

    #[test]
    fn real_photos_are_told_apart() {
        // Runs only when the model and sample photos are present.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM");
        let Ok(mut classifier) = load_classifier() else {
            eprintln!("no photo classification model, skipping");
            return;
        };
        if !dir.exists() {
            eprintln!("no sample photos, skipping");
            return;
        }
        for (file, expected) in [
            ("10361110/DSC04545.ARW", Category::Landscape),
            ("10361110/DSC04553.ARW", Category::Architecture),
            ("10361110/DSC04558.ARW", Category::Food),
            ("10161107/DSC04290.ARW", Category::Portrait),
        ] {
            let pixels = crate::preview::thumbnail(&dir.join(file), 448)
                .unwrap()
                .expect("no preview");
            let started = std::time::Instant::now();
            let guess = classifier.classify(&pixels).unwrap();
            eprintln!(
                "{file}: {guess:?}, classified in {:.0}ms",
                started.elapsed().as_secs_f32() * 1000.0
            );
            assert_eq!(guess.category, expected, "{file}: {guess:?}");
            assert!(guess.confidence >= CONFIDENT, "{file}: {guess:?}");
        }
    }

    #[test]
    fn a_confident_guess_maps_to_the_preset_of_that_category() {
        let presets = crate::preset::builtin();
        let preset = |category, confidence| {
            preset_for(
                &presets,
                &Guess {
                    category,
                    confidence,
                },
            )
            .map(|preset| preset.id.as_str())
        };
        assert_eq!(preset(Category::Food, 0.9), Some("food"));
        assert_eq!(preset(Category::Night, CONFIDENT), Some("night"));
        assert_eq!(
            preset(Category::Food, 0.5),
            None,
            "nothing is suggested when confidence is low"
        );
        assert_eq!(
            preset(Category::Architecture, 0.99),
            None,
            "nothing is suggested when the category has no preset"
        );
    }
}
