//! 명령행 도구.

use crate::edit::Params;
use crate::export::{self, GB, Photo, Report, Settings};
use crate::{Result, job, preset};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// 명령행 인자.
#[derive(Debug, PartialEq)]
pub struct Args {
    input: PathBuf,
    output: PathBuf,
    /// 노이즈 제거 강도. 0이면 끈다.
    denoise: f32,
    /// 이미 있는 출력 파일도 다시 만든다 (폴더 모드).
    overwrite: bool,
    /// 동시에 처리할 장수. 없으면 CPU 코어 수.
    jobs: Option<usize>,
    /// 메모리 한도(GB). 없으면 남은 메모리의 70%.
    memory_gb: Option<f64>,
    /// 프리셋의 식별자.
    preset: Option<String>,
    /// 프리셋의 강도 (0~1.5).
    strength: f32,
    /// 마지막에 입힐 LUT 파일.
    lut: Option<PathBuf>,
    /// LUT의 강도 (0~1).
    lut_strength: f32,
}

pub const USAGE: &str = "사용법: amsil [옵션] <입력 RAW> <출력.heic>
        amsil [옵션] <입력 폴더> <출력 폴더>     하위 폴더까지 전부 변환
옵션:
  --denoise[=강도]  AI 노이즈 제거 (강도 0~1, 생략하면 1)
  --preset=이름     프리셋 적용 (food, portrait, landscape, night, mono 또는 직접 만든 것)
  --strength=강도   프리셋의 강도 (0~1.5, 기본 1)
  --lut=파일        3D LUT(.cube)을 입힌다
  --lut-strength=강도  LUT의 강도 (0~1, 기본 1)
  --overwrite       이미 있는 출력 파일도 다시 만든다 (기본은 건너뜀)
  --jobs=N          동시에 처리할 장수 (기본: CPU 코어 수)
  --memory=GB       사용할 메모리 한도 (기본: 남은 메모리의 70%)";

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> std::result::Result<Args, String> {
    let (mut denoise, mut overwrite, mut jobs, mut memory_gb) = (0.0, false, None, None);
    let (mut preset, mut strength) = (None, 1.0);
    let (mut lut, mut lut_strength) = (None, 1.0);
    let mut paths = Vec::new();
    for arg in args {
        let Some(option) = arg.to_str().filter(|text| text.starts_with("--")) else {
            paths.push(PathBuf::from(arg));
            continue;
        };
        let (name, value) = match option.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (option, None),
        };
        match (name, value) {
            ("--denoise", None) => denoise = 1.0,
            ("--denoise", Some(value)) => {
                denoise = value
                    .parse()
                    .ok()
                    .filter(|strength| (0.0..=1.0).contains(strength))
                    .ok_or("노이즈 제거 강도는 0에서 1 사이의 숫자여야 합니다")?;
            }
            ("--overwrite", None) => overwrite = true,
            ("--jobs", Some(value)) => {
                jobs = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|&n| n >= 1)
                        .ok_or("--jobs에는 1 이상의 정수를 줘야 합니다")?,
                );
            }
            ("--memory", Some(value)) => {
                memory_gb = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|&gb| gb > 0.0)
                        .ok_or("--memory에는 0보다 큰 숫자(GB)를 줘야 합니다")?,
                );
            }
            ("--preset", Some(value)) => preset = Some(value.to_owned()),
            ("--strength", Some(value)) => {
                strength = value
                    .parse()
                    .ok()
                    .filter(|strength| (0.0..=1.5).contains(strength))
                    .ok_or("프리셋 강도는 0에서 1.5 사이의 숫자여야 합니다")?;
            }
            ("--lut", Some(value)) => lut = Some(PathBuf::from(value)),
            ("--lut-strength", Some(value)) => {
                lut_strength = value
                    .parse()
                    .ok()
                    .filter(|strength| (0.0..=1.0).contains(strength))
                    .ok_or("LUT 강도는 0에서 1 사이의 숫자여야 합니다")?;
            }
            _ => return Err(format!("모르는 옵션: {option}")),
        }
    }
    match <[PathBuf; 2]>::try_from(paths) {
        Ok([input, output]) => Ok(Args {
            input,
            output,
            denoise,
            overwrite,
            jobs,
            memory_gb,
            preset,
            strength,
            lut,
            lut_strength,
        }),
        Err(_) => Err("입력과 출력을 하나씩 지정해야 합니다".into()),
    }
}

/// 명령행에서 고른 프리셋과 강도로 보정 값을 정한다.
fn look(args: &Args) -> Result<Params> {
    let mut presets = preset::builtin();
    presets.extend(preset::load_dir(&crate::project::Config::path().with_file_name("presets")).0);
    if let Some(id) = &args.preset
        && !presets.iter().any(|p| &p.id == id)
    {
        let known: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        return Err(format!(
            "모르는 프리셋: {id}\n쓸 수 있는 프리셋: {}",
            known.join(", ")
        )
        .into());
    }
    Ok(preset::resolve(
        &presets,
        args.preset.as_deref(),
        args.strength,
        &Params::default(),
    ))
}

/// 노이즈 제거를 쓸 때만 모델을 읽는다. CPU로 돌게 되면 경고를 출력한다.
fn load_denoiser(needed: bool) -> Result<Option<Mutex<crate::gpu::Denoiser>>> {
    if !needed {
        return Ok(None);
    }
    let denoiser = export::load_denoiser()?;
    if let Some(notice) = export::cpu_notice(&denoiser) {
        eprintln!("경고: {notice}");
    }
    Ok(Some(Mutex::new(denoiser)))
}

/// 폴더 전체를 변환한다. 진행 상황은 `report`로 알린다.
fn run_folder(
    args: &Args,
    report: impl FnMut(&Report),
) -> Result<(job::Outcome, export::Warnings)> {
    let files = job::scan(&args.input)?;
    if files.is_empty() {
        return Err("RAW 파일을 찾지 못했습니다".into());
    }
    let params = look(args)?;
    let photos: Vec<Photo> = files
        .iter()
        .map(|file| Photo {
            input: args.input.join(file),
            output: job::output_path(&args.output, file),
            denoise: args.denoise,
            params,
            lut: args.lut.clone().map(|file| (file, args.lut_strength)),
        })
        .collect();
    let denoiser = load_denoiser(args.denoise > 0.0)?;
    let settings = Settings {
        overwrite: args.overwrite,
        jobs: args.jobs,
        memory_gb: args.memory_gb,
        ..Settings::default()
    };
    export::run(
        &photos,
        &settings,
        denoiser.as_ref(),
        &AtomicBool::new(false),
        report,
    )
}

/// 진행 상황 한 줄.
fn status_line(r: &Report) -> String {
    let total = r.status.total.max(1) as f64;
    let filled = (r.status.progress / total * 20.0) as usize;
    let mut line = format!(
        "[{}{}] {}/{} · {:.0}% · {} · CPU {:.0}% · 메모리 {:.1}/{:.1}GB · GPU {:.0}% · 처리 중 {}장",
        "#".repeat(filled.min(20)),
        ".".repeat(20 - filled.min(20)),
        r.status.finished,
        r.status.total,
        r.status.progress / total * 100.0,
        job::describe_remaining(r.remaining, r.idle),
        r.cpu,
        r.memory as f64 / GB,
        r.memory_limit as f64 / GB,
        r.gpu,
        r.status.running,
    );
    if r.low_memory {
        line.push_str(" · 메모리가 부족해 동시 처리 수를 줄였습니다");
    }
    line
}

pub fn run(args: &Args) -> Result<()> {
    if !args.input.is_dir() {
        let denoiser = load_denoiser(args.denoise > 0.0)?;
        let denoise = denoiser.as_ref().map(|model| (model, args.denoise));
        let lut = args.lut.as_deref().map(crate::lut::Lut::load).transpose()?;
        let warning = export::convert(
            &args.input,
            &args.output,
            export::QUALITY,
            export::cores(),
            denoise,
            &look(args)?,
            lut.as_ref().map(|lut| (lut, args.lut_strength)),
            &|_| {},
        )?;
        if let Some(warning) = warning {
            eprintln!("경고: {warning}");
        }
        eprintln!("완료: {}", args.output.display());
        return Ok(());
    }

    // 터미널이면 한 줄을 계속 고쳐 쓰고, 아니면(파일로 보낼 때) 5초에 한 줄씩 쓴다.
    let terminal = std::io::stderr().is_terminal();
    let mut printed = Instant::now() - std::time::Duration::from_secs(60);
    let (outcome, warnings) = run_folder(args, |report| {
        let interval = if terminal { 1.0 } else { 5.0 };
        if printed.elapsed().as_secs_f32() >= interval {
            printed = Instant::now();
            if terminal {
                eprint!("\r\x1b[K{}", status_line(report))
            } else {
                eprintln!("{}", status_line(report))
            }
        }
    })?;
    if terminal {
        eprintln!();
    }
    eprintln!(
        "완료 {}장, 건너뜀 {}장, 실패 {}장",
        outcome.done,
        outcome.skipped,
        outcome.failed.len()
    );
    for (file, warning) in &warnings {
        eprintln!("  경고: {}: {warning}", file.display());
    }
    for (file, reason) in &outcome.failed {
        eprintln!("  실패: {}: {reason}", file.display());
    }
    match outcome.stopped {
        Some(reason) => Err(format!("끝까지 처리하지 못했습니다: {reason}").into()),
        None if !outcome.failed.is_empty() => Err("일부 사진을 변환하지 못했습니다".into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample_arw;

    fn args(list: &[&str]) -> std::result::Result<Args, String> {
        parse_args(list.iter().map(OsString::from))
    }

    #[test]
    fn parses_input_output_and_denoise_strength() {
        let plain = args(&["a.ARW", "b.heic"]).unwrap();
        assert_eq!(
            (plain.input, plain.output),
            ("a.ARW".into(), "b.heic".into())
        );
        assert_eq!(
            (plain.denoise, plain.overwrite, plain.jobs, plain.memory_gb),
            (0.0, false, None, None)
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
    fn parses_batch_options() {
        let parsed = args(&["--overwrite", "--jobs=4", "--memory=12.5", "in", "out"]).unwrap();
        assert_eq!(
            (parsed.overwrite, parsed.jobs, parsed.memory_gb),
            (true, Some(4), Some(12.5))
        );
    }

    #[test]
    fn parses_preset_and_strength() {
        let parsed = args(&["--preset=food", "--strength=0.5", "in", "out"]).unwrap();
        assert_eq!(
            (parsed.preset.as_deref(), parsed.strength),
            (Some("food"), 0.5)
        );
        let plain = args(&["in", "out"]).unwrap();
        assert_eq!((plain.preset, plain.strength), (None, 1.0));
        assert!(args(&["--strength=9", "in", "out"]).is_err());
    }

    #[test]
    fn preset_option_becomes_scaled_params_and_an_unknown_name_lists_the_choices() {
        let half = look(&args(&["--preset=mono", "--strength=0.5", "in", "out"]).unwrap()).unwrap();
        assert_eq!(half.mono, 0.5);
        assert!(look(&args(&["in", "out"]).unwrap()).unwrap().is_neutral());
        let error = look(&args(&["--preset=없는것", "in", "out"]).unwrap())
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("없는것") && error.contains("food") && error.contains("mono"),
            "{error}"
        );
    }

    #[test]
    fn parses_lut_file_and_strength() {
        let parsed = args(&["--lut=필름.cube", "--lut-strength=0.5", "in", "out"]).unwrap();
        assert_eq!(
            (parsed.lut.as_deref(), parsed.lut_strength),
            (Some(std::path::Path::new("필름.cube")), 0.5)
        );
        let plain = args(&["in", "out"]).unwrap();
        assert_eq!((plain.lut, plain.lut_strength), (None, 1.0));
        assert!(args(&["--lut-strength=3", "in", "out"]).is_err());
    }

    #[test]
    fn rejects_bad_arguments() {
        assert!(args(&["a.ARW"]).is_err());
        assert!(args(&["a.ARW", "b.heic", "c.heic"]).is_err());
        assert!(args(&["--denoise=2", "a.ARW", "b.heic"]).is_err());
        assert!(args(&["--denoise=abc", "a.ARW", "b.heic"]).is_err());
        assert!(args(&["--unknown", "a.ARW", "b.heic"]).is_err());
        assert!(args(&["--jobs=0", "in", "out"]).is_err());
        assert!(args(&["--memory=abc", "in", "out"]).is_err());
        assert!(args(&["--overwrite=yes", "in", "out"]).is_err());
    }

    #[test]
    fn converts_a_folder_keeping_its_structure_and_skips_finished_files_on_rerun() {
        let Some(sample) = sample_arw() else { return };
        let dir = std::env::temp_dir().join(format!("amsil-test-folder-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("in/sub")).unwrap();
        std::os::unix::fs::symlink(&sample, dir.join("in/a.ARW")).unwrap();
        std::os::unix::fs::symlink(&sample, dir.join("in/sub/b.ARW")).unwrap();
        std::fs::write(dir.join("in/notes.txt"), "RAW가 아닌 파일").unwrap();
        let args = Args {
            input: dir.join("in"),
            output: dir.join("out"),
            denoise: 0.0,
            overwrite: false,
            jobs: Some(2),
            memory_gb: None,
            preset: None,
            strength: 1.0,
            lut: None,
            lut_strength: 1.0,
        };

        let mut last = None;
        let (outcome, warnings) = run_folder(&args, |report| last = Some(report.status)).unwrap();
        assert!(warnings.is_empty());
        assert_eq!(
            (outcome.done, outcome.skipped, outcome.failed.len()),
            (2, 0, 0)
        );
        assert!(dir.join("out/a.heic").exists() && dir.join("out/sub/b.heic").exists());
        let last = last.unwrap();
        assert_eq!((last.total, last.finished), (2, 2));

        let (again, _) = run_folder(&args, |_| {}).unwrap();
        assert_eq!(
            (again.done, again.skipped),
            (0, 2),
            "이미 만든 파일은 건너뛴다"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn status_line_shows_progress_time_and_resources() {
        let report = Report {
            status: job::Status {
                total: 309,
                finished: 142,
                progress: 154.5,
                running: 12,
            },
            remaining: Some(372.0),
            idle: 0.5,
            cpu: 87.4,
            memory: 18 << 30,
            memory_limit: 24 << 30,
            gpu: 96.0,
            low_memory: false,
        };
        assert_eq!(
            status_line(&report),
            "[##########..........] 142/309 · 50% · 약 6분 남음 · CPU 87% · 메모리 18.0/24.0GB · GPU 96% · 처리 중 12장"
        );
    }
}
