//! 인물 피부 보정 (실험 기능): 피부를 자동으로 찾아, 그 안에서만 얼룩과 고르지 않은 톤을 누그러뜨린다.
//!
//! 피부를 찾는 일은 MediaPipe selfie multiclass 모델이 한다 (256×256 격자). 보정은 신경망이 아니라
//! 흐림 두 번의 차이로 한다: 가는 결(모공, 잔털)과 큰 명암(얼굴의 입체감)은 남기고, 그 사이 크기의
//! 얼룩만 줄인다. 눈·입술 같은 뚜렷한 윤곽은 차이가 커서 건드리지 않는다.

use crate::develop::box_blur;
use crate::lang::t;
use crate::mask::Low;
use crate::preview::Pixels;
use crate::{Image, Result};
use ort::session::Session;
use ort::value::TensorRef;
use rayon::prelude::*;
use std::path::Path;

/// 모델 폴더 안에서 피부를 찾는 모델의 위치.
pub const MODEL: &str = "selfie-multiclass/model.onnx";

/// 모델 입력과 출력 격자의 한 변 (화소).
const SIDE: usize = 256;
/// 모델이 화소마다 내는 분류의 수: 배경, 머리카락, 몸의 피부, 얼굴의 피부, 옷, 그 밖.
const CLASSES: usize = 6;
/// 그 가운데 피부인 것.
const SKIN: [usize; 2] = [2, 3];

/// 그림 전체를 256×256으로 맞춘다 (비율을 지키지 않고 늘이거나 줄인다. 모델이 그렇게 배웠다).
/// 값은 0~1이고 화소마다 R, G, B 순서로 늘어놓는다.
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

/// 화소 하나의 분류 점수(로짓 6개)에서 "피부인가"의 로짓을 구한다.
fn skin_logit(scores: &[f32]) -> f32 {
    let top = scores.iter().copied().fold(f32::MIN, f32::max);
    let total: f32 = scores.iter().map(|score| (score - top).exp()).sum();
    let skin: f32 = SKIN.iter().map(|&class| (scores[class] - top).exp()).sum();
    let p = (skin / total).clamp(1e-5, 1.0 - 1e-5);
    (p / (1.0 - p)).ln()
}

/// 사진에서 피부(얼굴과 몸)를 찾는 모델.
pub struct SkinFinder {
    session: Session,
}

impl SkinFinder {
    pub fn load(model: &Path) -> Result<Self> {
        Ok(Self {
            session: crate::gpu::cpu_session(model)?,
        })
    }

    /// 피부일 가능성을 256×256 격자의 로짓으로 돌려준다. 격자는 그림 전체를 덮는다.
    pub fn find(&mut self, picture: &Pixels) -> Result<Low> {
        let input = preprocess(picture);
        let input = TensorRef::from_array_view(([1, SIDE, SIDE, 3], input.as_slice()))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let (_, scores) = outputs[0].try_extract_tensor::<f32>()?;
        if scores.len() != SIDE * SIDE * CLASSES {
            return Err(t(
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

/// 모델 폴더에서 피부를 찾는 모델을 읽는다. 모델 파일이 없으면 받는 방법을 알려 준다.
pub fn load_finder() -> Result<SkinFinder> {
    match missing() {
        Some(problem) => Err(problem.into()),
        None => SkinFinder::load(&crate::gpu::model_dir().join(MODEL)),
    }
}

/// 피부를 찾는 모델이 없으면 그 사실과 받는 방법을 알리는 문구. 있으면 None.
pub fn missing() -> Option<String> {
    let path = crate::gpu::model_dir().join(MODEL);
    if path.exists() {
        return None;
    }
    let path = path.display();
    Some(t(
        format!(
            "피부 보정 모델이 없습니다: {path}\nfetch-models.sh를 실행해 받으세요 (실행 파일 옆이나 저장소의 scripts 폴더에 있습니다)."
        ),
        format!(
            "The skin model is missing: {path}\nRun fetch-models.sh to download it (next to the program, or in the scripts folder of the repository)."
        ),
    ))
}

/// 피부 영역(`alpha`, 0~1) 안에서 중간 크기의 얼룩을 `strength`(0~1)만큼 누그러뜨린다.
// ponytail: 흐림 두 번의 차이를 빼는 단순한 방식이다. 뚜렷한 윤곽 바로 옆에는 옅은 번짐이 남을 수 있다.
// 거슬리면 윤곽을 보존하는 필터(guided filter 등)로 바꾼다.
pub fn smooth(image: &mut Image, alpha: &[f32], strength: f32) {
    /// 얼룩으로 보는 가장 큰 크기 (피부 넓이의 제곱근에 대한 비율).
    const COARSE: f32 = 0.06;
    /// 그대로 남기는 가는 결의 크기 (큰 쪽의 몇 분의 일).
    const FINE: f32 = 1.0 / 8.0;
    /// 이보다 큰 밝기 차이는 얼룩이 아니라 윤곽(눈, 입술, 머리카락)으로 본다.
    const EDGE: f32 = 0.06;

    let (w, h) = (image.width, image.height);
    // 모델은 사람이 없는 곳에도 옅은 값(몇 %)을 낸다. 확실한 곳만 피부로 친다: 0.4 아래는 0, 0.8 위는 1.
    let alpha: Vec<f32> = alpha
        .par_iter()
        .map(|alpha| ((alpha - 0.4) / 0.4).clamp(0.0, 1.0))
        .collect();
    let skin: f32 = alpha.par_iter().sum::<f32>() / alpha.len().max(1) as f32;
    if strength <= 0.0 || skin <= 0.0 {
        return;
    }
    // 얼굴이 크게 찍혔으면 얼룩도 크다. 피부가 차지하는 넓이로 크기를 가늠한다.
    let coarse = (COARSE * skin.sqrt() * w.min(h) as f32).round().max(2.0) as usize;
    let fine = (coarse as f32 * FINE).round().max(1.0) as usize;
    // 중간 크기의 변화만 남긴 것: 가늘게 흐린 것 - 굵게 흐린 것
    let band = |plane: &[f32]| -> Vec<f32> {
        let mut band = box_blur(plane, w, h, fine, 2);
        let wide = box_blur(plane, w, h, coarse, 2);
        band.par_iter_mut()
            .zip(wide.par_iter())
            .for_each(|(band, wide)| *band -= wide);
        band
    };
    // 밝기에서 변화가 작은 곳만 고른다 (세 채널에 같은 정도로 적용해야 색이 틀어지지 않는다).
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
        // 왼쪽 절반은 희고 오른쪽 절반은 검은, 가로로 긴 그림
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
            "자르지 않고 늘인다: 가운데까지가 흰색"
        );
        assert_eq!(pixel(128, 0), [0.0; 3]);
        assert_eq!(pixel(255, 255), [0.0; 3]);
    }

    #[test]
    fn skin_is_face_plus_body_against_everything_else() {
        // 순서: 배경, 머리카락, 몸의 피부, 얼굴의 피부, 옷, 그 밖
        assert!(skin_logit(&[0.0, 0.0, 0.0, 9.0, 0.0, 0.0]) > 4.0, "얼굴");
        assert!(skin_logit(&[0.0, 0.0, 9.0, 0.0, 0.0, 0.0]) > 4.0, "몸");
        assert!(skin_logit(&[9.0, 0.0, 0.0, 0.0, 0.0, 0.0]) < -4.0, "배경");
        assert!(
            skin_logit(&[0.0, 9.0, 0.0, 0.0, 0.0, 0.0]) < -4.0,
            "머리카락"
        );
        // 얼굴인지 몸인지는 갈려도 피부인 것은 분명한 경우
        assert!(skin_logit(&[0.0, 0.0, 9.0, 9.0, 0.0, 0.0]) > 4.0);
        // 반반이면 0
        assert!(skin_logit(&[5.0, 0.0, -20.0, 5.0, -20.0, -20.0]).abs() < 0.1);
        assert!(skin_logit(&[-50.0, -50.0, 50.0, 50.0, -50.0, -50.0]).is_finite());
    }

    /// 시험용 얼굴 피부: 고른 바탕에 가는 결, 중간 크기의 얼룩, 뚜렷한 윤곽이 있다.
    fn patch() -> Image {
        let (w, h) = (200, 200);
        let plane: Vec<f32> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f32, (i / w) as f32);
                // 가는 결: 한 화소마다 번갈아 ±0.01
                let grain = if (i % w + i / w) % 2 == 0 {
                    0.01
                } else {
                    -0.01
                };
                // 얼룩: (60, 100)에 반지름 5쯤, 깊이 0.04의 어두운 점
                let d2 = (x - 60.0).powi(2) + (y - 100.0).powi(2);
                let blotch = -0.04 * (-d2 / (2.0 * 3.0 * 3.0)).exp();
                // 윤곽: 150열부터 훨씬 어둡다 (눈썹이나 머리카락)
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

    /// 한 점 둘레(반지름 1)의 평균. 가는 결을 지우고 그 자리의 밝기를 본다.
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

        // 얼룩: 둘레(60, 70)와 견준 깊이가 절반 아래로 준다.
        let depth = |image: &Image| level(image, 60, 70) - level(image, 60, 100);
        assert!(depth(&before) > 0.03, "{}", depth(&before));
        assert!(
            depth(&after) < depth(&before) * 0.5,
            "{} → {}",
            depth(&before),
            depth(&after)
        );
        // 가는 결: 이웃한 화소의 차이(0.02)가 거의 그대로다.
        let grain =
            |image: &Image| (image.planes[1][30 * 200 + 30] - image.planes[1][30 * 200 + 31]).abs();
        assert!(grain(&after) > grain(&before) * 0.9, "{}", grain(&after));
        // 윤곽: 149열과 150열의 차이(0.3)가 거의 그대로다.
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
        // 피부가 아닌 곳은 그대로다.
        let mut outside = patch();
        let alpha: Vec<f32> = (0..200 * 200)
            .map(|i| if i / 200 < 80 { 1.0 } else { 0.0 })
            .collect();
        smooth(&mut outside, &alpha, 1.0);
        assert_eq!(
            outside.planes[0][100 * 200 + 60],
            before.planes[0][100 * 200 + 60]
        );
        // 모델이 사람이 없는 곳에 내는 옅은 값(피부일 가능성 30%)으로는 아무것도 바뀌지 않는다.
        let mut faint = patch();
        smooth(&mut faint, &vec![0.3; 200 * 200], 1.0);
        assert_eq!(faint.planes, before.planes);
        // 세기가 0이면 아무것도 바뀌지 않는다.
        let mut none = patch();
        smooth(&mut none, &vec![1.0; 200 * 200], 0.0);
        assert_eq!(none.planes, before.planes);
        // 세기가 절반이면 절반쯤만 준다.
        let (mut half, mut full) = (patch(), patch());
        smooth(&mut half, &vec![1.0; 200 * 200], 0.5);
        smooth(&mut full, &vec![1.0; 200 * 200], 1.0);
        let moved = |image: &Image| level(image, 60, 100) - level(&before, 60, 100);
        assert!(
            (moved(&half) - moved(&full) / 2.0).abs() < 0.002,
            "{} 대 {}",
            moved(&half),
            moved(&full)
        );
    }

    #[test]
    fn real_photos_show_skin_only_where_people_are() {
        // 모델과 샘플 사진이 있을 때만 돈다.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM");
        let Ok(mut finder) = load_finder() else {
            eprintln!("피부를 찾는 모델이 없어 건너뜀");
            return;
        };
        if !dir.exists() {
            eprintln!("샘플 사진이 없어 건너뜀");
            return;
        }
        let share = |file: &str, finder: &mut SkinFinder| {
            let picture = crate::preview::thumbnail(&dir.join(file), 1024)
                .unwrap()
                .expect("미리보기가 없다");
            let started = std::time::Instant::now();
            let low = finder.find(&picture).unwrap();
            eprintln!("{file}: 피부 찾기 {:.0?}", started.elapsed());
            assert_eq!((low.width, low.height), (SIDE, SIDE));
            low.logits.iter().filter(|&&v| v > 0.0).count() as f32 / low.logits.len() as f32
        };
        let person = share("10161107/DSC04367.ARW", &mut finder);
        let buildings = share("10361110/DSC04551.ARW", &mut finder);
        // 전신 사진이라 피부(얼굴과 팔)는 화면의 1%쯤이다.
        assert!(
            (0.003..0.3).contains(&person),
            "인물 사진의 피부 비율 {person}"
        );
        assert!(
            buildings < person / 3.0,
            "건물 사진의 피부 비율 {buildings} (인물 사진은 {person})"
        );
    }

    /// 눈으로 확인하기 위한 전후 그림을 `target/ui/`에 남긴다. `cargo test -- --ignored render_skin`
    #[test]
    #[ignore = "모델과 샘플 사진이 필요하고, 그림 파일을 남기는 용도"]
    fn render_skin() {
        use crate::export::{Areas, Look};
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui");
        std::fs::create_dir_all(&out).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10161107");
        let finder = std::sync::Mutex::new(load_finder().unwrap());
        for file in ["DSC04290.ARW", "DSC04367.ARW"] {
            let loaded = crate::export::load(&dir.join(file), None, &|_| {}).unwrap();
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
            eprintln!("{file}: 피부 보정을 켠 현상 {:.2?}", started.elapsed());
            // 피부가 뚜렷한 곳을 가운데로 600×600을 잘라 전후를 나란히 놓는다 (사진의 위쪽 절반에서 찾는다).
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
            assert!(total > 0.0, "{file}: 피부를 찾지 못했다");
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
