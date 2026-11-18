//! RAW 파일을 읽어 카메라 RGB와 현상에 필요한 값들을 꺼낸다.

use crate::denoise::{Packed, pack_rggb};
use crate::{Image, Result};
use rawler::RawImage;
use rawler::cfa::{CFA, CFAColor};
use rawler::decoders::{Orientation, RawDecodeParams};
use rawler::formats::tiff::TiffWriter;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::xyz::Illuminant;
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::rawsource::RawSource;
use rawler::tags::TiffCommonTag;
use std::io::Cursor;
use std::path::Path;

pub struct Raw {
    /// 블랙·화이트 레벨 보정이 끝난 센서 데이터.
    image: RawImage,
    /// 센서에서 첫 R 화소의 위치. AI 모델은 RGGB 배치를 전제로 한다.
    rggb_offset: (usize, usize),
    /// 촬영 시 화이트밸런스 배율 (G = 1).
    pub wb: [f32; 3],
    /// D65 기준 XYZ → 카메라 행렬.
    pub xyz_to_cam: [[f32; 3]; 3],
    /// 바로 세우는 데 필요한 (전치, 좌우 뒤집기, 상하 뒤집기). `develop::orient`의 인자.
    pub flips: (bool, bool, bool),
    /// 출력 파일에 넣을 EXIF (TIFF 형식). 픽셀을 돌려서 내보내므로 방향은 1로 고쳐 둔다.
    pub exif: Vec<u8>,
}

impl Raw {
    /// 일반 디모자익(PPG)을 거친 카메라 RGB. 선형 값이고 센서 포화가 1.0이며 출력 영역으로 잘려 있다.
    pub fn demosaic(&self) -> Result<Image> {
        // 화이트밸런스와 색행렬은 develop 단계에서 직접 적용하므로 여기서는 디모자익과 크롭까지만 한다.
        let steps = [
            ProcessingStep::Demosaic,
            ProcessingStep::CropActiveArea,
            ProcessingStep::CropDefault,
        ];
        let Intermediate::ThreeColor(rgb) =
            RawDevelop::new_with(&steps).develop_intermediate(&self.image)?
        else {
            return Err("RGB 컬러 센서만 지원합니다".into());
        };
        let planes = [0, 1, 2].map(|c| rgb.data.iter().map(|px| px[c]).collect());
        Ok(Image {
            width: rgb.width,
            height: rgb.height,
            planes,
        })
    }

    /// AI 노이즈 제거 모델의 입력. 센서 전체를 RGGB 4채널로 묶은 것이고 값은 0..1이다.
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

    /// `packed_bayer`로 만든 모델 출력(센서 전체 크기)을 `demosaic`과 같은 영역으로 자른다.
    pub fn crop_model_output(&self, full: &Image) -> Result<Image> {
        let (x, y, w, h) = match self.image.crop_area.or(self.image.active_area) {
            Some(area) => (area.p.x, area.p.y, area.d.w, area.d.h),
            None => (0, 0, self.image.width, self.image.height),
        };
        // 모델 출력의 (0, 0)은 센서의 첫 R 화소 자리다.
        let (ox, oy) = self.rggb_offset;
        if x < ox || y < oy || x - ox + w > full.width || y - oy + h > full.height {
            return Err("모델 출력이 출력 영역을 다 덮지 못합니다".into());
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

/// 2×2 Bayer 배치에서 R 화소의 위치 (x, y). RGB Bayer가 아니면 None.
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

/// 센서의 화소 수. 화소 데이터를 풀지 않고 헤더만 읽는다 (메모리 사용량을 미리 가늠하는 데 쓴다).
pub fn pixel_count(path: &Path) -> Result<u64> {
    let source = RawSource::new(path)?;
    let image = decoder(&source)?.raw_image(&source, &RawDecodeParams::default(), true)?;
    Ok((image.width * image.height) as u64)
}

/// 파일 형식에 맞는 디코더를 찾는다.
fn decoder(source: &RawSource) -> Result<Box<dyn rawler::decoders::Decoder>> {
    rawler::get_decoder(source)
        .map_err(|e| format!("지원하지 않거나 손상된 RAW 파일입니다 ({e})").into())
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
    .ok_or("RGB Bayer 센서만 지원합니다")?;
    image.apply_scaling()?;

    let wb = image.wb_coeffs;
    let wb = if wb[0].is_nan() {
        [1.0; 3]
    } else {
        [wb[0] / wb[1], 1.0, wb[2] / wb[1]]
    };

    // ponytail: D65 행렬만 쓴다. 다른 광원 행렬만 있는 카메라가 나오면 Bradford 변환을 추가.
    let m = image
        .color_matrix
        .get(&Illuminant::D65)
        .filter(|m| m.len() >= 9)
        .ok_or("이 카메라의 D65 색행렬이 없습니다")?;
    let xyz_to_cam = [0, 1, 2].map(|i| [m[i * 3], m[i * 3 + 1], m[i * 3 + 2]]);

    // rawler의 `RawImage::orientation`은 항상 Normal이다. 실제 방향은 EXIF에 있다.
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
            pixel_count(&path).err().unwrap(),
        );
        std::fs::remove_file(&path).unwrap();
        for error in [decoded.to_string(), counted.to_string()] {
            assert!(error.contains("지원하지 않거나 손상된 RAW 파일"), "{error}");
        }
    }

    #[test]
    fn pixel_count_is_the_sensor_size() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        assert_eq!(pixel_count(&path).unwrap(), 6080 * 4048);
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
            "세로로 찍은 사진이라 전치가 필요하다: {:?}",
            raw.flips
        );

        assert!(raw.exif.starts_with(b"II*\0"));
        let has = |needle: &[u8]| raw.exif.windows(needle.len()).any(|w| w == needle);
        assert!(has(b"DSLR-A900") && has(b"2026:11:07 13:27:19"));

        // 센서는 6080×4048이고 카메라 JPG와 같은 6048×4032로 크롭된다.
        let cam = raw.demosaic().unwrap();
        assert_eq!((cam.width, cam.height), (6048, 4032));
        let mean = |p: &[f32]| p.iter().sum::<f32>() / p.len() as f32;
        let green = mean(&cam.planes[1]);
        assert!(green > 0.005 && green < 0.6, "평균 {green}");

        // 모델 입력은 센서 전체의 절반 크기다.
        let packed = raw.packed_bayer();
        assert_eq!((packed.width, packed.height), (3040, 2024));
        assert!(
            packed
                .planes
                .iter()
                .flatten()
                .all(|v| (0.0..=1.0).contains(v))
        );
        // 두 G 채널은 거의 같고, 화이트밸런스 전이라 G가 R과 B보다 밝다.
        let [r, g1, g2, b] = [0, 1, 2, 3].map(|c| mean(&packed.planes[c]));
        assert!((g1 - g2).abs() < 0.02 * g1, "G1 {g1} G2 {g2}");
        assert!(g1 > r && g1 > b, "R {r} G {g1} B {b}");

        // 모델 출력을 자르면 일반 디모자익과 같은 영역이 된다:
        // R 화소 자리에서는 디모자익 결과의 R이 센서 값 그대로다.
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
}
