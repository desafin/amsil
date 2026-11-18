mod denoise;
mod develop;
mod gpu;
mod heif;
mod raw;

use gpu::Denoiser;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// 작업용 이미지. 채널별 평면(R, G, B), 값은 f32.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub planes: [Vec<f32>; 3],
}

/// libheif의 품질 값. 55가 JPEG q90 수준이고 70부터는 JPEG보다 파일이 커진다 (docs/design.md 3.2절).
const QUALITY: u8 = 55;

/// 명령행 인자.
#[derive(Debug, PartialEq)]
struct Args {
    input: PathBuf,
    output: PathBuf,
    /// 노이즈 제거 강도. 0이면 끈다.
    denoise: f32,
}

const USAGE: &str = "사용법: amsil [--denoise[=강도 0~1]] <입력 RAW> <출력.heic>";

fn parse_args(args: impl IntoIterator<Item = OsString>) -> std::result::Result<Args, String> {
    let mut denoise = 0.0;
    let mut paths = Vec::new();
    for arg in args {
        match arg.to_str() {
            Some("--denoise") => denoise = 1.0,
            Some(option) if option.starts_with("--denoise=") => {
                denoise = option["--denoise=".len()..]
                    .parse()
                    .ok()
                    .filter(|strength| (0.0..=1.0).contains(strength))
                    .ok_or("노이즈 제거 강도는 0에서 1 사이의 숫자여야 합니다")?;
            }
            Some(option) if option.starts_with("--") => {
                return Err(format!("모르는 옵션: {option}"));
            }
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    match <[PathBuf; 2]>::try_from(paths) {
        Ok([input, output]) => Ok(Args {
            input,
            output,
            denoise,
        }),
        Err(_) => Err("입력 파일과 출력 파일을 하나씩 지정해야 합니다".into()),
    }
}

/// RAW 한 장을 HEIF로 바꾼다. `denoise`는 모델과 강도(0~1).
fn convert(
    input: &Path,
    output: &Path,
    quality: u8,
    denoise: Option<(&mut Denoiser, f32)>,
) -> Result<()> {
    let raw = raw::decode(input)?;
    let cam = match denoise {
        Some((model, strength)) if strength > 0.0 => {
            let full = denoise::run(&raw.packed_bayer(), |tile| model.infer(tile))?;
            let denoised = raw.crop_model_output(&full)?;
            // 강도가 1이면 일반 디모자익은 필요 없다.
            if strength < 1.0 {
                denoise::mix(raw.demosaic()?, &denoised, strength)
            } else {
                denoised
            }
        }
        _ => raw.demosaic()?,
    };
    let image = develop::develop(&cam, raw.wb, raw.xyz_to_cam);
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

/// 노이즈 제거 모델을 읽는다. GPU를 못 쓰면 느리다는 것과 해결 방법을 알린다.
fn load_denoiser() -> Result<Denoiser> {
    let path = gpu::model_dir().join(gpu::DENOISE_MODEL);
    if !path.exists() {
        return Err(format!(
            "노이즈 제거 모델이 없습니다: {}\nscripts/fetch-models.sh 를 실행해 받으세요.",
            path.display()
        )
        .into());
    }
    let denoiser = Denoiser::load(&path)?;
    if let gpu::Backend::Cpu { reason } = &denoiser.backend {
        // 2,400만 화소 사진 한 장이 48타일이다.
        eprintln!(
            "경고: GPU 가속을 쓸 수 없어 CPU로 실행합니다. 노이즈 제거가 느립니다 \
             (2,400만 화소 사진 한 장에 약 {:.0}초).\n\
             해결: Vulkan 드라이버를 설치하세요 (NVIDIA는 전용 드라이버, AMD·Intel은 mesa-vulkan-drivers). \
             `vulkaninfo`로 확인할 수 있습니다.\n원인: {reason}",
            denoiser.tile_seconds * 48.0
        );
    }
    Ok(denoiser)
}

fn run(args: &Args) -> Result<()> {
    let mut denoiser = if args.denoise > 0.0 {
        Some(load_denoiser()?)
    } else {
        None
    };
    let denoise = denoiser.as_mut().map(|model| (model, args.denoise));
    convert(&args.input, &args.output, QUALITY, denoise)
}

fn main() -> std::process::ExitCode {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    let started = std::time::Instant::now();
    match run(&args) {
        Ok(()) => {
            eprintln!(
                "완료: {} ({:.1}초)",
                args.output.display(),
                started.elapsed().as_secs_f32()
            );
            std::process::ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("실패: {}: {e}", args.input.display());
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

        convert(&input, &output, 55, None).unwrap();

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

    fn args(list: &[&str]) -> std::result::Result<Args, String> {
        parse_args(list.iter().map(OsString::from))
    }

    #[test]
    fn parses_input_output_and_denoise_strength() {
        let plain = args(&["a.ARW", "b.heic"]).unwrap();
        assert_eq!(
            plain,
            Args {
                input: "a.ARW".into(),
                output: "b.heic".into(),
                denoise: 0.0
            }
        );
        assert_eq!(
            args(&["--denoise", "a.ARW", "b.heic"]).unwrap().denoise,
            1.0
        );
        assert_eq!(
            args(&["a.ARW", "--denoise=0.5", "b.heic"]).unwrap().denoise,
            0.5
        );
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(args(&["a.ARW"]).is_err());
        assert!(args(&["a.ARW", "b.heic", "c.heic"]).is_err());
        assert!(args(&["--denoise=2", "a.ARW", "b.heic"]).is_err());
        assert!(args(&["--denoise=abc", "a.ARW", "b.heic"]).is_err());
        assert!(args(&["--unknown", "a.ARW", "b.heic"]).is_err());
    }

    /// 출력 파일을 읽어 채널별 평균(0..255)과 크기를 돌려준다.
    fn stats(path: &Path) -> ((u32, u32), [f64; 3]) {
        use libheif_rs::{ColorSpace, LibHeif, RgbChroma};
        let ctx = HeifContext::read_from_file(path.to_str().unwrap()).unwrap();
        let handle = ctx.primary_image_handle().unwrap();
        let image = LibHeif::new()
            .decode(&handle, ColorSpace::Rgb(RgbChroma::Rgb), None)
            .unwrap();
        let planes = image.planes();
        let p = planes.interleaved.unwrap();
        let mut sum = [0f64; 3];
        for row in p.data.chunks(p.stride).take(p.height as usize) {
            for px in row[..p.width as usize * 3].chunks_exact(3) {
                (0..3).for_each(|c| sum[c] += px[c] as f64);
            }
        }
        (
            (p.width, p.height),
            sum.map(|s| s / (p.width as f64 * p.height as f64)),
        )
    }

    #[test]
    fn denoising_changes_neither_size_nor_brightness() {
        let (Some(input), Some(mut model)) = (sample_arw(), gpu::test_denoiser()) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("amsil-test-denoise-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (plain, denoised) = (dir.join("plain.heic"), dir.join("denoised.heic"));

        convert(&input, &plain, 55, None).unwrap();
        convert(&input, &denoised, 55, Some((&mut model, 1.0))).unwrap();

        let ((size_a, mean_a), (size_b, mean_b)) = (stats(&plain), stats(&denoised));
        assert_eq!(size_a, size_b);
        for c in 0..3 {
            assert!(
                (mean_a[c] - mean_b[c]).abs() < 0.02 * mean_a[c],
                "채널 {c}: {mean_a:?} 대 {mean_b:?}"
            );
        }
        // 실제로 모델을 거쳤다면 결과가 완전히 같을 수는 없다.
        assert_ne!(
            std::fs::read(&plain).unwrap(),
            std::fs::read(&denoised).unwrap()
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
