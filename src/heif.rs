//! sRGB 이미지를 10bit HEIF(HEVC 4:2:0)로 인코딩한다.

use crate::lang::text;
use crate::{Image, Result};
use libheif_rs::{
    Channel, ColorSpace, CompressionFormat, EncoderParameterValue, EncoderQuality, HeifContext,
    LibHeif, RgbChroma,
};

/// 인코딩 결과.
pub struct Encoded {
    pub bytes: Vec<u8>,
    /// 실제로 쓴 품질. 용량 한도 때문에 요청한 값보다 낮을 수 있다.
    pub quality: u8,
}

/// HEVC 데이터가 이 크기(2^24바이트)를 넘으면 libde265가 파일을 열지 못한다. 여유를 두고 그 아래로 맞춘다.
const MAX_BYTES: usize = 16_000_000;

/// `img`는 sRGB 감마가 적용된 0..1 값. `exif`는 TIFF 헤더로 시작하는 EXIF 블록.
/// `threads`는 인코더가 쓸 스레드 수 (여러 장을 동시에 인코딩할 때는 작게 준다).
pub fn encode(img: &Image, quality: u8, exif: Option<&[u8]>, threads: usize) -> Result<Encoded> {
    encode_within(img, quality, exif, threads, MAX_BYTES)
}

/// 결과가 `max_bytes`를 넘으면 품질을 낮춰 다시 인코딩한다.
fn encode_within(
    img: &Image,
    quality: u8,
    exif: Option<&[u8]>,
    threads: usize,
    max_bytes: usize,
) -> Result<Encoded> {
    let mut quality = quality;
    loop {
        let bytes = encode_once(img, quality, exif, threads, &[])?;
        if bytes.len() <= max_bytes {
            return Ok(Encoded { bytes, quality });
        }
        if quality <= MIN_QUALITY {
            let limit = max_bytes as f64 / 1e6;
            return Err(text!(
                format!("품질을 {MIN_QUALITY}까지 낮춰도 용량 한도({limit:.0}MB)를 넘습니다"),
                format!(
                    "Even at quality {MIN_QUALITY} the file is over the size limit ({limit:.0} MB)"
                ),
            )
            .into());
        }
        quality = quality.saturating_sub(10).max(MIN_QUALITY);
    }
}

/// 용량을 맞추려고 품질을 낮출 때의 하한.
const MIN_QUALITY: u8 = 30;

/// 한 번 인코딩한다. `extra`는 libheif 인코더에 더 줄 매개변수(측정에서 x265 설정을 바꿔 볼 때 쓴다).
fn encode_once(
    img: &Image,
    quality: u8,
    exif: Option<&[u8]>,
    threads: usize,
    extra: &[(&str, &str)],
) -> Result<Vec<u8>> {
    let (w, h) = (img.width, img.height);
    let mut out = libheif_rs::Image::new(w as u32, h as u32, ColorSpace::Rgb(RgbChroma::HdrRgbLe))?;
    out.create_plane(Channel::Interleaved, w as u32, h as u32, 10)?;
    {
        let plane = out.planes_mut().interleaved.ok_or(text!(
            "libheif: 인터리브 평면이 없음",
            "libheif: no interleaved plane",
        ))?;
        for (y, row) in plane.data.chunks_mut(plane.stride).take(h).enumerate() {
            for x in 0..w {
                for c in 0..3 {
                    let v = (img.planes[c][y * w + x].clamp(0.0, 1.0) * 1023.0 + 0.5) as u16;
                    row[(x * 3 + c) * 2..][..2].copy_from_slice(&v.to_le_bytes());
                }
            }
        }
    }
    // 색 정보는 따로 지정하지 않는다. libheif 기본값이 sRGB(BT.709 원색, sRGB 전달함수, full range)이고
    // 그 값이 HEVC 비트스트림에 기록된다.

    let lib = LibHeif::new();
    let mut ctx = HeifContext::new()?;
    let mut encoder = lib.encoder_for_format(CompressionFormat::Hevc)?;
    encoder.set_quality(EncoderQuality::Lossy(quality))?;
    // libheif의 기본값은 slow다. medium은 파일이 8% 크지만 화질은 같거나 약간 높고 인코딩 시간이 절반이다
    encoder.set_parameter_value("preset", EncoderParameterValue::String("medium".into()))?;
    // x265는 인코더마다 스레드 풀을 따로 만든다. 여러 장을 동시에 인코딩할 때 그대로 두면 스레드가
    // 코어 수의 몇십 배가 되어 서로 방해한다.
    encoder.set_parameter_value(
        "x265:pools",
        EncoderParameterValue::String(threads.max(1).to_string()),
    )?;
    for (name, value) in extra {
        encoder.set_parameter_value(name, EncoderParameterValue::String((*value).into()))?;
    }
    let handle = ctx.encode_image(&out, &mut encoder, None)?;
    if let Some(exif) = exif {
        ctx.add_exif_metadata(&handle, exif)?;
    }
    Ok(ctx.write_to_bytes()?)
}

/// HEIF를 0..1 값의 RGB로 되읽는다 (테스트와 측정에서 쓴다).
#[cfg(test)]
pub fn decode(bytes: &[u8]) -> Result<Image> {
    let ctx = HeifContext::read_from_bytes(bytes)?;
    let handle = ctx.primary_image_handle()?;
    let decoded = LibHeif::new().decode(&handle, ColorSpace::Rgb(RgbChroma::HdrRgbLe), None)?;
    let (w, h) = (decoded.width() as usize, decoded.height() as usize);
    let planes = decoded.planes();
    let p = planes.interleaved.ok_or("no interleaved plane")?;
    let mut out = [0, 1, 2].map(|_| vec![0f32; w * h]);
    for y in 0..h {
        for x in 0..w {
            for (c, plane) in out.iter_mut().enumerate() {
                let i = y * p.stride + (x * 3 + c) * 2;
                plane[y * w + x] = u16::from_le_bytes([p.data[i], p.data[i + 1]]) as f32 / 1023.0;
            }
        }
    }
    Ok(Image {
        width: w,
        height: h,
        planes: out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 인코더 설정 비교용 그림을 만든다 (`scripts/sharpness/encoder.py`가 읽는다).
    /// `cargo test --release -- --ignored encode_for_encoder_comparison`
    ///
    /// 먼저 `render_for_sharpness_measurement`를 돌려 둔다. 그 그림 가운데 캡처 샤픈을 끈 것(`off`)과
    /// 기본값인 것(`it20-t0.4`)의 압축 전 영역을 x265 설정마다 인코딩해 되읽고 `encoder/`에 쓴다.
    /// 크기는 `encoder/sizes.tsv`.
    #[test]
    #[ignore = "render_for_sharpness_measurement의 그림이 필요하다"]
    fn encode_for_encoder_comparison() {
        use rayon::prelude::*;
        let dir = crate::raw16::dir();
        let out = dir.join("encoder");
        std::fs::create_dir_all(&out).unwrap();
        /// (이름, 품질, libheif 매개변수)
        type Variant = (&'static str, u8, &'static [(&'static str, &'static str)]);
        let variants: &[Variant] = &[
            ("q55", 55, &[]),
            ("q60", 60, &[]),
            ("q65", 65, &[]),
            ("q55-grain", 55, &[("tune", "grain")]),
            (
                "q55-psy2",
                55,
                &[("x265:psy-rd", "2.0"), ("x265:psy-rdoq", "2.0")],
            ),
            ("q55-aq3", 55, &[("x265:aq-mode", "3")]),
            ("q55-444", 55, &[("chroma", "444")]),
            ("q55-psnr", 55, &[("tune", "psnr")]),
        ];
        let inputs: Vec<(String, std::path::PathBuf)> = crate::raw16::PHOTOS
            .iter()
            .flat_map(|(photo, _, _)| {
                let name = photo.rsplit('/').next().unwrap().to_owned();
                ["off", "it20-t0.4"].map(|condition| {
                    let stem = format!("{name}-{condition}");
                    let path = dir.join(format!("{stem}-pre.raw"));
                    (stem, path)
                })
            })
            .collect();
        let rows = std::sync::Mutex::new(Vec::new());
        inputs.par_iter().for_each(|(stem, path)| {
            assert!(
                path.exists(),
                "먼저 render_for_sharpness_measurement를 돌린다: {}",
                path.display()
            );
            let image = crate::raw16::read(path);
            for (label, quality, extra) in variants {
                let bytes = encode_once(&image, *quality, None, 2, extra).unwrap();
                let back = decode(&bytes).unwrap();
                let file = out.join(format!("{stem}__{label}.raw"));
                crate::raw16::write(&back, (0, 0), (back.width, back.height), &file);
                rows.lock()
                    .unwrap()
                    .push(format!("{stem}\t{label}\t{}", bytes.len()));
            }
        });
        let mut rows = rows.into_inner().unwrap();
        rows.sort();
        std::fs::write(out.join("sizes.tsv"), rows.join("\n") + "\n").unwrap();
    }

    fn gradient(w: usize, h: usize) -> Image {
        let mut planes = [vec![0f32; w * h], vec![0f32; w * h], vec![0f32; w * h]];
        for y in 0..h {
            for x in 0..w {
                planes[0][y * w + x] = x as f32 / (w - 1) as f32;
                planes[1][y * w + x] = y as f32 / (h - 1) as f32;
                planes[2][y * w + x] = 0.5;
            }
        }
        Image {
            width: w,
            height: h,
            planes,
        }
    }

    #[test]
    fn encodes_10bit_heif_with_exif_that_decodes_back() {
        let (w, h) = (256, 192);
        let img = gradient(w, h);
        // 최소 EXIF: TIFF 헤더 + 항목 0개짜리 IFD
        let exif = b"II*\0\x08\0\0\0\0\0\0\0\0\0";

        let bytes = encode(&img, 55, Some(exif), 4).unwrap().bytes;

        let ctx = HeifContext::read_from_bytes(&bytes).unwrap();
        let handle = ctx.primary_image_handle().unwrap();
        assert_eq!((handle.width(), handle.height()), (w as u32, h as u32));
        assert_eq!(handle.luma_bits_per_pixel(), 10);

        let mut ids = [0; 1];
        assert_eq!(handle.metadata_block_ids(&mut ids, b"Exif"), 1);
        assert!(handle.metadata(ids[0]).unwrap().ends_with(exif));

        let decoded = LibHeif::new()
            .decode(&handle, ColorSpace::Rgb(RgbChroma::HdrRgbLe), None)
            .unwrap();
        let planes = decoded.planes();
        let p = planes.interleaved.unwrap();
        let mut sq_err = 0f64;
        for y in 0..h {
            for x in 0..w {
                for c in 0..3 {
                    let i = y * p.stride + (x * 3 + c) * 2;
                    let got = u16::from_le_bytes([p.data[i], p.data[i + 1]]) as f64 / 1023.0;
                    let want = img.planes[c][y * w + x] as f64;
                    sq_err += (got - want).powi(2);
                }
            }
        }
        let psnr = -10.0 * (sq_err / (w * h * 3) as f64).log10();
        assert!(psnr > 35.0, "PSNR {psnr:.1} dB");
    }

    /// 압축이 잘 안 되는 이미지 (재현 가능한 잡음).
    fn noise(w: usize, h: usize) -> Image {
        let mut seed = 7u64;
        let mut plane = || {
            (0..w * h)
                .map(|_| {
                    seed = seed
                        .wrapping_mul(6364136223846793005)
                        .wrapping_add(1442695040888963407);
                    (seed >> 40) as f32 / (1u64 << 24) as f32
                })
                .collect()
        };
        Image {
            width: w,
            height: h,
            planes: [plane(), plane(), plane()],
        }
    }

    fn decodes(bytes: &[u8]) -> bool {
        let ctx = HeifContext::read_from_bytes(bytes).unwrap();
        let handle = ctx.primary_image_handle().unwrap();
        LibHeif::new()
            .decode(&handle, ColorSpace::Rgb(RgbChroma::HdrRgbLe), None)
            .is_ok()
    }

    #[test]
    fn thread_count_does_not_change_the_result() {
        let img = gradient(512, 384);
        let one = encode(&img, 55, None, 1).unwrap();
        let many = encode(&img, 55, None, 8).unwrap();
        assert_eq!(one.quality, 55);
        assert!(
            one.bytes == many.bytes,
            "스레드 수와 무관하게 같은 파일이 나와야 한다"
        );
    }

    #[test]
    fn output_over_the_size_limit_is_reencoded_at_lower_quality() {
        let img = noise(512, 384);
        let full = encode_within(&img, 90, None, 4, usize::MAX).unwrap();
        assert_eq!(full.quality, 90);

        let limit = full.bytes.len() / 2;
        let fitted = encode_within(&img, 90, None, 4, limit).unwrap();
        assert!(
            fitted.bytes.len() <= limit,
            "{} > {limit}",
            fitted.bytes.len()
        );
        assert!(fitted.quality < 90);
        assert!(decodes(&fitted.bytes));
    }

    #[test]
    fn an_image_that_cannot_fit_is_an_error_not_a_broken_file() {
        assert!(encode_within(&noise(512, 384), 90, None, 4, 1000).is_err());
    }
}
