mod develop;
mod heif;
mod raw;

use std::path::Path;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// 작업용 이미지. 채널별 평면(R, G, B), 값은 f32.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub planes: [Vec<f32>; 3],
}

/// libheif의 품질 값. 55가 JPEG q90 수준이고 70부터는 JPEG보다 파일이 커진다 (docs/design.md 3.2절).
const QUALITY: u8 = 55;

/// RAW 한 장을 HEIF로 바꾼다.
fn convert(input: &Path, output: &Path, quality: u8) -> Result<()> {
    let raw = raw::decode(input)?;
    let image = develop::develop(&raw.cam, raw.wb, raw.xyz_to_cam);
    let (transpose, flip_h, flip_v) = raw.flips;
    let image = develop::orient(image, transpose, flip_h, flip_v);
    let bytes = heif::encode(&image, quality, Some(&raw.exif))?;

    // 임시 파일에 쓰고 이름을 바꾼다. 도중에 꺼져도 반쯤 쓰인 출력 파일이 남지 않는다.
    let mut tmp = output.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, output)?;
    Ok(())
}

fn main() -> std::process::ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [input, output] = args.as_slice() else {
        eprintln!("사용법: amsil <입력 RAW> <출력.heic>");
        return std::process::ExitCode::from(2);
    };
    let started = std::time::Instant::now();
    match convert(Path::new(input), Path::new(output), QUALITY) {
        Ok(()) => {
            eprintln!(
                "완료: {} ({:.1}초)",
                Path::new(output).display(),
                started.elapsed().as_secs_f32()
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("실패: {}: {e}", Path::new(input).display());
            std::process::ExitCode::FAILURE
        }
    }
}

/// 테스트용 샘플 RAW. 개인 사진이라 저장소에 없고, 없으면 해당 테스트는 건너뛴다.
#[cfg(test)]
pub fn sample_arw() -> Option<std::path::PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10161107/DSC04307.ARW");
    if !path.exists() {
        eprintln!("샘플 ARW가 없어 건너뜀");
    }
    path.exists().then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use libheif_rs::HeifContext;

    #[test]
    fn converts_portrait_arw_to_upright_10bit_heic() {
        let Some(input) = sample_arw() else { return };
        let dir = std::env::temp_dir().join(format!("amsil-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.heic");

        convert(&input, &output, 55).unwrap();

        let ctx = HeifContext::read_from_file(output.to_str().unwrap()).unwrap();
        let handle = ctx.primary_image_handle().unwrap();
        // 6048×4032로 찍힌 세로 사진이 바로 서서 나온다.
        assert_eq!((handle.width(), handle.height()), (4032, 6048));
        assert_eq!(handle.luma_bits_per_pixel(), 10);
        assert_eq!(handle.number_of_metadata_blocks(b"Exif"), 1);
        // 임시 파일이 남지 않는다.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
