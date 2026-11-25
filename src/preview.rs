//! Images to show on screen: thumbnails and previews.

use crate::{Image, Result, raw};
use rayon::prelude::*;
use std::path::Path;

/// An 8-bit RGBA image to draw on screen.
#[derive(Debug, Clone, PartialEq)]
pub struct Pixels {
    pub width: usize,
    pub height: usize,
    pub rgba: Vec<u8>,
}

impl Pixels {
    /// If the long side exceeds `max_side`, returns a copy shrunk to fit within it. Otherwise None.
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

/// If the long side exceeds `max_side`, returns the image shrunk to fit within it (area average).
/// Otherwise None.
/// Used to make a small copy for previewing adjustments quickly.
pub fn downscale(image: &Image, max_side: usize) -> Option<Image> {
    let (w, h) = (image.width, image.height);
    let long = w.max(h);
    if long <= max_side {
        return None;
    }
    let (out_w, out_h) = ((w * max_side / long).max(1), (h * max_side / long).max(1));
    let planes = [0, 1, 2].map(|c| {
        let src = &image.planes[c];
        let mut out = vec![0f32; out_w * out_h];
        out.par_chunks_mut(out_w).enumerate().for_each(|(oy, row)| {
            let (y0, y1) = (
                oy * h / out_h,
                ((oy + 1) * h / out_h).max(oy * h / out_h + 1),
            );
            for (ox, value) in row.iter_mut().enumerate() {
                let (x0, x1) = (
                    ox * w / out_w,
                    ((ox + 1) * w / out_w).max(ox * w / out_w + 1),
                );
                let sum: f32 = (y0..y1)
                    .map(|y| src[y * w + x0..y * w + x1].iter().sum::<f32>())
                    .sum();
                *value = sum / ((x1 - x0) * (y1 - y0)) as f32;
            }
        });
        out
    });
    Some(Image {
        width: out_w,
        height: out_h,
        planes,
    })
}

/// Shrinks so the long side is at most `max_side` (area average). Leaves small images as they are.
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
            // The source area one output pixel covers
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

/// Makes a thumbnail from the small JPEG in the RAW file, returned upright. None if there is none.
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
        // Red 0·100 | 200·200 (green and blue are 0)
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
    fn downscaling_keeps_values_as_floats_and_averages_them() {
        let src = image(4, 2, |x, y| {
            if x >= 2 {
                0.2
            } else if (x + y) % 2 == 0 {
                0.0
            } else {
                1.0
            }
        });
        let small = downscale(&src, 2).unwrap();
        assert_eq!((small.width, small.height), (2, 1));
        assert_eq!(small.planes[1], [0.5, 0.2]);
        assert!(
            downscale(&src, 4).is_none() && downscale(&src, 99).is_none(),
            "used as is when small"
        );
    }

    #[test]
    fn thumbnail_of_a_portrait_photo_is_upright() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        let thumb = thumbnail(&path, 160)
            .unwrap()
            .expect("an A900 ARW contains a preview JPEG");
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
