//! 측정 도구(`scripts/sharpness`)와 주고받는 16비트 그림 파일. 테스트에서만 쓴다.
//!
//! 형식 (리틀 엔디언): u32 가로, u32 세로, 그 뒤에 화소마다 R, G, B의 u16 (0..65535 = 0..1).
//! 파이썬 쪽은 `scripts/sharpness/common.py`의 `load_raw`, `save_raw`.

use crate::Image;
use std::path::Path;

/// `image`에서 (`x0`, `y0`)부터 `width` × `height`를 잘라 쓴다.
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

/// 측정 그림을 두는 폴더 (`AMSIL_SHARPNESS_DIR`, 없으면 `/tmp/amsil-sharpness`).
pub fn dir() -> std::path::PathBuf {
    let dir = std::path::PathBuf::from(
        std::env::var("AMSIL_SHARPNESS_DIR").unwrap_or("/tmp/amsil-sharpness".into()),
    );
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 측정에 쓰는 샘플: (DCIM 아래 경로, 2048×2048로 자를 곳의 왼쪽 위 y, x).
/// 2026-11-20 측정에서 세부가 가장 많던 창이다. `scripts/sharpness/common.py`의 `PHOTOS`와 같아야 한다.
pub const PHOTOS: [(&str, usize, usize); 6] = [
    ("10361110/DSC04551", 3328, 0),
    ("10361110/DSC04546", 3072, 0),
    ("10361110/DSC04545", 768, 1536),
    ("10161107/DSC04249", 1792, 256),
    ("10161107/DSC04311", 1792, 768),
    ("10161107/DSC04286", 768, 1536),
];

/// 자르는 크기.
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
