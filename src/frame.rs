//! Frames: puts a margin around the photo and writes the shooting information.

use crate::Image;
use crate::lang::{Text, text};
use ab_glyph::{Font, FontRef, ScaleFont};
use serde::{Deserialize, Serialize};

/// The font built into the program (NanumGothic). Shared by the screen and frames.
pub const FONT: &[u8] = include_bytes!("../assets/fonts/NanumGothic-Regular.ttf");

/// Shooting information to write on the frame. Values missing from the photo are empty.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Shot {
    /// Make and model.
    pub camera: String,
    pub lens: Option<String>,
    pub focal_mm: Option<f32>,
    pub f_number: Option<f32>,
    /// Numerator and denominator of the exposure time (seconds).
    pub exposure: Option<(u32, u32)>,
    pub iso: Option<u32>,
    /// Capture time in EXIF format (`2026:11:07 13:27:19`).
    pub date: Option<String>,
}

/// The frame's shape.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub enum Layout {
    /// Only attaches a band below the photo.
    Band,
    /// A thin border on top and both sides, and a wide margin below for the text.
    Border,
    /// After adding the border, adds margin to match this ratio (width:height).
    Ratio(u32, u32),
}

/// The most elongated shape allowed as a ratio (3:1). Longer makes the image absurdly large.
const MAX_RATIO: u32 = 3;

impl std::str::FromStr for Layout {
    type Err = Text;

    fn from_str(text: &str) -> Result<Self, Text> {
        let ratio = || {
            let (w, h) = text.split_once(':')?;
            let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
            (w.min(h) > 0 && w.max(h) <= w.min(h) * MAX_RATIO).then_some(Layout::Ratio(w, h))
        };
        match text {
            "band" => Ok(Layout::Band),
            "border" => Ok(Layout::Border),
            _ => ratio().ok_or_else(|| {
                text!(
                    format!(
                        "모르는 프레임 모양: {text} (band, border 또는 4:5 같은 비율. 비율은 3:1보다 길쭉할 수 없습니다)"
                    ),
                    format!(
                        "Unknown frame layout: {text} (use band, border or a ratio such as 4:5, no more elongated than 3:1)"
                    ),
                )
            }),
        }
    }
}

impl std::fmt::Display for Layout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Layout::Band => write!(f, "band"),
            Layout::Border => write!(f, "border"),
            Layout::Ratio(w, h) => write!(f, "{w}:{h}"),
        }
    }
}

impl TryFrom<String> for Layout {
    type Error = Text;
    fn try_from(text: String) -> Result<Self, Text> {
        text.parse()
    }
}

impl From<Layout> for String {
    fn from(layout: Layout) -> String {
        layout.to_string()
    }
}

/// The frame to put around a photo.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Frame {
    pub layout: Layout,
    /// Light text on a black background. Otherwise dark text on a white background.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub dark: bool,
}

// All dimensions are ratios of the photo's short side, so preview and export look the same.
/// Border thickness on top and both sides.
const MARGIN: f64 = 0.04;
/// Height of the bottom margin that holds the text.
const CAPTION: f64 = 0.10;
/// Text size of the first line (model) and the second line (settings).
const TEXT: [f64; 2] = [0.026, 0.019];
/// Gap between the two lines.
const LINE_GAP: f64 = 0.010;

/// The nearest even number. 4:2:0 encoding assumes even sizes, so all added dimensions are even.
fn even(value: f64) -> usize {
    (value / 2.0).round() as usize * 2
}

/// Size of the framed image and the layout within it.
struct Plan {
    width: usize,
    height: usize,
    /// Top-left corner of the photo.
    x: usize,
    y: usize,
    /// Height of the area for the text (right below the photo).
    caption: usize,
}

impl Frame {
    fn plan(&self, width: usize, height: usize) -> Plan {
        let short = width.min(height) as f64;
        let caption = even(short * CAPTION);
        let margin = match self.layout {
            Layout::Band => 0,
            Layout::Border | Layout::Ratio(..) => even(short * MARGIN),
        };
        // Size including the border. When matching a ratio, margin is added to the short side.
        let (inner_w, inner_h) = (width + 2 * margin, height + margin + caption);
        let (full_w, full_h) = match self.layout {
            Layout::Ratio(w, h) if inner_w as u64 * h as u64 > inner_h as u64 * w as u64 => (
                inner_w,
                even(inner_w as f64 * h as f64 / w as f64).max(inner_h),
            ),
            Layout::Ratio(w, h) => (
                even(inner_h as f64 * w as f64 / h as f64).max(inner_w),
                inner_h,
            ),
            Layout::Band | Layout::Border => (inner_w, inner_h),
        };
        Plan {
            width: full_w,
            height: full_h,
            x: (full_w - inner_w) / 2 + margin,
            y: (full_h - inner_h) / 2 + margin,
            caption,
        }
    }

    /// Size of the framed image.
    pub fn size(&self, width: usize, height: usize) -> (usize, usize) {
        let plan = self.plan(width, height);
        (plan.width, plan.height)
    }

    /// How many times the pixel count grows with the frame. Used to gauge how much more memory to
    /// reserve.
    pub fn growth(&self, width: usize, height: usize) -> f64 {
        let (w, h) = self.size(width, height);
        (w * h) as f64 / (width * height) as f64
    }
}

/// The two lines to write on the frame: model and lens, shooting settings and date. Missing values
/// are skipped.
pub fn caption(shot: &Shot) -> [String; 2] {
    let first = [Some(&shot.camera), shot.lens.as_ref()]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(" · ");

    // 2.8 → "2.8", 8.0 → "8"
    let short = |value: f32| format!("{value:.1}").trim_end_matches(".0").to_owned();
    let exposure = shot.exposure.and_then(|(n, d)| {
        if n == 0 || d == 0 {
            return None;
        }
        let (seconds, inverse) = (n as f32 / d as f32, d as f32 / n as f32);
        // Shorter than 1 second with a whole-number denominator: write 1/N like a camera.
        Some(
            if seconds < 1.0 && (inverse - inverse.round()).abs() < 0.05 {
                format!("1/{inverse:.0}s")
            } else {
                format!("{}s", short(seconds))
            },
        )
    });
    let settings = [
        shot.focal_mm.map(|mm| format!("{mm:.0}mm")),
        shot.f_number.map(|f| format!("f/{}", short(f))),
        exposure,
        shot.iso.map(|iso| format!("ISO {iso}")),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("  ");
    // "2026:11:07 13:27:19" → "2026.11.07". The 0000:00:00 of a camera whose clock was never set is
    // not written.
    let date = shot
        .date
        .as_deref()
        .and_then(|date| date.get(..10))
        .filter(|day| {
            day.bytes().filter(u8::is_ascii_digit).count() == 8 && !day.starts_with("0000")
        })
        .map(|day| day.replace(':', "."));
    let second = [Some(settings), date]
        .into_iter()
        .flatten()
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("  ·  ");
    [first, second]
}

/// Makes a new image with a frame around the photo and the shooting information written. Values are
/// taken as sRGB gamma (0–1).
pub fn apply(image: &Image, frame: &Frame, shot: &Shot) -> Image {
    let plan = frame.plan(image.width, image.height);
    let (paper, ink) = if frame.dark {
        (0.0, [0.92, 0.60])
    } else {
        (1.0, [0.12, 0.45])
    };
    let mut out = Image {
        width: plan.width,
        height: plan.height,
        planes: [0, 1, 2].map(|_| vec![paper; plan.width * plan.height]),
    };
    for (to, from) in out.planes.iter_mut().zip(&image.planes) {
        for (row, line) in from.chunks_exact(image.width).enumerate() {
            let start = (plan.y + row) * plan.width + plan.x;
            to[start..start + image.width].copy_from_slice(line);
        }
    }

    // Text: place the two lines in the middle of the margin right below the photo. Shrink them if
    // too long.
    let font = FontRef::try_from_slice(FONT).expect("cannot read the font built into the program");
    let short = image.width.min(image.height) as f64;
    let room = image.width as f32 - 2.0 * (short * MARGIN) as f32;
    let lines: Vec<(String, f32, f32)> = caption(shot)
        .into_iter()
        .zip(TEXT)
        .zip(ink)
        .filter(|((text, _), _)| !text.is_empty())
        .map(|((text, size), ink)| {
            let size = (short * size) as f32;
            let size = size * (room / line_width(&font, &text, size)).min(1.0);
            (text, size, ink)
        })
        .collect();
    let gap = (short * LINE_GAP) as f32;
    let block: f32 =
        lines.iter().map(|line| line.1).sum::<f32>() + gap * lines.len().saturating_sub(1) as f32;
    let centre = plan.x as f32 + image.width as f32 / 2.0;
    let mut top = (plan.y + image.height) as f32 + (plan.caption as f32 - block) / 2.0;
    for (text, size, ink) in &lines {
        draw_line(&mut out, &font, text, *size, centre, top, *ink);
        top += size + gap;
    }
    out
}

/// Width of one line written at text size `size` (pixels).
// ponytail: no kerning. It is not noticeable at caption size.
fn line_width(font: &FontRef, text: &str, size: f32) -> f32 {
    let scaled = font.as_scaled(size);
    text.chars()
        .map(|c| scaled.h_advance(font.glyph_id(c)))
        .sum()
}

/// Writes one line centred on `centre` with `top` as its top edge, at lightness `ink`.
fn draw_line(
    image: &mut Image,
    font: &FontRef,
    text: &str,
    size: f32,
    centre: f32,
    top: f32,
    ink: f32,
) {
    let scaled = font.as_scaled(size);
    let mut x = centre - line_width(font, text, size) / 2.0;
    let baseline = top + scaled.ascent();
    for c in text.chars() {
        let id = font.glyph_id(c);
        let glyph = id.with_scale_and_position(size, ab_glyph::point(x, baseline));
        x += scaled.h_advance(id);
        let Some(outline) = font.outline_glyph(glyph) else {
            continue;
        };
        let bounds = outline.px_bounds();
        outline.draw(|dx, dy, coverage| {
            let (px, py) = (
                bounds.min.x as i64 + dx as i64,
                bounds.min.y as i64 + dy as i64,
            );
            if (0..image.width as i64).contains(&px) && (0..image.height as i64).contains(&py) {
                let index = py as usize * image.width + px as usize;
                for plane in &mut image.planes {
                    plane[index] += (ink - plane[index]) * coverage.clamp(0.0, 1.0);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shot() -> Shot {
        Shot {
            camera: "Sony DSLR-A900".into(),
            lens: Some("50mm F1.4".into()),
            focal_mm: Some(55.0),
            f_number: Some(2.8),
            exposure: Some((1, 60)),
            iso: Some(200),
            date: Some("2026:11:07 13:27:19".into()),
        }
    }

    /// An image filled with one value.
    fn flat(width: usize, height: usize, value: f32) -> Image {
        Image {
            width,
            height,
            planes: [0, 1, 2].map(|_| vec![value; width * height]),
        }
    }

    fn pixel(image: &Image, x: usize, y: usize) -> [f32; 3] {
        [0, 1, 2].map(|c| image.planes[c][y * image.width + x])
    }

    fn white(layout: Layout) -> Frame {
        Frame {
            layout,
            dark: false,
        }
    }

    /// The column range with text (pixels clearly unlike the background) within `xs` and `ys`.
    fn text_columns(
        image: &Image,
        ys: std::ops::Range<usize>,
        background: f32,
    ) -> Option<(usize, usize)> {
        let inked = |x: usize| {
            ys.clone()
                .any(|y| (pixel(image, x, y)[0] - background).abs() > 0.3)
        };
        let columns: Vec<usize> = (0..image.width).filter(|&x| inked(x)).collect();
        Some((*columns.first()?, *columns.last()?))
    }

    #[test]
    fn caption_names_the_camera_and_lists_the_settings() {
        assert_eq!(
            caption(&shot()),
            [
                "Sony DSLR-A900 · 50mm F1.4",
                "55mm  f/2.8  1/60s  ISO 200  ·  2026.11.07"
            ]
        );
    }

    #[test]
    fn missing_values_are_left_out_of_the_caption() {
        let bare = Shot {
            camera: "Sony DSLR-A900".into(),
            ..Shot::default()
        };
        assert_eq!(caption(&bare), ["Sony DSLR-A900", ""]);
        let some = Shot {
            f_number: Some(8.0),
            exposure: Some((2, 1)),
            iso: Some(1600),
            ..bare.clone()
        };
        assert_eq!(caption(&some)[1], "f/8  2s  ISO 1600");
        let dated = Shot {
            date: Some("2026:11:07 13:27:19".into()),
            ..bare
        };
        assert_eq!(caption(&dated)[1], "2026.11.07");
    }

    #[test]
    fn exposure_times_read_like_a_camera_shows_them() {
        let line = |exposure| {
            caption(&Shot {
                exposure: Some(exposure),
                ..Shot::default()
            })[1]
                .clone()
        };
        assert_eq!(line((1, 250)), "1/250s");
        assert_eq!(line((10, 2500)), "1/250s");
        assert_eq!(line((1, 3)), "1/3s");
        assert_eq!(line((4, 10)), "0.4s");
        assert_eq!(line((13, 10)), "1.3s");
        assert_eq!(line((30, 1)), "30s");
        assert_eq!(line((1, 0)), "", "invalid values are not written");
    }

    #[test]
    fn layouts_are_written_as_short_words_or_ratios() {
        for (text, layout) in [
            ("band", Layout::Band),
            ("border", Layout::Border),
            ("1:1", Layout::Ratio(1, 1)),
            ("4:5", Layout::Ratio(4, 5)),
            ("9:16", Layout::Ratio(9, 16)),
        ] {
            assert_eq!(text.parse::<Layout>().unwrap(), layout);
            assert_eq!(layout.to_string(), text);
        }
        for bad in [
            "", "frame", "4:", ":5", "0:5", "4:0", "4:5:6", "-4:5", "1:100", "7:2",
        ] {
            assert!(bad.parse::<Layout>().is_err(), "{bad}");
        }
    }

    #[test]
    fn band_adds_a_strip_below_and_leaves_the_picture_alone() {
        let image = flat(600, 400, 0.5);
        let framed = apply(&image, &white(Layout::Band), &shot());
        // Band height is 10% of the short side
        assert_eq!((framed.width, framed.height), (600, 440));
        assert_eq!(pixel(&framed, 0, 0), [0.5; 3]);
        assert_eq!(pixel(&framed, 599, 399), [0.5; 3]);
        assert_eq!(pixel(&framed, 0, 400), [1.0; 3], "the band is white");
        assert_eq!(pixel(&framed, 599, 439), [1.0; 3]);
        // The text goes inside the band, centred.
        let (left, right) = text_columns(&framed, 400..440, 1.0).expect("no text");
        assert!(left > 20 && right < 580, "{left}..{right}");
        assert!(
            ((left + right) as i32 - 600).abs() <= 4,
            "not centred: {left}..{right}"
        );
        assert!(
            text_columns(&framed, 0..400, 0.5).is_none(),
            "nothing is written over the photo"
        );
    }

    #[test]
    fn a_dark_frame_is_black_with_light_text() {
        let image = flat(600, 400, 0.5);
        let framed = apply(
            &image,
            &Frame {
                layout: Layout::Band,
                dark: true,
            },
            &shot(),
        );
        assert_eq!(pixel(&framed, 0, 439), [0.0; 3]);
        assert!(text_columns(&framed, 400..440, 0.0).is_some());
    }

    #[test]
    fn border_goes_all_round_with_room_for_the_caption_below() {
        let image = flat(600, 400, 0.5);
        let framed = apply(&image, &white(Layout::Border), &shot());
        // Top and sides are 4% of the short side, bottom 10%
        assert_eq!((framed.width, framed.height), (632, 456));
        assert_eq!(pixel(&framed, 15, 15), [1.0; 3]);
        assert_eq!(pixel(&framed, 16, 16), [0.5; 3]);
        assert_eq!(pixel(&framed, 615, 415), [0.5; 3]);
        assert_eq!(pixel(&framed, 616, 416), [1.0; 3]);
        assert!(text_columns(&framed, 416..456, 1.0).is_some());
    }

    #[test]
    fn a_ratio_pads_the_bordered_picture_to_that_shape() {
        let image = flat(600, 400, 0.5);
        // The bordered size is 632×456. At 1:1, top and bottom grow equally to 632×632.
        let square = apply(&image, &white(Layout::Ratio(1, 1)), &shot());
        assert_eq!((square.width, square.height), (632, 632));
        assert_eq!(pixel(&square, 16, 103), [1.0; 3]);
        assert_eq!(
            pixel(&square, 16, 104),
            [0.5; 3],
            "the photo starts at row (632-456)/2+16 = 104"
        );
        assert!(
            text_columns(&square, 504..544, 1.0).is_some(),
            "text right below the photo"
        );

        // A tall 9:16 gives a height of 632×16/9 = 1124 (made even).
        let tall = apply(&image, &white(Layout::Ratio(9, 16)), &shot());
        assert_eq!((tall.width, tall.height), (632, 1124));
        // A wider 16:9 grows the sides: 456×16/9 = 810.
        let wide = apply(&image, &white(Layout::Ratio(16, 9)), &shot());
        assert_eq!((wide.width, wide.height), (810, 456));
        assert_eq!(pixel(&wide, (810 - 632) / 2 + 16, 16), [0.5; 3]);
    }

    #[test]
    fn a_long_caption_shrinks_to_fit_a_narrow_picture() {
        let image = flat(200, 600, 0.5);
        let long = Shot {
            lens: Some("Vario-Sonnar T* 24-70mm F2.8 ZA SSM II".into()),
            ..shot()
        };
        let framed = apply(&image, &white(Layout::Band), &long);
        assert_eq!((framed.width, framed.height), (200, 620));
        let (left, right) = text_columns(&framed, 600..620, 1.0).expect("no text");
        assert!(
            left >= 4 && right <= 195,
            "touched the edge: {left}..{right}"
        );
    }

    #[test]
    fn framed_sizes_stay_even_for_the_encoder() {
        // 4:2:0 encoding assumes even sizes. An even-sized photo must stay even with a frame.
        for (width, height) in [(6048, 4032), (4032, 6048), (5998, 3998), (1202, 802)] {
            for layout in [
                Layout::Band,
                Layout::Border,
                Layout::Ratio(1, 1),
                Layout::Ratio(4, 5),
                Layout::Ratio(9, 16),
            ] {
                let (w, h) = white(layout).size(width, height);
                assert!(
                    w % 2 == 0 && h % 2 == 0,
                    "{layout} {width}×{height} → {w}×{h}"
                );
                assert!(w >= width && h >= height);
            }
        }
    }

    #[test]
    fn growth_says_how_much_bigger_the_framed_picture_is() {
        assert_eq!(white(Layout::Band).growth(600, 400), 1.1);
        // A landscape photo in a tall 9:16 grows a lot. That much more memory must be taken.
        let tall = white(Layout::Ratio(9, 16)).growth(600, 400);
        assert!((tall - 632.0 * 1124.0 / 240_000.0).abs() < 1e-6, "{tall}");
    }

    /// Leaves images in `target/ui/` for checking by eye. `cargo test -- --ignored render_frames`
    #[test]
    #[ignore = "needs sample photos, and leaves image files"]
    fn render_frames() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui");
        std::fs::create_dir_all(&out).unwrap();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110");
        let params = crate::edit::Params::default();
        // One portrait photo and one landscape photo
        for (file, name) in [("DSC04551.ARW", "portrait"), ("DSC04553.ARW", "landscape")] {
            let loaded = crate::export::load(
                &dir.join(file),
                None,
                &crate::capture::Capture::off(),
                &|_| {},
            )
            .unwrap();
            let small = crate::preview::downscale(&loaded.cam, 1600).unwrap();
            let frames = [
                ("band", Layout::Band, false),
                ("border-black", Layout::Border, true),
                ("square", Layout::Ratio(1, 1), false),
                ("4x5", Layout::Ratio(4, 5), false),
            ];
            for (label, layout, dark) in frames {
                let frame = Frame { layout, dark };
                let look = crate::export::Look {
                    frame: Some(&frame),
                    ..crate::export::Look::new(&params)
                };
                let pixels = crate::preview::shrink(
                    &loaded.develop(&small, look, &crate::export::Areas::default()),
                    usize::MAX,
                );
                image::save_buffer(
                    out.join(format!("frame-{name}-{label}.png")),
                    &pixels.rgba,
                    pixels.width as u32,
                    pixels.height as u32,
                    image::ColorType::Rgba8,
                )
                .unwrap();
            }
        }
    }
}
