//! 변환의 실제 작업: 사진 한 장의 처리 과정과, 여러 장을 자원 한도 안에서 동시에 돌리는 일.
//! 명령행과 GUI가 함께 쓴다.

use crate::gpu::{self, Denoiser};
use crate::{Result, denoise, develop, heif, job, raw};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Instant;

/// libheif의 품질 값. 55가 JPEG q90 수준이고 70부터는 JPEG보다 파일이 커진다 (docs/design.md 3.2절).
pub const QUALITY: u8 = 55;

/// 사진 한 장을 처리하는 데 드는 메모리의 추정값 (센서 화소당 바이트).
// ponytail: 실측으로 정한 상수 (A900 기준 장당 최대 53바이트/화소). 다른 카메라에서 틀려도 시스템 여유 메모리 감시가 받쳐 준다.
const BYTES_PER_PIXEL: u64 = 56;

pub const GB: f64 = (1u64 << 30) as f64;

/// 변환할 사진 한 장.
#[derive(Debug, Clone, PartialEq)]
pub struct Photo {
    pub input: PathBuf,
    pub output: PathBuf,
    /// 노이즈 제거 강도 (0~1). 0이면 끈다.
    pub denoise: f32,
}

/// 배치 전체에 적용되는 설정.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub quality: u8,
    /// 이미 있는 출력 파일도 다시 만든다.
    pub overwrite: bool,
    /// 동시에 처리할 장수. 없으면 CPU 코어 수.
    pub jobs: Option<usize>,
    /// 메모리 한도(GB). 없으면 남은 메모리의 70%.
    pub memory_gb: Option<f64>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            quality: QUALITY,
            overwrite: false,
            jobs: None,
            memory_gb: None,
        }
    }
}

/// 변환은 됐지만 알려야 할 일이 있는 사진과 그 내용.
pub type Warnings = Vec<(PathBuf, String)>;

/// RAW 한 장을 HEIF로 바꾼다. `denoise`는 모델과 강도(0~1), `progress`에는 진행 정도(0~1)를 알린다.
/// 알려야 할 일이 있으면(품질을 낮춘 경우) 그 문구를 돌려준다.
pub fn convert(
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
pub fn cores() -> usize {
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

/// 노이즈 제거 모델을 읽는다. 모델 파일이 없으면 받는 방법을 알려 준다.
pub fn load_denoiser() -> Result<Denoiser> {
    let path = gpu::model_dir().join(gpu::DENOISE_MODEL);
    if !path.exists() {
        return Err(format!(
            "노이즈 제거 모델이 없습니다: {}\nscripts/fetch-models.sh 를 실행해 받으세요.",
            path.display()
        )
        .into());
    }
    Denoiser::load(&path)
}

/// GPU를 쓰지 못해 CPU로 돌고 있으면, 느리다는 것과 해결 방법을 알리는 문구.
pub fn cpu_notice(denoiser: &Denoiser) -> Option<String> {
    let gpu::Backend::Cpu { reason } = &denoiser.backend else {
        return None;
    };
    // 2,400만 화소 사진 한 장이 48타일이다.
    Some(format!(
        "GPU 가속을 쓸 수 없어 CPU로 실행합니다. 노이즈 제거가 느립니다 \
         (2,400만 화소 사진 한 장에 약 {:.0}초).\n\
         해결: Vulkan 드라이버를 설치하세요 (NVIDIA는 전용 드라이버, AMD·Intel은 mesa-vulkan-drivers). \
         `vulkaninfo`로 확인할 수 있습니다.\n원인: {reason}",
        denoiser.tile_seconds * 48.0
    ))
}

/// 배치가 도는 동안 주기적으로 알리는 내용.
pub struct Report {
    pub status: job::Status,
    /// 남은 시간(초). 아직 모르면 None.
    pub remaining: Option<f64>,
    /// 마지막 진행 이후 흐른 시간(초).
    pub idle: f64,
    /// 이 프로그램의 CPU 사용률 (전체 코어 대비 %).
    pub cpu: f32,
    /// 이 프로그램이 쓰는 메모리와 한도 (바이트).
    pub memory: u64,
    pub memory_limit: u64,
    /// AI 모델이 실제로 돌고 있던 시간의 비율 (%).
    pub gpu: f32,
    /// 시스템 메모리가 부족해 동시 처리 수를 줄인 상태.
    pub low_memory: bool,
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

/// 사진들을 변환한다. 진행 상황은 `report`로 알리고, 결과와 함께 경고를 돌려준다.
pub fn run(
    photos: &[Photo],
    settings: &Settings,
    denoiser: Option<&Mutex<Denoiser>>,
    cancel: &AtomicBool,
    mut report: impl FnMut(&Report),
) -> Result<(job::Outcome, Warnings)> {
    if photos.is_empty() {
        return Err("변환할 사진이 없습니다".into());
    }
    if denoiser.is_none() && photos.iter().any(|photo| photo.denoise > 0.0) {
        return Err("노이즈 제거 모델이 준비되지 않았습니다".into());
    }
    let tasks: Vec<job::Task> = photos
        .iter()
        .map(|photo| job::Task {
            input: photo.input.clone(),
            output: photo.output.clone(),
        })
        .collect();
    let strength: HashMap<&Path, f32> = photos
        .iter()
        .map(|photo| (photo.input.as_path(), photo.denoise))
        .collect();

    let mut system = sysinfo::System::new()?;
    system.refresh_memory();
    let limit = match settings.memory_gb {
        Some(gb) => (gb * GB) as u64,
        None => system.available_memory() / 10 * 7,
    };
    // ponytail: 첫 사진의 크기로 전체를 가늠한다. 크기가 제각각인 폴더에서는 어긋날 수 있지만
    // 실제 입장 제한은 사진마다 따로 계산하므로 안전에는 영향이 없다.
    let per_photo = raw::pixel_count(&tasks[0].input).unwrap_or(24_000_000) * BYTES_PER_PIXEL;
    let (workers, encoder_threads) = plan(cores(), settings.jobs, limit, per_photo, tasks.len());
    let budget = job::Budget::new(limit, workers);
    let warnings = Mutex::new(Vec::new());
    let batch = job::Batch {
        tasks: &tasks,
        workers,
        budget: &budget,
        overwrite: settings.overwrite,
        cancel,
        cost: &|task| Ok(raw::pixel_count(&task.input)? * BYTES_PER_PIXEL),
        available: &|| {
            let mut system = sysinfo::System::new().expect("시스템 정보를 읽을 수 없습니다");
            system.refresh_memory();
            system.available_memory()
        },
        work: &|task, progress| {
            let denoise = denoiser.map(|model| (model, strength[task.input.as_path()]));
            let warning = convert(
                &task.input,
                &task.output,
                settings.quality,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample_arw;
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
