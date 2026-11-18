//! RAW 파일을 읽어 카메라 RGB와 현상에 필요한 값들을 꺼낸다.

use crate::{Image, Result};
use rawler::decoders::{Orientation, RawDecodeParams};
use rawler::formats::tiff::TiffWriter;
use rawler::imgop::develop::{Intermediate, ProcessingStep, RawDevelop};
use rawler::imgop::xyz::Illuminant;
use rawler::rawsource::RawSource;
use rawler::tags::TiffCommonTag;
use std::io::Cursor;
use std::path::Path;

pub struct Raw {
    /// 카메라 RGB. 디모자익과 크롭이 끝난 선형 값이고 센서 포화가 1.0이다.
    pub cam: Image,
    /// 촬영 시 화이트밸런스 배율 (G = 1).
    pub wb: [f32; 3],
    /// D65 기준 XYZ → 카메라 행렬.
    pub xyz_to_cam: [[f32; 3]; 3],
    /// 바로 세우는 데 필요한 (전치, 좌우 뒤집기, 상하 뒤집기). `develop::orient`의 인자.
    pub flips: (bool, bool, bool),
    /// 출력 파일에 넣을 EXIF (TIFF 형식). 픽셀을 돌려서 내보내므로 방향은 1로 고쳐 둔다.
    pub exif: Vec<u8>,
}

pub fn decode(path: &Path) -> Result<Raw> {
    let source = RawSource::new(path)?;
    let decoder = rawler::get_decoder(&source)?;
    let params = RawDecodeParams::default();
    let image = decoder.raw_image(&source, &params, false)?;
    let mut metadata = decoder.raw_metadata(&source, &params)?;

    // 화이트밸런스와 색행렬은 develop 단계에서 직접 적용하므로 여기서는 디모자익과 크롭까지만 한다.
    let steps = [
        ProcessingStep::Rescale,
        ProcessingStep::Demosaic,
        ProcessingStep::CropActiveArea,
        ProcessingStep::CropDefault,
    ];
    let Intermediate::ThreeColor(rgb) =
        RawDevelop::new_with(&steps).develop_intermediate(&image)?
    else {
        return Err("RGB 컬러 센서만 지원합니다".into());
    };
    let planes = [0, 1, 2].map(|c| rgb.data.iter().map(|px| px[c]).collect());

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
        cam: Image {
            width: rgb.width,
            height: rgb.height,
            planes,
        },
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
    fn decodes_a900_portrait_sample() {
        let Some(path) = crate::sample_arw() else {
            return;
        };
        let raw = decode(&path).unwrap();

        // 센서는 6080×4048이고 카메라 JPG와 같은 6048×4032로 크롭된다.
        assert_eq!((raw.cam.width, raw.cam.height), (6048, 4032));
        let green = &raw.cam.planes[1];
        let mean = green.iter().sum::<f32>() / green.len() as f32;
        assert!(mean > 0.005 && mean < 0.6, "평균 {mean}");

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
    }
}
