mod denoise;
mod develop;
mod gpu;
mod heif;
mod job;
mod raw;

use gpu::Denoiser;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Instant;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// 작업용 이미지. 채널별 평면(R, G, B), 값은 f32.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub planes: [Vec<f32>; 3],
}

/// libheif의 품질 값. 55가 JPEG q90 수준이고 70부터는 JPEG보다 파일이 커진다 (docs/design.md 3.2절).
const QUALITY: u8 = 55;

/// 사진 한 장을 처리하는 데 드는 메모리의 추정값 (센서 화소당 바이트).
// ponytail: 실측으로 정한 상수 (A900 기준 장당 최대 53바이트/화소). 다른 카메라에서 틀려도 시스템 여유 메모리 감시가 받쳐 준다.
const BYTES_PER_PIXEL: u64 = 56;

const GB: f64 = (1u64 << 30) as f64;

/// 명령행 인자.
#[derive(Debug, PartialEq)]
struct Args {
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
}

const USAGE: &str = "사용법: amsil [옵션] <입력 RAW> <출력.heic>
        amsil [옵션] <입력 폴더> <출력 폴더>     하위 폴더까지 전부 변환
옵션:
  --denoise[=강도]  AI 노이즈 제거 (강도 0~1, 생략하면 1)
  --overwrite       이미 있는 출력 파일도 다시 만든다 (기본은 건너뜀)
  --jobs=N          동시에 처리할 장수 (기본: CPU 코어 수)
  --memory=GB       사용할 메모리 한도 (기본: 남은 메모리의 70%)";

fn parse_args(args: impl IntoIterator<Item = OsString>) -> std::result::Result<Args, String> {
    let (mut denoise, mut overwrite, mut jobs, mut memory_gb) = (0.0, false, None, None);
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
        }),
        Err(_) => Err("입력과 출력을 하나씩 지정해야 합니다".into()),
    }
}

/// RAW 한 장을 HEIF로 바꾼다. `denoise`는 모델과 강도(0~1), `progress`에는 진행 정도(0~1)를 알린다.
/// 알려야 할 일이 있으면(품질을 낮춘 경우) 그 문구를 돌려준다.
fn convert(
    input: &Path,
    output: &Path,
    quality: u8,
    encoder_threads: usize,
    denoise: Option<(&Mutex<Denoiser>, f32)>,
    progress: &dyn Fn(f32),
) -> Result<Option<String>> {
    let timing = Timing::start();
    let raw = raw::decode(input)?;
    timing.mark("읽기");
    progress(0.15);
    let cam = match denoise {
        Some((model, strength)) if strength > 0.0 => {
            let packed = raw.packed_bayer();
            let (tiles, done) = (denoise::tile_count(&packed) as f32, AtomicUsize::new(0));
            let full = {
                // GPU는 하나라서 한 번에 한 장씩 처리한다.
                // ponytail: 먼저 잡은 사진이 끝까지 쓴다. 화면에서 누른 작업을 먼저 처리해야 하면 우선순위 큐로 바꾼다.
                let mut model = model.lock().unwrap();
                denoise::run(&packed, |tile| {
                    let out = model.infer(tile);
                    progress(0.15 + 0.45 * done.fetch_add(1, SeqCst) as f32 / tiles);
                    out
                })?
            };
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
    timing.mark("디모자익/노이즈 제거");
    progress(0.6);
    let image = develop::develop(&cam, raw.wb, raw.xyz_to_cam);
    drop(cam);
    let (transpose, flip_h, flip_v) = raw.flips;
    let image = develop::orient(image, transpose, flip_h, flip_v);
    timing.mark("현상");
    progress(0.75);
    let encoded = heif::encode(&image, quality, Some(&raw.exif), encoder_threads)?;
    timing.mark("인코딩");
    progress(0.98);

    // 임시 파일에 쓰고 이름을 바꾼다. 도중에 꺼져도 반쯤 쓰인 출력 파일이 남지 않는다.
    let mut tmp = output.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, encoded.bytes)?;
    std::fs::rename(&tmp, output)?;
    Ok((encoded.quality < quality).then(|| {
        format!(
            "파일이 너무 커져서 품질을 {quality}에서 {}(으)로 낮췄습니다",
            encoded.quality
        )
    }))
}

/// 논리 코어 수.
fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// 단계별 소요 시간. 환경변수 `AMSIL_TIMING`이 있을 때만 출력한다 (성능을 조정할 때 쓴다).
struct Timing(Option<std::cell::Cell<Instant>>);

impl Timing {
    fn start() -> Self {
        Self(std::env::var_os("AMSIL_TIMING").map(|_| std::cell::Cell::new(Instant::now())))
    }

    fn mark(&self, stage: &str) {
        if let Some(last) = &self.0 {
            eprintln!(
                "  {stage}: {:.2}초",
                last.replace(Instant::now()).elapsed().as_secs_f32()
            );
        }
    }
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

/// 배치가 도는 동안 주기적으로 알리는 내용.
struct Report {
    status: job::Status,
    /// 남은 시간(초). 아직 모르면 None.
    remaining: Option<f64>,
    /// 마지막 진행 이후 흐른 시간(초).
    idle: f64,
    /// 이 프로그램의 CPU 사용률 (전체 코어 대비 %).
    cpu: f32,
    /// 이 프로그램이 쓰는 메모리와 한도 (바이트).
    memory: u64,
    memory_limit: u64,
    /// AI 모델이 실제로 돌고 있던 시간의 비율 (%).
    gpu: f32,
    /// 시스템 메모리가 부족해 동시 처리 수를 줄인 상태.
    low_memory: bool,
}

/// 자원 사용량을 재고, 시스템 메모리가 부족해지면 새 작업의 시작을 늦춘다.
struct Monitor {
    system: sysinfo::System,
    pid: sysinfo::Pid,
    cores: f32,
    memory_limit: u64,
    started: Instant,
    eta: job::Eta,
    /// 마지막으로 잰 시각과 그때의 GPU 누적 사용 시간.
    measured: (Instant, u64),
    cpu: f32,
    memory: u64,
    gpu: f32,
    low_memory: bool,
}

impl Monitor {
    fn new(system: sysinfo::System, memory_limit: u64) -> Result<Self> {
        let now = Instant::now();
        Ok(Self {
            system,
            pid: sysinfo::get_current_pid()?,
            cores: cores() as f32,
            memory_limit,
            started: now,
            eta: job::Eta::new(0.0),
            measured: (now, gpu::busy_nanos()),
            cpu: 0.0,
            memory: 0,
            gpu: 0.0,
            low_memory: false,
        })
    }

    fn sample(&mut self, status: &job::Status, budget: &job::Budget) -> Report {
        let now = Instant::now();
        let seconds = now.duration_since(self.started).as_secs_f64();
        self.eta.record(seconds, status.progress);

        // 자원은 1초에 한 번만 잰다.
        let since = now.duration_since(self.measured.0).as_secs_f32();
        if since >= 1.0 {
            use sysinfo::{ProcessRefreshKind, ProcessesToUpdate};
            self.system.refresh_memory();
            let kind = ProcessRefreshKind::nothing().with_cpu().with_memory();
            self.system.refresh_processes_specifics(
                ProcessesToUpdate::Some(&[self.pid]),
                false,
                kind,
            );
            if let Some(process) = self.system.process(self.pid) {
                self.cpu = process.cpu_usage() / self.cores;
                self.memory = process.memory();
            }
            let busy = gpu::busy_nanos();
            self.gpu = ((busy - self.measured.1) as f32 / 1e9 / since * 100.0).min(100.0);
            self.measured = (now, busy);

            // 시스템 전체의 여유 메모리가 10% 아래로 떨어지면 새 작업을 한 번에 하나만 시작한다.
            self.low_memory = self.system.available_memory() < self.system.total_memory() / 10;
            budget.set_low_memory(self.low_memory);
        }
        Report {
            status: *status,
            remaining: self.eta.remaining(status.total as f64),
            idle: self.eta.idle_for(seconds),
            cpu: self.cpu,
            memory: self.memory,
            memory_limit: self.memory_limit,
            gpu: self.gpu,
            low_memory: self.low_memory,
        }
    }
}

/// 동시에 처리할 장수와 인코더 한 개가 쓸 스레드 수를 정한다.
///
/// 메모리가 허락하는 장수만큼만 동시에 돌리고, 남는 코어는 인코더 스레드로 돌린다.
fn plan(
    cores: usize,
    jobs: Option<usize>,
    memory_limit: u64,
    memory_per_photo: u64,
    photos: usize,
) -> (usize, usize) {
    let fits = (memory_limit / memory_per_photo.max(1)) as usize;
    let workers = jobs.unwrap_or(cores).min(fits).min(photos).max(1);
    (workers, (cores / workers).max(1))
}

/// 폴더 전체를 변환한다. 진행 상황은 `report`로 알린다.
fn run_folder(
    args: &Args,
    mut report: impl FnMut(&Report),
) -> Result<(job::Outcome, Vec<(PathBuf, String)>)> {
    let files = job::scan(&args.input)?;
    if files.is_empty() {
        return Err("RAW 파일을 찾지 못했습니다".into());
    }
    let tasks: Vec<job::Task> = files
        .iter()
        .map(|file| job::Task {
            input: args.input.join(file),
            output: job::output_path(&args.output, file),
        })
        .collect();
    let denoiser = if args.denoise > 0.0 {
        Some(Mutex::new(load_denoiser()?))
    } else {
        None
    };

    let mut system = sysinfo::System::new()?;
    system.refresh_memory();
    let limit = match args.memory_gb {
        Some(gb) => (gb * GB) as u64,
        None => system.available_memory() / 10 * 7,
    };
    // ponytail: 첫 사진의 크기로 전체를 가늠한다. 크기가 제각각인 폴더에서는 어긋날 수 있지만
    // 실제 입장 제한은 사진마다 따로 계산하므로 안전에는 영향이 없다.
    let per_photo = raw::pixel_count(&tasks[0].input).unwrap_or(24_000_000) * BYTES_PER_PIXEL;
    let (workers, encoder_threads) = plan(cores(), args.jobs, limit, per_photo, tasks.len());
    let budget = job::Budget::new(limit, workers);
    let warnings = Mutex::new(Vec::new());
    let cancel = AtomicBool::new(false);
    let batch = job::Batch {
        tasks: &tasks,
        workers,
        budget: &budget,
        overwrite: args.overwrite,
        cancel: &cancel,
        cost: &|task| Ok(raw::pixel_count(&task.input)? * BYTES_PER_PIXEL),
        available: &|| {
            let mut system = sysinfo::System::new().expect("시스템 정보를 읽을 수 없습니다");
            system.refresh_memory();
            system.available_memory()
        },
        work: &|task, progress| {
            let denoise = denoiser.as_ref().map(|model| (model, args.denoise));
            let warning = convert(
                &task.input,
                &task.output,
                QUALITY,
                encoder_threads,
                denoise,
                progress,
            )?;
            if let Some(warning) = warning {
                warnings.lock().unwrap().push((task.input.clone(), warning));
            }
            Ok(())
        },
    };
    let mut monitor = Monitor::new(system, limit)?;
    let outcome = job::run_batch(&batch, |status| report(&monitor.sample(status, &budget)));
    Ok((outcome, warnings.into_inner().unwrap()))
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

fn run(args: &Args) -> Result<()> {
    if !args.input.is_dir() {
        let denoiser = if args.denoise > 0.0 {
            Some(Mutex::new(load_denoiser()?))
        } else {
            None
        };
        let denoise = denoiser.as_ref().map(|model| (model, args.denoise));
        if let Some(warning) = convert(
            &args.input,
            &args.output,
            QUALITY,
            cores(),
            denoise,
            &|_| {},
        )? {
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

fn main() -> std::process::ExitCode {
    let args = match parse_args(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n{USAGE}");
            return std::process::ExitCode::from(2);
        }
    };
    let started = Instant::now();
    let result = run(&args);
    eprintln!("걸린 시간 {:.1}초", started.elapsed().as_secs_f32());
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("실패: {e}");
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

        convert(&input, &output, 55, 8, None, &|_| {}).unwrap();

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
        let (Some(input), Some(model)) = (sample_arw(), gpu::test_denoiser()) else {
            return;
        };
        let dir = std::env::temp_dir().join(format!("amsil-test-denoise-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let (plain, denoised) = (dir.join("plain.heic"), dir.join("denoised.heic"));

        convert(&input, &plain, 55, 8, None, &|_| {}).unwrap();
        convert(
            &input,
            &denoised,
            55,
            8,
            Some((&Mutex::new(model), 1.0)),
            &|_| {},
        )
        .unwrap();

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

    #[test]
    fn plan_runs_as_many_photos_as_memory_allows_and_gives_spare_cores_to_the_encoder() {
        const GIB: u64 = 1 << 30;
        let photo = GIB * 5 / 4;
        assert_eq!(
            plan(32, None, 40 * GIB, photo, 309),
            (32, 1),
            "메모리가 넉넉하면 코어 수만큼"
        );
        assert_eq!(
            plan(32, None, 4 * GIB, photo, 309),
            (3, 10),
            "메모리가 3장분이면 3장, 인코더는 10스레드씩"
        );
        assert_eq!(
            plan(32, Some(4), 40 * GIB, photo, 309),
            (4, 8),
            "--jobs로 줄인 경우"
        );
        assert_eq!(
            plan(8, None, GIB, photo, 309),
            (1, 8),
            "한 장분도 안 되면 한 장씩"
        );
        assert_eq!(
            plan(32, None, 40 * GIB, photo, 2),
            (2, 16),
            "사진이 두 장뿐이면 코어를 둘이 나눠 쓴다"
        );
    }
}
