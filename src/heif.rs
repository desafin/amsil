//! Encodes an sRGB image to 10-bit HEIF (HEVC 4:2:0).

use crate::lang::text;
use crate::{Image, Result};
use libheif_rs::{
    Channel, ColorSpace, CompressionFormat, EncoderParameterValue, EncoderQuality, HeifContext,
    LibHeif, RgbChroma,
};

/// Encoding result.
pub struct Encoded {
    pub bytes: Vec<u8>,
    /// The quality actually used. May be lower than requested because of the size limit.
    pub quality: u8,
}

/// libde265 cannot open the file when the HEVC data exceeds this size (2^24 bytes). Stay below it
/// with some margin.
const MAX_BYTES: usize = 16_000_000;

/// `img` holds sRGB-gamma-encoded values in 0..1. `exif` is an EXIF block starting with a TIFF
/// header. `threads` is the number of encoder threads (pass a small value when encoding several
/// images at once).
pub fn encode(img: &Image, quality: u8, exif: Option<&[u8]>, threads: usize) -> Result<Encoded> {
    encode_within(img, quality, exif, threads, MAX_BYTES)
}

/// If the result exceeds `max_bytes`, lowers the quality and encodes again.
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

/// Lower bound when lowering quality to fit the size limit.
const MIN_QUALITY: u8 = 30;

/// Encodes once. `extra` holds additional libheif encoder parameters (used by measurements to try
/// different x265 settings).
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
    // Color information is not set explicitly. The libheif default is sRGB (BT.709 primaries, sRGB
    // transfer function, full range), and that is written into the HEVC bitstream.

    let lib = LibHeif::new();
    let mut ctx = HeifContext::new()?;
    let mut encoder = lib.encoder_for_format(CompressionFormat::Hevc)?;
    encoder.set_quality(EncoderQuality::Lossy(quality))?;
    // The libheif default is slow. medium gives 8% larger files but equal or slightly better
    // quality in half the encoding time
    encoder.set_parameter_value("preset", EncoderParameterValue::String("medium".into()))?;
    // x265 creates a separate thread pool per encoder. Left alone while encoding several images at
    // once, the thread count grows to tens of times the core count and the threads get in each
    // other's way.
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

/// Reads a HEIF back as RGB in 0..1 (used by tests and measurements).
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

    /// Produces images for comparing encoder settings (read by `scripts/sharpness/encoder.py`).
    /// `cargo test --release -- --ignored encode_for_encoder_comparison`
    ///
    /// Run `render_for_sharpness_measurement` first. From its images, the pre-compression crops
    /// with capture sharpening off (`off`) and at the default (`it20-t0.4`) are encoded with each
    /// x265 setting, read back, and written to `encoder/`. Sizes go to `encoder/sizes.tsv`.
    #[test]
    #[ignore = "needs the images from render_for_sharpness_measurement"]
    fn encode_for_encoder_comparison() {
        use rayon::prelude::*;
        let dir = crate::raw16::dir();
        let out = dir.join("encoder");
        std::fs::create_dir_all(&out).unwrap();
        /// (name, quality, libheif parameters)
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
                "run render_for_sharpness_measurement first: {}",
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
        // Minimal EXIF: TIFF header + an IFD with zero entries
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

    /// An image that compresses poorly (reproducible noise).
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
            "the output must be identical regardless of thread count"
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
