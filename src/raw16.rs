//! 16-bit image files exchanged with the measurement tools (`scripts/sharpness`). Test-only.
//!
//! Format (little-endian): u32 width, u32 height, then u16 R, G, B per pixel (0..65535 = 0..1).
//! The Python side is `load_raw` and `save_raw` in `scripts/sharpness/common.py`.

use crate::Image;
use std::path::Path;

/// Crops `width` × `height` from `image` starting at (`x0`, `y0`) and writes it.
pub fn write(
    image: &Image,
    (x0, y0): (usize, usize),
    (width, height): (usize, usize),
    path: &Path,
) {
    let mut bytes = Vec::with_capacity(8 + width * height * 6);
    bytes.extend((width as u32).to_le_bytes());
    bytes.extend((height as u32).to_le_bytes());
    for y in y0..y0 + height {
        for x in x0..x0 + width {
            for c in 0..3 {
                let v = image.planes[c][y * image.width + x].clamp(0.0, 1.0);
                bytes.extend(((v * 65535.0 + 0.5) as u16).to_le_bytes());
            }
        }
    }
    std::fs::write(path, bytes).unwrap();
}

pub fn read(path: &Path) -> Image {
    let bytes = std::fs::read(path).unwrap();
    let word = |i: usize| u32::from_le_bytes(bytes[i * 4..i * 4 + 4].try_into().unwrap()) as usize;
    let (width, height) = (word(0), word(1));
    let mut planes = [0, 1, 2].map(|_| vec![0f32; width * height]);
    for (k, v) in bytes[8..].chunks_exact(2).enumerate() {
        planes[k % 3][k / 3] = u16::from_le_bytes([v[0], v[1]]) as f32 / 65535.0;
    }
    Image {
        width,
        height,
        planes,
    }
}

/// Folder for measurement images (`AMSIL_SHARPNESS_DIR`, or `/tmp/amsil-sharpness` if unset).
pub fn dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(
        std::env::var("AMSIL_SHARPNESS_DIR").unwrap_or("/tmp/amsil-sharpness".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Samples used for measurement: (path under DCIM, top-left y, x of the 2048×2048 crop).
/// These were the windows with the most detail in the 2026-11-20 measurement. Must match `PHOTOS`
/// in `scripts/sharpness/common.py`.
pub const PHOTOS: [(&str, usize, usize); 6] = [
    ("10361110/DSC04551", 3328, 0),
    ("10361110/DSC04546", 3072, 0),
    ("10361110/DSC04545", 768, 1536),
    ("10161107/DSC04249", 1792, 256),
    ("10161107/DSC04311", 1792, 768),
    ("10161107/DSC04286", 768, 1536),
];

/// Crop size.
pub const SIDE: usize = 2048;

#[cfg(test)]
mod tests {
    #[test]
    fn a_crop_comes_back_as_written() {
        let image = crate::Image {
            width: 4,
            height: 3,
            planes: [0, 1, 2].map(|c| (0..12).map(|i| (i * 3 + c) as f32 / 40.0).collect()),
        };
        let path = std::env::temp_dir().join(format!("amsil-raw16-{}.raw", std::process::id()));
        super::write(&image, (1, 1), (2, 2), &path);
        let back = super::read(&path);
        std::fs::remove_file(&path).unwrap();
        assert_eq!((back.width, back.height), (2, 2));
        for c in 0..3 {
            for (y, x) in [(0, 0), (1, 1)] {
                let want = image.planes[c][(y + 1) * 4 + x + 1];
                assert!((back.planes[c][y * 2 + x] - want).abs() < 1e-4);
            }
        }
    }
}
