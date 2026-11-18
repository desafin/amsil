//! sRGB 이미지를 10bit HEIF(HEVC 4:2:0)로 인코딩한다.

use crate::{Image, Result};
use libheif_rs::{
    Channel, ColorSpace, CompressionFormat, EncoderQuality, HeifContext, LibHeif, RgbChroma,
};

/// `img`는 sRGB 감마가 적용된 0..1 값. `exif`는 TIFF 헤더로 시작하는 EXIF 블록.
pub fn encode(img: &Image, quality: u8, exif: Option<&[u8]>) -> Result<Vec<u8>> {
    let (w, h) = (img.width, img.height);
    let mut out = libheif_rs::Image::new(w as u32, h as u32, ColorSpace::Rgb(RgbChroma::HdrRgbLe))?;
    out.create_plane(Channel::Interleaved, w as u32, h as u32, 10)?;
    {
        let plane = out
            .planes_mut()
            .interleaved
            .ok_or("libheif: 인터리브 평면이 없음")?;
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
    let handle = ctx.encode_image(&out, &mut encoder, None)?;
    if let Some(exif) = exif {
        ctx.add_exif_metadata(&handle, exif)?;
    }
    Ok(ctx.write_to_bytes()?)
}

#[cfg(test)]
mod tests {
    use super::*;

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

        let bytes = encode(&img, 55, Some(exif)).unwrap();

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
}
