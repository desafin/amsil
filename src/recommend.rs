//! 프리셋 추천: CLIP으로 사진이 무엇을 찍은 것인지 가늠한다.
//!
//! 사진을 CLIP 이미지 인코더에 넣어 얻은 벡터를, 분류마다 미리 계산해 둔 문장 벡터
//! (`assets/clip/categories.txt`)와 견준다. 가장 가까운 분류가 답이다.

use crate::Result;
use crate::lang::{t, text};
use crate::preset::Preset;
use crate::preview::Pixels;
use ort::session::Session;
use ort::value::TensorRef;
use std::path::Path;

/// 모델 폴더 안에서 분류 모델의 위치.
pub const MODEL: &str = "clip-vit-b32/vision_model_fp16.onnx";

/// 모델 입력의 한 변 (화소).
const SIDE: usize = 224;
/// 임베딩의 길이.
const EMBEDDING: usize = 512;
/// 이보다 확신이 낮으면 추천하지 않는다.
const CONFIDENT: f32 = 0.6;

/// 사진의 종류.
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
    /// 표(`categories.txt`)의 줄 순서와 같다.
    pub const ALL: [Category; 6] = [
        Category::Food,
        Category::Portrait,
        Category::Landscape,
        Category::Night,
        Category::Architecture,
        Category::Other,
    ];

    /// 표와 프리셋 파일의 `category`에 쓰는 이름.
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

    /// 화면에 보이는 이름.
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

/// 사진의 종류에 대한 짐작과 그 확신(0~1).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Guess {
    pub category: Category,
    pub confidence: f32,
}

impl Guess {
    /// 믿고 보여 줄 만큼 확실한가.
    pub fn is_confident(&self) -> bool {
        self.confidence >= CONFIDENT
    }
}

/// 짐작에 맞는 프리셋. 확신이 낮거나 그 분류의 프리셋이 없으면 추천하지 않는다.
pub fn preset_for<'a>(presets: &'a [Preset], guess: &Guess) -> Option<&'a Preset> {
    presets
        .iter()
        .find(|preset| preset.category.as_deref() == Some(guess.category.id()))
        .filter(|_| guess.is_confident())
}

/// 분류마다의 문장 벡터. 저장소에 넣어 둔 표를 읽는다.
fn text_table() -> Vec<(Category, Vec<f32>)> {
    let rows: Vec<Vec<f32>> = include_str!("../assets/clip/categories.txt")
        .lines()
        .filter(|line| !line.starts_with('#') && !line.trim().is_empty())
        .map(|line| {
            line.split_whitespace()
                .skip(1)
                .map(|value| value.parse().expect("분류 표의 숫자가 잘못됐다"))
                .collect()
        })
        .collect();
    assert_eq!(rows.len(), Category::ALL.len(), "분류 표의 줄 수가 다르다");
    assert!(
        rows.iter().all(|row| row.len() == EMBEDDING),
        "분류 표의 벡터 길이가 다르다"
    );
    Category::ALL.into_iter().zip(rows).collect()
}

/// 그림의 가운데 정사각형을 224×224로 맞추고 CLIP의 평균·표준편차로 정규화한다 (채널 우선).
fn preprocess(pixels: &Pixels) -> Vec<f32> {
    const MEAN: [f32; 3] = [0.48145466, 0.4578275, 0.40821073];
    // CLIP의 값(0.26862954, 0.26130258, 0.27577711)을 f32가 담을 수 있는 자리까지 적는다.
    const STD: [f32; 3] = [0.26862954, 0.2613026, 0.2757771];
    let (w, h) = (pixels.width, pixels.height);
    let square = w.min(h);
    let (left, top) = ((w - square) / 2, (h - square) / 2);
    let mut input = vec![0.0; 3 * SIDE * SIDE];
    for oy in 0..SIDE {
        // 출력 화소 하나가 덮는 원본 영역의 평균. 원본이 더 작으면 가장 가까운 화소 하나다.
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

/// 이미지 벡터와 가장 가까운 분류를 고른다. 확신은 CLIP과 같은 방식(코사인 유사도 × 100의 소프트맥스)이다.
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
        .expect("분류가 하나도 없다");
    let total: f32 = logits
        .iter()
        .map(|logit| (logit - logits[best]).exp())
        .sum();
    Guess {
        category: table[best].0,
        confidence: 1.0 / total,
    }
}

/// 사진 분류기 (CLIP ViT-B/32 이미지 인코더).
pub struct Classifier {
    session: Session,
    table: Vec<(Category, Vec<f32>)>,
}

impl Classifier {
    // ponytail: CPU로만 돌린다 (사진 한 장에 수십 ms). GPU는 노이즈 제거가 쓰게 둔다.
    pub fn load(model: &Path) -> Result<Self> {
        Ok(Self {
            session: crate::gpu::cpu_session(model)?,
            table: text_table(),
        })
    }

    /// 그림이 무엇을 찍은 것인지 가늠한다. 긴 변이 300화소쯤 되는 작은 그림이면 충분하다.
    pub fn classify(&mut self, pixels: &Pixels) -> Result<Guess> {
        let input = preprocess(pixels);
        let input = TensorRef::from_array_view(([1, 3, SIDE, SIDE], input.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, embedding) = outputs["image_embeds"].try_extract_tensor::<f32>()?;
        Ok(pick(embedding, &self.table))
    }
}

/// 모델 폴더에서 분류 모델을 읽는다. 모델 파일이 없으면 받는 방법을 알려 준다.
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

    /// 한 가지 색으로 채운 그림.
    fn flat(width: usize, height: usize, rgb: [u8; 3]) -> Pixels {
        Pixels {
            width,
            height,
            rgba: [rgb[0], rgb[1], rgb[2], 255].repeat(width * height),
        }
    }

    /// `hot`번째만 1인 단위 벡터.
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
        // CLIP의 평균과 표준편차로 정규화한다: (1 - 0.48145466) / 0.26862954
        assert!((white[0] - 1.930_336).abs() < 1e-4, "{}", white[0]);
        assert!((white[SIDE * SIDE] - 2.074_875).abs() < 1e-4);
        assert!((white[2 * SIDE * SIDE] - 2.145_897).abs() < 1e-4);

        // 가로로 긴 그림: 가운데 정사각형만 쓴다. 왼쪽 끝의 빨간 띠(가운데 300 밖)는 들어가지 않는다.
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
            "가운데 정사각형(74..374) 밖은 잘려야 한다"
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
        // 풍경 쪽으로 뚜렷이 기운 임베딩
        let mut clear = axis(2);
        clear[0] = 0.2;
        let guess = pick(&clear, &table());
        assert_eq!(guess.category, Category::ALL[2]);
        assert!(guess.confidence > 0.99, "{}", guess.confidence);

        // 두 분류의 한가운데: 어느 쪽인지 알 수 없다.
        let mut torn = axis(0);
        torn[1] = 1.0;
        let guess = pick(&torn, &table());
        assert!(
            (guess.confidence - 0.5).abs() < 0.01,
            "{}",
            guess.confidence
        );

        // 크기가 달라도(정규화하지 않은 임베딩) 결과는 같다.
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
        // 모델과 샘플 사진이 있을 때만 돈다.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM");
        let Ok(mut classifier) = load_classifier() else {
            eprintln!("사진 분류 모델이 없어 건너뜀");
            return;
        };
        if !dir.exists() {
            eprintln!("샘플 사진이 없어 건너뜀");
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
                .expect("미리보기가 없다");
            let started = std::time::Instant::now();
            let guess = classifier.classify(&pixels).unwrap();
            eprintln!(
                "{file}: {guess:?}, 분류 {:.0}ms",
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
            "확신이 낮으면 추천하지 않는다"
        );
        assert_eq!(
            preset(Category::Architecture, 0.99),
            None,
            "그 분류의 프리셋이 없으면 추천하지 않는다"
        );
    }
}
