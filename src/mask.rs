//! AI 마스크: 사진에서 누른 자리의 물체를 골라내어, 그 영역에만 보정을 더한다 (SAM 2.1).
//!
//! 인코더는 사진마다 한 번 돌려 결과(`Embedding`)를 두고 쓰고, 디코더는 누를 때마다 돌린다.
//! 디코더가 내는 마스크는 256×256 격자라서, 그림의 윤곽을 따라가며 원래 크기로 키운다.

use crate::edit::Params;
use crate::lang::t;
use crate::preview::Pixels;
use crate::{Image, Result};
use ort::session::Session;
use ort::value::TensorRef;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// 모델 폴더 안에서 마스크 모델(인코더와 디코더)이 든 폴더.
pub const MODEL: &str = "mask-object-sam21-small";

/// 인코더 입력의 한 변 (화소). 넣는 그림의 긴 변은 이 값 이하여야 한다.
pub const INPUT: usize = 1024;
/// 디코더가 내는 마스크 격자의 한 변. 입력의 4분의 1이다.
const GRID: usize = 256;

/// 사진에서 누른 자리. 바로 세운 사진의 왼쪽 위가 (0, 0), 오른쪽 아래가 (1, 1)이다.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Point {
    pub x: f32,
    pub y: f32,
    /// 이 자리를 고르는지(참), 빼는지(거짓).
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub include: bool,
}

fn yes() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

/// 마스크 하나: 누른 자리들로 고른 영역과, 그 영역에만 더할 보정.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Mask {
    pub points: Vec<Point>,
    /// 고른 영역의 바깥에 보정을 더한다.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub invert: bool,
    #[serde(skip_serializing_if = "Params::is_neutral")]
    pub adjust: Params,
}

/// 인코더에 넣을 입력을 만든다: 그림을 왼쪽 위에 놓고 ImageNet 평균·표준편차로 정규화한다 (채널 우선).
/// 남는 자리는 0이다. 그림의 크기도 함께 돌려준다.
fn preprocess(picture: &Pixels) -> (Vec<f32>, (usize, usize)) {
    const MEAN: [f32; 3] = [123.675, 116.28, 103.53];
    const STD: [f32; 3] = [58.395, 57.12, 57.375];
    let (w, h) = (picture.width, picture.height);
    assert!(
        w <= INPUT && h <= INPUT,
        "마스크 모델에는 긴 변 {INPUT} 이하의 그림을 넣는다"
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

/// 사진 한 장의 인코더 출력 (약 17MB). 누를 때마다 다시 쓴다.
pub struct Embedding {
    embed: Vec<f32>,
    feats0: Vec<f32>,
    feats1: Vec<f32>,
    /// 인코더에 넣은 그림의 크기.
    size: (usize, usize),
}

/// 디코더가 낸 저해상도 마스크에서 사진이 차지하는 부분. 값은 로짓이다 (0보다 크면 고른 영역).
#[derive(Clone)]
pub struct Low {
    pub width: usize,
    pub height: usize,
    pub logits: Vec<f32>,
}

/// 누른 자리의 물체를 골라내는 모델 (SAM 2.1 Hiera Small).
pub struct Segmenter {
    encoder: Session,
    decoder: Session,
}

impl Segmenter {
    // ponytail: CPU로만 돌린다 (인코더가 사진당 1~2초). 누르고 기다리는 시간이 거슬리면 GPU로 옮긴다.
    pub fn load(dir: &Path) -> Result<Self> {
        Ok(Self {
            encoder: crate::gpu::cpu_session(&dir.join("encoder.onnx"))?,
            decoder: crate::gpu::cpu_session(&dir.join("decoder.onnx"))?,
        })
    }

    /// 사진을 인코더에 넣는다. `picture`는 보정하지 않은 사진을 긴 변 1024 이하로 줄인 것이다.
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

    /// 누른 자리들로 영역을 고른다. 디코더가 내는 후보 가운데 모델이 가장 믿는 것을 쓴다.
    // ponytail: 누른 자리를 한꺼번에 넣고 한 번만 돌린다. 앞선 결과를 되먹여 다듬는 방식은 쓰지 않는다.
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
        // 모델은 맨 끝에 (0, 0)에 레이블 -1인 채움 점을 하나 붙여 받는다.
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
                t(
                    "마스크 모델이 결과를 내지 않았습니다",
                    "The mask model returned nothing",
                )
            })?;
        let grid = masks
            .get(best * GRID * GRID..(best + 1) * GRID * GRID)
            .ok_or_else(|| {
                t(
                    "마스크 모델의 출력 크기가 예상과 다릅니다",
                    "Unexpected mask model output size",
                )
            })?;
        // 격자에서 사진이 차지하는 왼쪽 위 부분만 남긴다.
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

/// 모델 폴더에서 마스크 모델을 읽는다. 모델 파일이 없으면 받는 방법을 알려 준다.
pub fn load_segmenter() -> Result<Segmenter> {
    match missing() {
        Some(problem) => Err(problem.into()),
        None => Segmenter::load(&crate::gpu::model_dir().join(MODEL)),
    }
}

/// 마스크 모델이 없으면 그 사실과 받는 방법을 알리는 문구. 있으면 None.
pub fn missing() -> Option<String> {
    let dir = crate::gpu::model_dir().join(MODEL);
    if dir.join("encoder.onnx").exists() && dir.join("decoder.onnx").exists() {
        return None;
    }
    let dir = dir.display();
    Some(t(
        format!(
            "마스크 모델이 없습니다: {dir}\nfetch-models.sh를 실행해 받으세요 (실행 파일 옆이나 저장소의 scripts 폴더에 있습니다)."
        ),
        format!(
            "The mask model is missing: {dir}\nRun fetch-models.sh to download it (next to the program, or in the scripts folder of the repository)."
        ),
    ))
}

/// 저해상도 마스크를 `guide`의 크기로 키운다. 결과는 0~1이고, `invert`면 뒤집는다.
/// 둘레 네 격자점을 섞을 때 밝기가 비슷한 쪽을 더 따르게 해서, 경계가 그림의 윤곽에 붙는다.
pub fn upscale(low: &Low, guide: &Image, invert: bool) -> Vec<f32> {
    /// 밝기 차이가 이쯤 나면 다른 물체로 본다.
    const SIGMA: f32 = 0.1;
    let (w, h) = (guide.width, guide.height);
    let luma = |i: usize| {
        0.2126 * guide.planes[0][i] + 0.7152 * guide.planes[1][i] + 0.0722 * guide.planes[2][i]
    };
    // 그림의 한 화소가 격자에서 얼마만큼인지 (양 끝을 서로 맞춘다)
    let step = |low: usize, full: usize| {
        if full > 1 {
            (low - 1) as f32 / (full - 1) as f32
        } else {
            0.0
        }
    };
    let (step_x, step_y) = (step(low.width, w), step(low.height, h));
    // 격자점이 그림에서 놓이는 자리
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
            // 네 점이 모두 전혀 다른 밝기면 밝기를 따지지 않고 섞는다.
            let logit = if total > 1e-6 { sum / total } else { plain };
            let inside = 1.0 / (1.0 + (-logit).exp());
            *out = if invert { 1.0 - inside } else { inside };
        }
    });
    alpha
}

/// `base`를 `alpha`만큼 `other` 쪽으로 옮긴다 (화소마다).
pub fn blend(base: &mut Image, other: &Image, alpha: &[f32]) {
    for (to, from) in base.planes.iter_mut().zip(&other.planes) {
        to.par_iter_mut()
            .zip(from.par_iter())
            .zip(alpha.par_iter())
            .for_each(|((to, from), alpha)| *to += (from - *to) * alpha);
    }
}

/// 고른 영역을 눈에 띄게 붉게 물들인다 (화면에서 영역을 확인할 때).
pub fn tint(image: &mut Image, alpha: &[f32]) {
    /// 가장 진한 곳에서 붉은색이 차지하는 정도.
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

    /// 왼쪽 `split`열까지는 `left`, 그 오른쪽은 `right`인 그림 (세 채널 같은 값).
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
        // ImageNet 평균과 표준편차: (128 - 123.675) / 58.395
        assert!((input[0] - 0.074_064).abs() < 1e-4, "{}", input[0]);
        assert!((input[INPUT * INPUT] - 0.205_182).abs() < 1e-4);
        assert!((input[2 * INPUT * INPUT] - 0.426_492).abs() < 1e-4);
        assert!(
            (input[511 * INPUT + 1023] - 0.074_064).abs() < 1e-4,
            "그림의 마지막 화소"
        );
        assert_eq!(input[512 * INPUT], 0.0, "그림 아래는 0으로 둔다");
    }

    #[test]
    fn a_picture_larger_than_the_input_is_refused() {
        // 넣기 전에 긴 변을 1024 이하로 줄이는 것은 부르는 쪽의 일이다.
        let result = std::panic::catch_unwind(|| preprocess(&flat(2000, 100, 0)));
        assert!(result.is_err());
    }

    #[test]
    fn upscaling_follows_the_edges_of_the_picture() {
        // 저해상도 마스크: 왼쪽 절반이 고른 영역. 경계는 가운데(5열)쯤에 있다.
        let low = Low {
            width: 2,
            height: 2,
            logits: vec![8.0, -8.0, 8.0, -8.0],
        };
        // 밝기가 고른 그림에서는 경계가 가운데에서 부드럽게 넘어간다.
        let smooth = upscale(&low, &halves(10, 4, 10, 0.5, 0.5), false);
        assert_eq!(smooth.len(), 40);
        assert!(smooth[0] > 0.99 && smooth[9] < 0.01);
        assert!(smooth[4] > 0.5 && smooth[5] < 0.5, "{smooth:?}");

        // 그림의 윤곽이 7열에 있으면, 마스크의 경계도 거기에 붙는다.
        let snapped = upscale(&low, &halves(10, 4, 7, 0.9, 0.1), false);
        assert!(snapped[6] > 0.9, "윤곽 안쪽은 고른 영역: {snapped:?}");
        assert!(snapped[7] < 0.1, "윤곽 바깥은 아니다: {snapped:?}");

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
        assert!(!text.contains("invert"), "기본값은 적지 않는다: {text}");
        assert!(!text.contains("include = true"), "{text}");
        assert!(
            text.contains("include = false") && text.contains("exposure = 0.5"),
            "{text}"
        );
        assert_eq!(toml::from_str::<Mask>(&text).unwrap(), mask);
    }

    #[test]
    fn a_click_on_a_real_photo_selects_the_thing_under_it() {
        // 모델과 샘플 사진이 있을 때만 돈다.
        let photo =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110/DSC04551.ARW");
        let Ok(mut segmenter) = load_segmenter() else {
            eprintln!("마스크 모델이 없어 건너뜀");
            return;
        };
        if !photo.exists() {
            eprintln!("샘플 사진이 없어 건너뜀");
            return;
        }
        let picture = crate::preview::thumbnail(&photo, INPUT)
            .unwrap()
            .expect("미리보기가 없다");
        let started = std::time::Instant::now();
        let embedding = segmenter.embed(&picture).unwrap();
        let embedded = started.elapsed();
        // 가운데 유리 건물을 누른다.
        let tower = Point {
            x: 0.48,
            y: 0.35,
            include: true,
        };
        let started = std::time::Instant::now();
        let low = segmenter.select(&embedding, &[tower]).unwrap();
        eprintln!("인코더 {embedded:.2?}, 디코더 {:.2?}", started.elapsed());
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
        assert!(at(0.48, 0.35) > 0.0, "누른 자리는 고른 영역 안이다");
        assert!(at(0.1, 0.1) < 0.0, "하늘은 아니다");
        assert!(at(0.5, 0.95) < 0.0, "잔디밭도 아니다");
        let share =
            low.logits.iter().filter(|&&v| v > 0.0).count() as f32 / low.logits.len() as f32;
        assert!((0.03..0.3).contains(&share), "고른 넓이 {share}");

        // 누른 자리가 없으면 아무것도 고르지 않는다.
        let nothing = segmenter.select(&embedding, &[]).unwrap();
        assert!(nothing.logits.iter().all(|&v| v < 0.0));
    }
}
