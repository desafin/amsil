//! Command-line tool.

use crate::capture::Capture;
use crate::edit::Params;
use crate::export::{self, Look, Photo, Report, Settings};
use crate::frame::{Frame, Layout};
use crate::lang::{Text, t, text};
use crate::{Result, job, preset};
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::PathBuf;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::time::Instant;

/// Command-line arguments.
#[derive(Debug, PartialEq)]
pub struct Args {
    input: PathBuf,
    output: PathBuf,
    /// Denoise strength. 0 turns it off.
    denoise: f32,
    /// Recreate output files that already exist (folder mode).
    overwrite: bool,
    /// Number of photos processed at once. Defaults to the CPU core count.
    jobs: Option<usize>,
    /// Memory limit (GB). Defaults to 70% of free memory.
    memory_gb: Option<f64>,
    /// Preset identifier.
    preset: Option<String>,
    /// Preset strength (0–1.5).
    strength: f32,
    /// LUT file applied last.
    lut: Option<PathBuf>,
    /// LUT strength (0–1).
    lut_strength: f32,
    /// Frame drawn around the photo.
    frame: Option<Frame>,
    /// Skin smoothing strength (0–1). 0 turns it off.
    skin: f32,
    /// Output quality.
    quality: u8,
    /// Capture sharpening.
    pub capture: Capture,
}

/// Usage text.
pub fn usage() -> &'static str {
    t(
        "사용법: amsil [옵션] <입력 RAW> <출력.heic>
        amsil [옵션] <입력 폴더> <출력 폴더>     하위 폴더까지 전부 변환
옵션:
  --denoise[=강도]     AI 노이즈 제거 (강도 0~1, 생략하면 1)
  --preset=이름        프리셋 적용 (food, portrait, landscape, night, mono 또는 직접 만든 것).
                       auto면 사진마다 내용에 맞는 것을 고른다
  --strength=강도      프리셋의 강도 (0~1.5, 기본 1)
  --lut=파일           3D LUT(.cube)을 입힌다
  --lut-strength=강도  LUT의 강도 (0~1, 기본 1)
  --frame=모양         촬영 정보를 적은 프레임: band(아래 띠), border(테두리), 4:5 같은 비율
  --frame-black        프레임을 검은색으로 (기본은 흰색)
  --skin[=세기]        인물의 피부를 고르게 다듬는다 (실험 기능. 세기 0~1, 생략하면 0.5)
  --quality=N          출력 품질 (40~100, 기본 55. 55가 JPEG 90 수준이다)
  --overwrite          이미 있는 출력 파일도 다시 만든다 (기본은 건너뜀)
  --jobs=N             동시에 처리할 장수 (기본: CPU 코어 수)
  --memory=GB          사용할 메모리 한도 (기본: 남은 메모리의 70%)
  --no-capture         캡처 샤픈을 하지 않는다 (기본은 한다: RAW의 흐림을 되돌린다)
  --capture-iterations=N  캡처 샤픈의 반복 횟수 (1~100, 기본 20. 많을수록 강하게)
  --capture-radius=R   되돌릴 흐림의 반경 (0.4~2, 주지 않으면 사진에서 구한다)
  --capture-threshold=T  대비 문턱 (0~2, 기본 0.4. 높을수록 뚜렷한 경계만. auto면 사진에서 구한다)
  --capture-corner=X   모서리로 갈수록 반경에 더하는 값 (-0.5~0.5, 기본 0)
  --capture-no-auto-stop  넘침이 생겨도 반복을 멈추지 않는다",
        "Usage: amsil [options] <input RAW> <output.heic>
       amsil [options] <input folder> <output folder>     converts everything, subfolders included
Options:
  --denoise[=LEVEL]    AI noise reduction (0 to 1; 1 when no level is given)
  --preset=NAME        apply a preset (food, portrait, landscape, night, mono, or one of your own);
                       auto picks the one that suits each photo
  --strength=X         preset strength (0 to 1.5, default 1)
  --lut=FILE           apply a 3D LUT (.cube)
  --lut-strength=X     LUT strength (0 to 1, default 1)
  --frame=LAYOUT       frame with the shooting info: band (strip below), border, or a ratio such as 4:5
  --frame-black        black frame (white by default)
  --skin[=AMOUNT]      even out skin in portraits (experimental; 0 to 1, 0.5 when no amount is given)
  --quality=N          output quality (40 to 100, default 55; 55 is about JPEG 90)
  --overwrite          redo output files that already exist (skipped by default)
  --jobs=N             photos to process at once (default: number of CPU cores)
  --memory=GB          memory limit (default: 70% of the available memory)
  --no-capture         skip capture sharpening (on by default: it undoes the blur in the RAW)
  --capture-iterations=N  capture sharpening iterations (1 to 100, default 20; more is stronger)
  --capture-radius=R   radius of the blur to undo (0.4 to 2; worked out from the photo if not given)
  --capture-threshold=T  contrast threshold (0 to 2, default 0.4; higher sharpens only clear edges; auto works it out from the photo)
  --capture-corner=X   radius added towards the corners (-0.5 to 0.5, default 0)
  --capture-no-auto-stop  keep iterating even when overshoot appears",
    )
}

pub fn parse_args(args: impl IntoIterator<Item = OsString>) -> std::result::Result<Args, Text> {
    let (mut denoise, mut overwrite, mut jobs, mut memory_gb) = (0.0, false, None, None);
    let (mut preset, mut strength) = (None, 1.0);
    let (mut lut, mut lut_strength) = (None, 1.0);
    let (mut layout, mut frame_black) = (None::<Layout>, false);
    let mut skin = 0.0;
    let mut quality = export::QUALITY;
    let mut capture = Capture::default();
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
                    .ok_or(text!(
                        "노이즈 제거 강도는 0에서 1 사이의 숫자여야 합니다",
                        "The denoise level must be a number from 0 to 1",
                    ))?;
            }
            ("--overwrite", None) => overwrite = true,
            ("--jobs", Some(value)) => {
                jobs = Some(value.parse().ok().filter(|&n| n >= 1).ok_or(text!(
                    "--jobs에는 1 이상의 정수를 줘야 합니다",
                    "--jobs needs a whole number, 1 or more",
                ))?);
            }
            ("--memory", Some(value)) => {
                memory_gb = Some(value.parse().ok().filter(|&gb| gb > 0.0).ok_or(text!(
                    "--memory에는 0보다 큰 숫자(GB)를 줘야 합니다",
                    "--memory needs a number above 0 (in GB)",
                ))?);
            }
            ("--preset", Some(value)) => preset = Some(value.to_owned()),
            ("--strength", Some(value)) => {
                strength = value
                    .parse()
                    .ok()
                    .filter(|strength| (0.0..=1.5).contains(strength))
                    .ok_or(text!(
                        "프리셋 강도는 0에서 1.5 사이의 숫자여야 합니다",
                        "The preset strength must be a number from 0 to 1.5",
                    ))?;
            }
            ("--lut", Some(value)) => lut = Some(PathBuf::from(value)),
            ("--lut-strength", Some(value)) => {
                lut_strength = value
                    .parse()
                    .ok()
                    .filter(|strength| (0.0..=1.0).contains(strength))
                    .ok_or(text!(
                        "LUT 강도는 0에서 1 사이의 숫자여야 합니다",
                        "The LUT strength must be a number from 0 to 1",
                    ))?;
            }
            ("--frame", Some(value)) => layout = Some(value.parse()?),
            ("--frame-black", None) => frame_black = true,
            ("--quality", Some(value)) => {
                quality = value
                    .parse()
                    .ok()
                    .filter(|quality| export::QUALITY_RANGE.contains(quality))
                    .ok_or(text!(
                        "품질은 40에서 100 사이의 정수여야 합니다",
                        "The quality must be a whole number from 40 to 100",
                    ))?;
            }
            ("--skin", None) => skin = 0.5,
            ("--skin", Some(value)) => {
                skin = value
                    .parse()
                    .ok()
                    .filter(|amount| (0.0..=1.0).contains(amount))
                    .ok_or(text!(
                        "피부 보정의 세기는 0에서 1 사이의 숫자여야 합니다",
                        "The skin amount must be a number from 0 to 1",
                    ))?;
            }
            ("--no-capture", None) => capture.enabled = false,
            ("--capture-no-auto-stop", None) => capture.auto_stop = false,
            ("--capture-iterations", Some(value)) => {
                capture.iterations = value
                    .parse()
                    .ok()
                    .filter(|n| crate::capture::ITERATIONS.contains(n))
                    .ok_or(text!(
                        "캡처 샤픈의 반복 횟수는 1에서 100 사이의 정수여야 합니다",
                        "The capture sharpening iterations must be a whole number from 1 to 100",
                    ))?;
            }
            ("--capture-radius", Some(value)) => {
                capture.radius = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|r| crate::capture::RADIUS.contains(r))
                        .ok_or(text!(
                            "캡처 샤픈의 반경은 0.4에서 2 사이의 숫자여야 합니다",
                            "The capture sharpening radius must be a number from 0.4 to 2",
                        ))?,
                );
            }
            ("--capture-threshold", Some("auto")) => capture.threshold = None,
            ("--capture-threshold", Some(value)) => {
                capture.threshold = Some(
                    value
                        .parse()
                        .ok()
                        .filter(|t| crate::capture::THRESHOLD.contains(t))
                        .ok_or(text!(
                            "캡처 샤픈의 대비 문턱은 0에서 2 사이의 숫자여야 합니다",
                            "The capture sharpening threshold must be a number from 0 to 2",
                        ))?,
                );
            }
            ("--capture-corner", Some(value)) => {
                capture.corner = value
                    .parse()
                    .ok()
                    .filter(|c| crate::capture::CORNER.contains(c))
                    .ok_or(text!(
                        "캡처 샤픈의 모서리 반경은 -0.5에서 0.5 사이의 숫자여야 합니다",
                        "The capture sharpening corner boost must be a number from -0.5 to 0.5",
                    ))?;
            }
            _ => {
                return Err(text!(
                    format!("모르는 옵션: {option}"),
                    format!("Unknown option: {option}"),
                ));
            }
        }
    }
    if frame_black && layout.is_none() {
        return Err(text!(
            "--frame-black은 --frame과 함께 써야 합니다",
            "--frame-black needs --frame",
        ));
    }
    let frame = layout.map(|layout| Frame {
        layout,
        dark: frame_black,
    });
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
            frame,
            skin,
            quality,
            capture,
        }),
        Err(_) => Err(text!(
            "입력과 출력을 하나씩 지정해야 합니다",
            "Give exactly one input and one output",
        )),
    }
}

/// Selectable presets: the built-in ones followed by user-made ones (same list as the GUI).
fn presets() -> Vec<preset::Preset> {
    let mut presets = preset::builtin();
    presets.extend(preset::load_dir(&crate::project::Config::path().with_file_name("presets")).0);
    presets
}

/// Passing this name to `--preset` picks a preset per photo based on its content.
const AUTO: &str = "auto";

/// Per-photo adjustments. With `--preset=auto` each photo is classified and gets a matching
/// preset; otherwise every photo gets the same values.
fn params_for(args: &Args, inputs: &[PathBuf]) -> Result<Vec<Params>> {
    if args.preset.as_deref() == Some(AUTO) {
        auto_params(args, inputs)
    } else {
        Ok(vec![params(args)?; inputs.len()])
    }
}

/// Classifies the photos and picks the matching preset's adjustments for each. Photos with no
/// match are left unadjusted.
fn auto_params(args: &Args, inputs: &[PathBuf]) -> Result<Vec<Params>> {
    let presets = presets();
    let mut classifier = crate::recommend::load_classifier()?;
    // ponytail: classifies one photo at a time (about 0.1 s each). Split across threads for
    // thousands of photos.
    Ok(inputs
        .iter()
        .map(|input| {
            // Unreadable photos are skipped here. Conversion then fails just that photo, with
            // the reason.
            let guess = crate::preview::thumbnail(input, 448)
                .ok()
                .flatten()
                .and_then(|picture| classifier.classify(&picture).ok());
            let preset = guess.and_then(|guess| crate::recommend::preset_for(&presets, &guess));
            preset::resolve(
                &presets,
                preset.map(|preset| preset.id.as_str()),
                args.strength,
                &Params::default(),
            )
        })
        .collect())
}

/// Builds the adjustments from the preset and strength chosen on the command line.
fn params(args: &Args) -> Result<Params> {
    let presets = presets();
    if let Some(id) = &args.preset
        && !presets.iter().any(|p| &p.id == id)
    {
        let known: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        let known = known.join(", ");
        return Err(text!(
            format!("모르는 프리셋: {id}\n쓸 수 있는 프리셋: {known}"),
            format!("Unknown preset: {id}\nAvailable presets: {known}"),
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

/// Loads the model only when denoising is on. Prints a warning if it falls back to the CPU.
fn load_denoiser(needed: bool) -> Result<Option<Mutex<crate::gpu::Denoiser>>> {
    if !needed {
        return Ok(None);
    }
    let denoiser = export::load_denoiser()?;
    if let Some(notice) = export::cpu_notice(&denoiser) {
        eprintln!("{}: {notice}", t("경고", "Warning"));
    }
    Ok(Some(Mutex::new(denoiser)))
}

/// Loads the skin detection model only when skin smoothing is on.
fn load_skin_finder(needed: bool) -> Result<Option<Mutex<crate::skin::SkinFinder>>> {
    if !needed {
        return Ok(None);
    }
    Ok(Some(Mutex::new(crate::skin::load_finder()?)))
}

/// Converts a whole folder. Progress is reported through `report`.
fn run_folder(
    args: &Args,
    report: impl FnMut(&Report),
) -> Result<(job::Outcome, export::Warnings)> {
    let files = job::scan(&args.input)?;
    if files.is_empty() {
        return Err(text!("RAW 파일을 찾지 못했습니다", "No RAW files found").into());
    }
    let inputs: Vec<PathBuf> = files.iter().map(|file| args.input.join(file)).collect();
    let params = params_for(args, &inputs)?;
    let photos: Vec<Photo> = files
        .iter()
        .zip(params)
        .map(|(file, params)| Photo {
            input: args.input.join(file),
            output: job::output_path(&args.output, file),
            denoise: args.denoise,
            params,
            lut: args.lut.clone().map(|file| (file, args.lut_strength)),
            frame: args.frame,
            masks: Vec::new(),
            skin: args.skin,
            capture: args.capture,
        })
        .collect();
    let denoiser = load_denoiser(args.denoise > 0.0)?;
    let skin_finder = load_skin_finder(args.skin > 0.0)?;
    let settings = Settings {
        quality: args.quality,
        overwrite: args.overwrite,
        jobs: args.jobs,
        memory_gb: args.memory_gb,
    };
    export::run(
        &photos,
        &settings,
        denoiser.as_ref(),
        None,
        skin_finder.as_ref(),
        &AtomicBool::new(false),
        report,
    )
}

/// One line of progress.
fn status_line(r: &Report) -> String {
    let total = r.status.total.max(1) as f64;
    let filled = (r.status.progress / total * 20.0) as usize;
    format!(
        "[{}{}] {}/{} · {:.0}% · {} · {}",
        "#".repeat(filled.min(20)),
        ".".repeat(20 - filled.min(20)),
        r.status.finished,
        r.status.total,
        r.status.progress / total * 100.0,
        job::describe_remaining(r.remaining, r.idle),
        job::describe_resources(
            r.cpu,
            r.memory,
            r.memory_limit,
            r.gpu,
            r.status.running,
            r.low_memory
        ),
    )
}

pub fn run(args: &Args) -> Result<()> {
    if !args.input.is_dir() {
        let denoiser = load_denoiser(args.denoise > 0.0)?;
        let denoise = denoiser.as_ref().map(|model| (model, args.denoise));
        let lut = args.lut.as_deref().map(crate::lut::Lut::load).transpose()?;
        let skin_finder = load_skin_finder(args.skin > 0.0)?;
        let warning = export::convert(
            &args.input,
            &args.output,
            args.quality,
            export::cores(),
            denoise,
            Look {
                lut: lut.as_ref().map(|lut| (lut, args.lut_strength)),
                frame: args.frame.as_ref(),
                skin: args.skin,
                skin_finder: skin_finder.as_ref(),
                capture: args.capture,
                ..Look::new(&params_for(args, std::slice::from_ref(&args.input))?[0])
            },
            &|_| {},
        )?;
        if let Some(warning) = warning {
            crate::log::warn(format_args!("{}: {warning}", args.input.display()));
            eprintln!("{}: {warning}", t("경고", "Warning"));
        }
        crate::log::info(format!("Done: {}", args.output.display()));
        eprintln!("{}: {}", t("완료", "Done"), args.output.display());
        return Ok(());
    }

    // On a terminal, keep rewriting one line; otherwise (redirected to a file) write a line
    // every 5 s.
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
    let (done, skipped, failed) = (outcome.done, outcome.skipped, outcome.failed.len());
    eprintln!(
        "{}",
        t(
            format!("완료 {done}장, 건너뜀 {skipped}장, 실패 {failed}장"),
            format!("Done {done}, skipped {skipped}, failed {failed}"),
        )
    );
    for (file, warning) in &warnings {
        eprintln!("  {}: {}: {warning}", t("경고", "Warning"), file.display());
    }
    for (file, reason) in &outcome.failed {
        eprintln!("  {}: {}: {reason}", t("실패", "Failed"), file.display());
    }
    match outcome.stopped {
        Some(reason) => Err(text!(
            format!("끝까지 처리하지 못했습니다: {reason}"),
            format!("Stopped before finishing: {reason}"),
        )
        .into()),
        None if !outcome.failed.is_empty() => Err(text!(
            "일부 사진을 변환하지 못했습니다",
            "Some photos could not be converted",
        )
        .into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample_arw;

    fn args(list: &[&str]) -> std::result::Result<Args, Text> {
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
        let half =
            params(&args(&["--preset=mono", "--strength=0.5", "in", "out"]).unwrap()).unwrap();
        assert_eq!(half.mono, 0.5);
        assert!(params(&args(&["in", "out"]).unwrap()).unwrap().is_neutral());
        let error = params(&args(&["--preset=없는것", "in", "out"]).unwrap())
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
            frame: None,
            skin: 0.0,
            quality: export::QUALITY,
            capture: Capture::off(),
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
            "files already made are skipped"
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

    #[test]
    fn capture_sharpening_options() {
        assert_eq!(args(&["in", "out"]).unwrap().capture, Capture::default());
        assert!(
            !args(&["--no-capture", "in", "out"])
                .unwrap()
                .capture
                .enabled
        );
        let a = args(&[
            "--capture-iterations=40",
            "--capture-radius=0.9",
            "--capture-threshold=0.2",
            "--capture-corner=0.3",
            "--capture-no-auto-stop",
            "in",
            "out",
        ])
        .unwrap()
        .capture;
        assert_eq!(
            (a.iterations, a.radius, a.threshold, a.corner, a.auto_stop),
            (40, Some(0.9), Some(0.2), 0.3, false)
        );
        assert_eq!(
            args(&["--capture-threshold=auto", "in", "out"])
                .unwrap()
                .capture
                .threshold,
            None
        );
        for bad in [
            "--capture-iterations=0",
            "--capture-radius=3",
            "--capture-threshold=-1",
            "--capture-corner=0.8",
        ] {
            assert!(args(&[bad, "in", "out"]).is_err(), "{bad}");
        }
        assert!(usage().contains("--capture-iterations"));
        crate::lang::set(crate::lang::Lang::En);
        assert!(usage().contains("--no-capture"));
    }

    #[test]
    fn usage_and_errors_follow_the_language() {
        assert!(usage().starts_with("사용법"));
        assert_eq!(
            args(&["--nope", "in", "out"]).unwrap_err().to_string(),
            "모르는 옵션: --nope"
        );
        crate::lang::set(crate::lang::Lang::En);
        assert!(usage().starts_with("Usage"));
        assert!(usage().contains("--lut-strength"));
        assert_eq!(
            args(&["--nope", "in", "out"]).unwrap_err().to_string(),
            "Unknown option: --nope"
        );
        assert_eq!(
            args(&["in"]).unwrap_err().to_string(),
            "Give exactly one input and one output"
        );
    }

    #[test]
    fn parses_the_frame_layout_and_colour() {
        use crate::frame::{Frame, Layout};
        assert_eq!(args(&["in", "out"]).unwrap().frame, None);
        assert_eq!(
            args(&["--frame=band", "in", "out"]).unwrap().frame,
            Some(Frame {
                layout: Layout::Band,
                dark: false
            })
        );
        assert_eq!(
            args(&["--frame=4:5", "--frame-black", "in", "out"])
                .unwrap()
                .frame,
            Some(Frame {
                layout: Layout::Ratio(4, 5),
                dark: true
            })
        );
        assert!(
            args(&["--frame=round", "in", "out"])
                .unwrap_err()
                .to_string()
                .contains("round")
        );
        assert!(
            args(&["--frame-black", "in", "out"]).is_err(),
            "colour without a shape is reported"
        );
        assert!(usage().contains("--frame="));
    }

    #[test]
    fn auto_preset_picks_by_what_each_photo_shows() {
        // Runs only when the classifier model and the sample photo are present.
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110");
        if !dir.exists() || crate::recommend::load_classifier().is_err() {
            eprintln!("Classifier model or sample photo missing, skipping");
            return;
        }
        let args = args(&["--preset=auto", "--strength=0.5", "in", "out"]).unwrap();
        let inputs = [dir.join("DSC04558.ARW"), dir.join("DSC04545.ARW")];
        let chosen = auto_params(&args, &inputs).unwrap();
        let presets = preset::builtin();
        let expected = |id: &str| preset::resolve(&presets, Some(id), 0.5, &Params::default());
        assert_eq!(chosen, [expected("food"), expected("landscape")]);
        assert!(usage().contains("auto"));
    }

    #[test]
    fn parses_the_skin_retouch_amount() {
        assert_eq!(args(&["in", "out"]).unwrap().skin, 0.0);
        assert_eq!(args(&["--skin", "in", "out"]).unwrap().skin, 0.5);
        assert_eq!(args(&["--skin=0.8", "in", "out"]).unwrap().skin, 0.8);
        assert!(args(&["--skin=2", "in", "out"]).is_err());
        assert!(usage().contains("--skin"));
    }

    #[test]
    fn parses_the_quality() {
        assert_eq!(args(&["in", "out"]).unwrap().quality, 55);
        assert_eq!(args(&["--quality=90", "in", "out"]).unwrap().quality, 90);
        assert_eq!(args(&["--quality=100", "in", "out"]).unwrap().quality, 100);
        for bad in [
            "--quality=101",
            "--quality=10",
            "--quality=abc",
            "--quality",
        ] {
            assert!(args(&[bad, "in", "out"]).is_err(), "{bad}");
        }
        assert!(usage().contains("--quality"));
    }

    #[test]
    fn auto_preset_does_not_stop_the_batch_for_an_unreadable_photo() {
        // Runs only when the classifier model is present.
        if crate::recommend::load_classifier().is_err() {
            eprintln!("Classifier model missing, skipping");
            return;
        }
        let dir = std::env::temp_dir().join(format!("amsil-auto-broken-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.ARW");
        std::fs::write(&broken, [0x5a; 4096]).unwrap();
        let args = args(&["--preset=auto", "in", "out"]).unwrap();
        // Photos that could not be classified are left unadjusted. Conversion then fails just that
        // photo, with the reason.
        let chosen = auto_params(&args, std::slice::from_ref(&broken));
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(chosen.unwrap()[0].is_neutral());
    }
}
