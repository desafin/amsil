//! 화면에 보여 줄 이미지: 썸네일과 미리보기.

use crate::{Image, Result, raw};
use rayon::prelude::*;
use std::path::Path;

/// 화면에 그릴 8bit RGBA 이미지.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Pixels {
    /// 긴 변이 `max_side`를 넘으면 그 안에 들어오도록 줄인 사본을 돌려준다. 넘지 않으면 None.
    pub fn fit_within(&self, max_side: usize) -> Option<Pixels> {
        let (w, h) = (self.width, self.height);
        let long = w.max(h);
        if long <= max_side {
            return None;
        }
        let (out_w, out_h) = ((w * max_side / long).max(1), (h * max_side / long).max(1));
        let mut rgba = vec![0u8; out_w * out_h * 4];
        rgba.par_chunks_mut(out_w * 4)
            .enumerate()
            .for_each(|(oy, row)| {
                let (y0, y1) = (
                    oy * h / out_h,
                    ((oy + 1) * h / out_h).max(oy * h / out_h + 1),
                );
                for (ox, px) in row.chunks_mut(4).enumerate() {
                    let (x0, x1) = (
                        ox * w / out_w,
                        ((ox + 1) * w / out_w).max(ox * w / out_w + 1),
                    );
                    let count = ((x1 - x0) * (y1 - y0)) as u32;
                    for (c, out) in px.iter_mut().enumerate() {
                        let sum: u32 = (y0..y1)
                            .flat_map(|y| (x0..x1).map(move |x| (y * w + x) * 4 + c))
                            .map(|i| self.rgba[i] as u32)
                            .sum();
                        *out = ((sum + count / 2) / count) as u8;
                    }
                }
            });
        Some(Pixels {
            width: out_w,
            height: out_h,
            rgba,
        })
    }
}

/// 긴 변이 `max_side` 이하가 되도록 줄인다 (넓이 평균). 이미 작으면 크기를 바꾸지 않는다.
pub fn shrink(image: &Image, max_side: usize) -> Pixels {
    let (w, h) = (image.width, image.height);
    let long = w.max(h).max(1);
    let (out_w, out_h) = if long <= max_side {
        (w, h)
    } else {
        ((w * max_side / long).max(1), (h * max_side / long).max(1))
    };
    let mut rgba = vec![255u8; out_w * out_h * 4];
    rgba.par_chunks_mut(out_w * 4)
        .enumerate()
        .for_each(|(oy, row)| {
            // 출력 화소 하나가 덮는 원본 영역
            let (y0, y1) = (
                oy * h / out_h,
                ((oy + 1) * h / out_h).max(oy * h / out_h + 1),
            );
            for (ox, px) in row.chunks_mut(4).enumerate() {
                let (x0, x1) = (
                    ox * w / out_w,
                    ((ox + 1) * w / out_w).max(ox * w / out_w + 1),
                );
                let count = ((x1 - x0) * (y1 - y0)) as f32;
                for (c, plane) in image.planes.iter().enumerate() {
                    let sum: f32 = (y0..y1)
                        .map(|y| plane[y * w + x0..y * w + x1].iter().sum::<f32>())
                        .sum();
                    px[c] = ((sum / count).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                }
            }
        });
    Pixels {
        width: out_w,
        height: out_h,
        rgba,
    }
}

/// RAW 파일 안에 들어 있는 작은 JPEG로 썸네일을 만든다. 바로 세운 상태로 돌려준다. 없으면 None.
pub fn thumbnail(path: &Path, max_side: usize) -> Result<Option<Pixels>> {
    let Some(jpeg) = raw::embedded_preview(path)? else {
        return Ok(None);
    };
    let small = jpeg
        .thumbnail(max_side as u32, max_side as u32)
        .into_rgba8();
    Ok(Some(Pixels {
        width: small.width() as usize,
        height: small.height() as usize,
        rgba: small.into_raw(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(width: usize, height: usize, value: impl Fn(usize, usize) -> f32) -> Image {
        let plane: Vec<f32> = (0..width * height)
            .map(|i| value(i % width, i / width))
            .collect();
        Image {
            width,
            height,
            planes: [plane.clone(), plane.clone(), plane],
        }
    }

    #[test]
    fn shrinking_averages_the_pixels_it_merges() {
        // 0.0 1.0 | 0.2 0.2
        // 1.0 0.0 | 0.2 0.2
        let src = image(4, 2, |x, y| {
            if x >= 2 {
                0.2
            } else if (x + y) % 2 == 0 {
                0.0
            } else {
                1.0
            }
        });
        let out = shrink(&src, 2);
        assert_eq!((out.width, out.height), (2, 1));
        assert_eq!(out.rgba, [128, 128, 128, 255, 51, 51, 51, 255]);
    }

    #[test]
    fn shrinking_keeps_the_aspect_ratio() {
        let out = shrink(&image(6048, 4032, |_, _| 0.5), 2048);
        assert_eq!((out.width, out.height), (2048, 1365));
        assert!(out.rgba.chunks(4).all(|px| px == [128, 128, 128, 255]));
    }

    #[test]
    fn a_small_image_is_not_enlarged() {
        let out = shrink(&image(3, 2, |x, _| x as f32 / 2.0), 100);
        assert_eq!((out.width, out.height), (3, 2));
        assert_eq!(
            &out.rgba[..12],
            [0, 0, 0, 255, 128, 128, 128, 255, 255, 255, 255, 255]
        );
    }

    #[test]
    fn pixels_larger_than_the_limit_are_reduced_by_averaging() {
        // 빨강 0·100 | 200·200 (녹색과 파랑은 0)
        let px = |r: u8| [r, 0, 0, 255];
        let pixels = Pixels {
            width: 4,
            height: 1,
            rgba: [px(0), px(100), px(200), px(200)].concat(),
        };
        let small = pixels.fit_within(2).unwrap();
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.rgba, [50, 0, 0, 255, 200, 0, 0, 255]);
    }

    #[test]
    fn pixels_within_the_limit_are_left_alone() {
        let pixels = Pixels {
            width: 4,
            height: 2,
            rgba: vec![7; 32],
        };
        assert!(pixels.fit_within(4).is_none());
        assert!(pixels.fit_within(100).is_none());
    }

    #[test]
    fn thumbnail_of_a_portrait_photo_is_upright() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        let thumb = thumbnail(&path, 160)
            .unwrap()
            .expect("A900 ARW에는 미리보기 JPEG가 들어 있다");
        assert_eq!(thumb.height, 160);
        assert!(
            thumb.width < thumb.height,
            "{}x{}",
            thumb.width,
            thumb.height
        );
        assert_eq!(thumb.rgba.len(), thumb.width * thumb.height * 4);
    }
}
