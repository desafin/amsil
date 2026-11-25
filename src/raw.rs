//! Reads a RAW file and extracts camera RGB and the values needed for development.

use crate::denoise::{Packed, pack_rggb};
use crate::frame::Shot;
use crate::lang::text;
use crate::{Image, Result};
use rawler::RawImage;
use rawler::cfa::{CFA, CFAColor};
use rawler::decoders::{Orientation, RawDecodeParams};
use rawler::formats::tiff::TiffWriter;
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::tags::TiffCommonTag;
use std::borrow::Cow;
use std::io::Cursor;
use std::path::Path;

pub struct Raw {
    /// Sensor data with black and white level correction applied.
    image: RawImage,
    /// Position of the first R pixel on the sensor. The AI model assumes an RGGB layout.
    rggb_offset: (usize, usize),
    /// As-shot white balance multipliers (G = 1).
    pub wb: [f32; 3],
    /// D65 XYZ → camera matrix.
    pub xyz_to_cam: [[f32; 3]; 3],
    /// (transpose, horizontal flip, vertical flip) needed to make the image upright. Arguments to
    /// `develop::orient`.
    pub flips: (bool, bool, bool),
    /// EXIF to embed in the output file (TIFF format). The pixels are rotated on export, so the
    /// orientation is rewritten to 1.
    pub exif: Vec<u8>,
    /// Shooting information to print on the frame.
    pub shot: Shot,
}

/// The full-sensor CFA and its layout. Capture sharpening uses it to derive highlight protection
/// and the radius.
pub struct Sensor<'a> {
    /// Values in 0..1 after black/white correction (before white balance). Full sensor, row-major.
    pub cfa: Cow<'a, [f32]>,
    pub width: usize,
    pub height: usize,
    /// Position of the first R pixel (x, y).
    pub red: (usize, usize),
    /// Output area: (x, y, width, height). `Raw::demosaic` returns this area of the sensor.
    pub crop: (usize, usize, usize, usize),
}

impl Raw {
    /// Camera RGB after RCD demosaicing. Linear, with sensor saturation at 1.0, cropped to the
    /// output area. RCD assumes white-balanced input, so the data is multiplied by the camera white
    /// balance, clipped to [0, 1], demosaiced, and divided back. Saturated pixels are therefore
    /// clipped relative to the camera white balance (development also clips at 1.0 after applying
    /// white balance).
    pub fn demosaic(&self) -> Result<Image> {
        let sensor = self.sensor();
        let (w, h) = (sensor.width, sensor.height);
        let colour = |x: usize, y: usize| crate::rcd::colour_at(x, y, sensor.red);
        let balanced: Vec<f32> = sensor
            .cfa
            .iter()
            .enumerate()
            .map(|(i, v)| (v * self.wb[colour(i % w, i / w)]).clamp(0.0, 1.0))
            .collect();
        let mut full = crate::rcd::demosaic(&balanced, w, h, sensor.red);
        for (plane, wb) in full.planes.iter_mut().zip(self.wb) {
            plane.iter_mut().for_each(|v| *v /= wb);
        }
        let image = self.crop(&full, (0, 0))?;
        debug_assert_eq!((image.width, image.height), (sensor.crop.2, sensor.crop.3));
        Ok(image)
    }

    /// The full-sensor CFA, its layout, and the output area.
    pub fn sensor(&self) -> Sensor<'_> {
        let (x, y, w, h) = self.crop_area();
        Sensor {
            cfa: match self.image.data.as_f32() {
                Cow::Borrowed(data) => Cow::Borrowed(data.as_slice()),
                Cow::Owned(data) => Cow::Owned(data),
            },
            width: self.image.width,
            height: self.image.height,
            red: self.rggb_offset,
            crop: (x, y, w, h),
        }
    }

    /// Output area (x, y, width, height) in sensor coordinates.
    fn crop_area(&self) -> (usize, usize, usize, usize) {
        match self.image.crop_area.or(self.image.active_area) {
            Some(area) => (area.p.x, area.p.y, area.d.w, area.d.h),
            None => (0, 0, self.image.width, self.image.height),
        }
    }

    /// For comparison: rawler's PPG demosaic (the one used previously).
    #[cfg(test)]
    pub fn demosaic_ppg_for_tests(&self) -> Result<Image> {
        use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
        // White balance and the color matrix are applied directly in the develop stage, so only
        // demosaic and crop happen here.
        let steps = [
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
            ProcessingStep::CropDefault,
        ];
        let Intermediate::ThreeColor(rgb) =
            RawDevelop::new_with(&steps).develop_intermediate(&self.image)?
        else {
            return Err(text!(
                "RGB 컬러 센서만 지원합니다",
                "Only RGB colour sensors are supported",
            )
            .into());
        };
        let planes = [0, 1, 2].map(|c| rgb.data.iter().map(|px| px[c]).collect());
        Ok(Image {
            width: rgb.width,
            height: rgb.height,
            planes,
        })
    }

    /// Input for the AI denoising model: the full sensor packed into 4 RGGB channels, values in
    /// 0..1.
    pub fn packed_bayer(&self) -> Packed {
        let (ox, oy) = self.rggb_offset;
        let mut packed = pack_rggb(
            &self.image.data.as_f32(),
            self.image.width,
            self.image.height,
            ox,
            oy,
        );
        packed
            .planes
            .iter_mut()
            .flatten()
            .for_each(|v| *v = v.clamp(0.0, 1.0));
        packed
    }

    /// Crops the model output made from `packed_bayer` (full sensor size) to the same area as
    /// `demosaic`. The model output's (0, 0) is at the sensor's first R pixel.
    pub fn crop_model_output(&self, full: &Image) -> Result<Image> {
        self.crop(full, self.rggb_offset)
    }

    /// Crops the image `full`, which starts at the sensor position `origin`, to the output area.
    fn crop(&self, full: &Image, (ox, oy): (usize, usize)) -> Result<Image> {
        let (x, y, w, h) = self.crop_area();
        if x < ox || y < oy || x - ox + w > full.width || y - oy + h > full.height {
            return Err(text!(
                "모델 출력이 출력 영역을 다 덮지 못합니다",
                "The model output does not cover the whole picture",
            )
            .into());
        }
        let (x, y) = (x - ox, y - oy);
        let planes = [0, 1, 2].map(|c| {
            let mut out = Vec::with_capacity(w * h);
            for row in y..y + h {
                out.extend_from_slice(&full.planes[c][row * full.width + x..][..w]);
            }
            out
        });
        Ok(Image {
            width: w,
            height: h,
            planes,
        })
    }
}

/// Position (x, y) of the R pixel in a 2×2 Bayer layout. None if not an RGB Bayer.
fn rggb_offset(cfa: &CFA) -> Option<(usize, usize)> {
    if cfa.width != 2 || cfa.height != 2 {
        return None;
    }
    [(0, 0), (1, 0), (0, 1), (1, 1)]
        .into_iter()
        .find(|&(x, y)| {
            cfa.cfa_color_at(y, x) == CFAColor::RED
                && cfa.cfa_color_at(y, x + 1) == CFAColor::GREEN
                && cfa.cfa_color_at(y + 1, x) == CFAColor::GREEN
                && cfa.cfa_color_at(y + 1, x + 1) == CFAColor::BLUE
        })
}

/// Returns the preview JPEG embedded in the RAW file, rotated upright. None if there is none.
pub fn embedded_preview(path: &Path) -> Result<Option<image::DynamicImage>> {
    let source = RawSource::new(path)?;
    let decoder = decoder(&source)?;
    let params = RawDecodeParams::default();
    let Some(preview) = decoder.preview_image(&source, &params)? else {
        return Ok(None);
    };
    let orientation = decoder
        .raw_metadata(&source, &params)?
        .exif
        .orientation
        .unwrap_or(1);
    Ok(Some(match Orientation::from_u16(orientation) {
        Orientation::Rotate90 => preview.rotate90(),
        Orientation::Rotate180 => preview.rotate180(),
        Orientation::Rotate270 => preview.rotate270(),
        Orientation::HorizontalFlip => preview.fliph(),
        Orientation::VerticalFlip => preview.flipv(),
        Orientation::Transpose => preview.rotate90().fliph(),
        Orientation::Transverse => preview.rotate270().fliph(),
        Orientation::Normal | Orientation::Unknown => preview,
    }))
}

/// Sensor width and height in pixels. Reads only the header without decoding pixel data (used to
/// estimate memory use in advance).
pub fn sensor_size(path: &Path) -> Result<(usize, usize)> {
    let source = RawSource::new(path)?;
    let image = decoder(&source)?.raw_image(&source, &RawDecodeParams::default(), true)?;
    Ok((image.width, image.height))
}

/// Finds the decoder for the file format.
fn decoder(source: &RawSource) -> Result<Box<dyn rawler::decoders::Decoder>> {
    rawler::get_decoder(source).map_err(|e| {
        text!(
            format!("지원하지 않거나 손상된 RAW 파일입니다 ({e})"),
            format!("Unsupported or damaged RAW file ({e})"),
        )
        .into()
    })
}

/// Extracts the shooting information to print on the frame.
fn shot(metadata: &rawler::decoders::RawMetadata) -> Shot {
    let exif = &metadata.exif;
    let ratio = |r: &rawler::formats::tiff::Rational| (r.d != 0).then(|| r.n as f32 / r.d as f32);
    let (make, model) = (metadata.make.trim(), metadata.model.trim());
    // Don't repeat the maker if the model name already contains it (e.g. "Canon EOS R5").
    let camera = if model.to_lowercase().starts_with(&make.to_lowercase()) {
        model.to_owned()
    } else {
        format!("{make} {model}")
    };
    let lens = exif
        .lens_model
        .clone()
        .or_else(|| metadata.lens.as_ref().map(|lens| lens.lens_name.clone()))
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty());
    Shot {
        camera,
        lens,
        focal_mm: exif.focal_length.as_ref().and_then(ratio),
        f_number: exif.fnumber.as_ref().and_then(ratio),
        exposure: exif.exposure_time.as_ref().map(|r| (r.n, r.d)),
        iso: exif
            .iso_speed_ratings
            .map(u32::from)
            .or(exif.iso_speed)
            .filter(|&iso| iso > 0),
        date: exif
            .date_time_original
            .clone()
            .or_else(|| exif.create_date.clone()),
    }
}

pub fn decode(path: &Path) -> Result<Raw> {
    let source = RawSource::new(path)?;
    let decoder = decoder(&source)?;
    let params = RawDecodeParams::default();
    let mut image = decoder.raw_image(&source, &params, false)?;
    let mut metadata = decoder.raw_metadata(&source, &params)?;

    let rggb_offset = match &image.photometric {
        RawPhotometricInterpretation::Cfa(config) => rggb_offset(&config.cfa),
        _ => None,
    }
    .ok_or(text!(
        "RGB Bayer 센서만 지원합니다",
        "Only RGB Bayer sensors are supported",
    ))?;
    image.apply_scaling()?;

    let wb = image.wb_coeffs;
    let wb = if wb[0].is_nan() {
        [1.0; 3]
    } else {
        [wb[0] / wb[1], 1.0, wb[2] / wb[1]]
    };

    // ponytail: only the D65 matrix is used. Add a Bradford transform if a camera turns up with
    // only other illuminant matrices.
    let m = image
        .color_matrix
        .get(&Illuminant::D65)
        .filter(|m| m.len() >= 9)
        .ok_or(text!(
            "이 카메라의 D65 색행렬이 없습니다",
            "No D65 colour matrix for this camera",
        ))?;
    let xyz_to_cam = [0, 1, 2].map(|i| [m[i * 3], m[i * 3 + 1], m[i * 3 + 2]]);

    // rawler's `RawImage::orientation` is always Normal. The real orientation is in the EXIF.
    let flips = Orientation::from_u16(metadata.exif.orientation.unwrap_or(1)).to_flips();
    metadata.exif.orientation = Some(1);
    let mut exif = Cursor::new(Vec::new());
    let mut tiff = TiffWriter::new(&mut exif)?;
    let (mut root, mut exif_ifd) = (tiff.new_directory(), tiff.new_directory());
    root.add_tag(TiffCommonTag::Make, metadata.make.as_str());
    root.add_tag(TiffCommonTag::Model, metadata.model.as_str());
    metadata.write_exif_tags(&mut tiff, &mut root, &mut exif_ifd)?;
    let exif_offset = exif_ifd.build(&mut tiff)?;
    root.add_tag(TiffCommonTag::ExifIFDPointer, exif_offset);
    tiff.build(root)?;

    Ok(Raw {
        image,
        rggb_offset,
        wb,
        xyz_to_cam,
        flips,
        exif: exif.into_inner(),
        shot: shot(&metadata),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_red_pixel_in_each_bayer_layout() {
        assert_eq!(rggb_offset(&CFA::new("RGGB")), Some((0, 0)));
        assert_eq!(rggb_offset(&CFA::new("GRBG")), Some((1, 0)));
        assert_eq!(rggb_offset(&CFA::new("GBRG")), Some((0, 1)));
        assert_eq!(rggb_offset(&CFA::new("BGGR")), Some((1, 1)));
    }

    #[test]
    fn a_file_that_is_not_a_raw_photo_gets_a_plain_explanation() {
        let path = std::env::temp_dir().join(format!("amsil-not-raw-{}.ARW", std::process::id()));
        std::fs::write(&path, [0x5a; 4096]).unwrap();
        let (decoded, counted) = (
            decode(&path).err().unwrap(),
            sensor_size(&path).err().unwrap(),
        );
        std::fs::remove_file(&path).unwrap();
        for error in [decoded.to_string(), counted.to_string()] {
            assert!(error.contains("지원하지 않거나 손상된 RAW 파일"), "{error}");
        }
    }

    #[test]
    fn the_sensor_size_is_read_from_the_header() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        assert_eq!(sensor_size(&path).unwrap(), (6080, 4048));
    }

    #[test]
    fn decodes_a900_portrait_sample() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        let raw = decode(&path).unwrap();

        assert_eq!(raw.wb[1], 1.0);
        assert!(raw.wb[0] > 1.0 && raw.wb[2] > 1.0, "{:?}", raw.wb);
        assert_eq!(raw.xyz_to_cam[0], [0.5209, -0.1072, -0.0397]);
        assert!(
            raw.flips.0,
            "a portrait shot needs a transpose: {:?}",
            raw.flips
        );

        assert!(raw.exif.starts_with(b"II*\0"));
        let has = |needle: &[u8]| raw.exif.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"DSLR-A900") && has(b"2026:11:07 13:27:19"));

        // The sensor is 6080×4048 and is cropped to 6048×4032, the same as the camera JPG.
        let cam = raw.demosaic().unwrap();
        assert_eq!((cam.width, cam.height), (6048, 4032));
        let mean = |p: &[f32]| p.iter().sum::<f32>() / p.len() as f32;
        let green = mean(&cam.planes[1]);
        assert!(green > 0.005 && green < 0.6, "mean {green}");

        // The model input is half the size of the full sensor.
        let packed = raw.packed_bayer();
        assert_eq!((packed.width, packed.height), (3040, 2024));
        assert!(
            packed
                .planes
                .iter()
                .flatten()
                .all(|v| (0.0..=1.0).contains(v))
        );
        // The two G channels are nearly equal, and before white balance G is brighter than R and B.
        let [r, g1, g2, b] = [0, 1, 2, 3].map(|c| mean(&packed.planes[c]));
        assert!((g1 - g2).abs() < 0.02 * g1, "G1 {g1} G2 {g2}");
        assert!(g1 > r && g1 > b, "R {r} G {g1} B {b}");

        // Cropping the model output gives the same area as the regular demosaic:
        // at R pixel sites, the demosaiced R equals the sensor value.
        let full = Image {
            width: packed.width * 2,
            height: packed.height * 2,
            planes: [0, 1, 2].map(|_| {
                let mut plane = vec![0f32; packed.width * packed.height * 4];
                for (i, v) in packed.planes[0].iter().enumerate() {
                    let (x, y) = (i % packed.width * 2, i / packed.width * 2);
                    plane[y * packed.width * 2 + x] = *v;
                }
                plane
            }),
        };
        let cropped = raw.crop_model_output(&full).unwrap();
        assert_eq!((cropped.width, cropped.height), (cam.width, cam.height));
        for (x, y) in [(0, 0), (100, 200), (2000, 1000), (6046, 4030)] {
            let i = y * cam.width + x;
            assert!(
                (cropped.planes[0][i] - cam.planes[0][i]).abs() < 1e-6,
                "({x}, {y})"
            );
            assert!(cropped.planes[0][i] > 0.0);
        }
    }

    #[test]
    fn rcd_covers_the_same_area_as_rawler_ppg_and_is_not_shifted() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        let raw = decode(&path).unwrap();
        let ours = raw.demosaic().unwrap();
        let ppg = raw.demosaic_ppg_for_tests().unwrap();
        assert_eq!((ours.width, ours.height), (ppg.width, ppg.height));
        // Green-plane correlation must peak at zero offset (it drops with even a one-pixel shift).
        let corr = |dx: isize, dy: isize| {
            let (w, h) = (ours.width as isize, ours.height as isize);
            let (mut sab, mut saa, mut sbb) = (0f64, 0f64, 0f64);
            for y in (100..h - 100).step_by(7) {
                for x in (100..w - 100).step_by(7) {
                    let a = ours.planes[1][(y * w + x) as usize] as f64;
                    let b = ppg.planes[1][((y + dy) * w + x + dx) as usize] as f64;
                    sab += a * b;
                    saa += a * a;
                    sbb += b * b;
                }
            }
            sab / (saa * sbb).sqrt()
        };
        let here = corr(0, 0);
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            assert!(here > corr(dx, dy), "({dx}, {dy}): {here} {}", corr(dx, dy));
        }
        // Colors are nearly the same as PPG (mean absolute difference).
        for c in 0..3 {
            let diff = ours.planes[c]
                .iter()
                .zip(&ppg.planes[c])
                .map(|(a, b)| (a - b).abs() as f64)
                .sum::<f64>()
                / ours.planes[c].len() as f64;
            assert!(diff < 0.005, "{c}: {diff}");
        }
    }

    #[test]
    fn the_sensor_view_matches_the_crop_of_the_demosaiced_photo() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        let raw = decode(&path).unwrap();
        let sensor = raw.sensor();
        let photo = raw.demosaic().unwrap();
        assert_eq!((sensor.crop.2, sensor.crop.3), (photo.width, photo.height));
        assert_eq!((sensor.width, sensor.height), (6080, 4048));
        assert_eq!(sensor.cfa.len(), sensor.width * sensor.height);
        assert_eq!(sensor.red, raw.rggb_offset);
        // (0, 0) of the cropped image is at the sensor's crop position: that pixel's own-color
        // value equals the sensor value.
        let (cx, cy) = (sensor.crop.0, sensor.crop.1);
        let colour = |x: usize, y: usize| match ((x + sensor.red.0) % 2, (y + sensor.red.1) % 2) {
            (0, 0) => 0,
            (1, 1) => 2,
            _ => 1,
        };
        for (x, y) in [(0, 0), (31, 17), (3000, 2000)] {
            let (sx, sy) = (cx + x, cy + y);
            let c = colour(sx, sy);
            let sensor_value = sensor.cfa[sy * sensor.width + sx].clamp(0.0, 1.0 / raw.wb[c]);
            let photo_value = photo.planes[c][y * photo.width + x];
            assert!(
                (photo_value - sensor_value).abs() < 1e-6,
                "({x}, {y}): {photo_value} {sensor_value}"
            );
        }
    }

    #[test]
    fn reads_the_shot_info_for_the_frame() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        let shot = decode(&path).unwrap().shot;
        assert_eq!(shot.camera, "Sony DSLR-A900");
        assert_eq!(
            (shot.focal_mm, shot.f_number, shot.exposure, shot.iso),
            (Some(55.0), Some(2.8), Some((1, 60)), Some(200))
        );
        assert_eq!(shot.date.as_deref(), Some("2026:11:07 13:27:19"));
        eprintln!("lens: {:?}", shot.lens);
    }
}
