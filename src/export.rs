//! 변환의 실제 작업: 사진 한 장의 처리 과정과, 여러 장을 자원 한도 안에서 동시에 돌리는 일.
//! 명령행과 GUI가 함께 쓴다.

use crate::edit::Params;
use crate::frame::{Frame, Shot};
use crate::gpu::{self, Denoiser};
use crate::lang::{Text, t, text};
use crate::lut::Lut;
use crate::mask::{self, Low, Mask, Segmenter};
use crate::skin::{self, SkinFinder};
use crate::{Result, denoise, develop, heif, job, log, raw};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::time::Instant;

/// libheif의 품질 값. A900 사진으로 재 보니 55가 JPEG q90 수준이고 70부터는 JPEG보다 파일이 커진다.
pub const QUALITY: u8 = 55;
/// 고를 수 있는 품질의 범위.
pub const QUALITY_RANGE: std::ops::RangeInclusive<u8> = 40..=100;

/// 사진 한 장을 처리하는 데 드는 메모리의 추정값 (센서 화소당 바이트).
// ponytail: 실측으로 정한 상수 (A900 기준 장당 최대 53바이트/화소). 다른 카메라에서 틀려도 시스템 여유 메모리 감시가 받쳐 준다.
const BYTES_PER_PIXEL: u64 = 64;

pub const GB: f64 = (1u64 << 30) as f64;

/// 변환할 사진 한 장.
#[derive(Debug, Clone, PartialEq)]
pub struct Photo {
    pub input: PathBuf,
    pub output: PathBuf,
    /// 노이즈 제거 강도 (0~1). 0이면 끈다.
    pub denoise: f32,
    /// 보정 값 (프리셋 × 강도 + 수동 조정).
    pub params: Params,
    /// 마지막에 입힐 LUT 파일과 강도.
    pub lut: Option<(PathBuf, f32)>,
    /// 사진에 두를 프레임.
    pub frame: Option<Frame>,
    /// 부분 보정 마스크.
    pub masks: Vec<Mask>,
    /// 피부 보정의 세기 (0~1). 0이면 끈다.
    pub skin: f32,
}

impl Photo {
    /// 마스크나 피부 보정처럼, 현상할 때 그림을 한 장 더 들고 있어야 하는 것이 있는가.
    fn needs_extra_memory(&self) -> bool {
        !self.masks.is_empty() || self.skin > 0.0
    }
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
pub type Warnings = Vec<(PathBuf, Text)>;

/// 현상이 끝난 사진: 바로 세운 sRGB 이미지와 출력 파일에 넣을 EXIF.
pub struct Rendered {
    pub image: crate::Image,
    pub exif: Vec<u8>,
}

/// 사진 한 장에 입힐 것들: 보정 값, LUT, 마스크, 프레임.
#[derive(Clone, Copy)]
pub struct Look<'a> {
    /// 보정 값 (프리셋 × 강도 + 수동 조정).
    pub params: &'a Params,
    /// 보정 뒤에 입힐 LUT과 강도.
    pub lut: Option<(&'a Lut, f32)>,
    /// 맨 마지막에 두를 프레임.
    pub frame: Option<&'a Frame>,
    /// 부분 보정 마스크.
    pub masks: &'a [Mask],
    /// 마스크가 고르는 영역을 계산할 모델. 마스크가 있는 사진에 필요하다.
    pub segmenter: Option<&'a Mutex<Segmenter>>,
    /// 이 번호의 마스크가 고른 영역을 붉게 표시한다 (화면에서 영역을 확인할 때 쓴다).
    pub overlay: Option<usize>,
    /// 피부 보정의 세기 (0~1). 0이면 끈다.
    pub skin: f32,
    /// 피부를 찾을 모델. 피부 보정을 켠 사진에 필요하다.
    pub skin_finder: Option<&'a Mutex<SkinFinder>>,
}

impl<'a> Look<'a> {
    /// LUT도 마스크도 프레임도 없이 보정 값만.
    pub fn new(params: &'a Params) -> Self {
        Self {
            params,
            lut: None,
            frame: None,
            masks: &[],
            segmenter: None,
            overlay: None,
            skin: 0.0,
            skin_finder: None,
        }
    }
}

/// 모델로 구한 영역들. 사진을 현상할 때 함께 넘긴다.
#[derive(Default)]
pub struct Areas {
    /// `Look::masks`의 마스크마다 고른 곳.
    pub masks: Vec<Low>,
    /// 피부인 곳 (피부 보정을 켰을 때).
    pub skin: Option<Low>,
}

/// 읽어 들인 사진: 보정을 적용하기 전의 카메라 RGB와 현상에 필요한 값들.
/// 읽기(디모자익, 노이즈 제거)는 오래 걸리고 보정을 바꿀 때마다 되풀이할 필요가 없어서 따로 둔다.
pub struct Loaded {
    /// 카메라 RGB (선형, 출력 영역으로 잘린 것).
    pub cam: crate::Image,
    wb: [f32; 3],
    xyz_to_cam: [[f32; 3]; 3],
    flips: (bool, bool, bool),
    pub exif: Vec<u8>,
    /// 프레임에 적을 촬영 정보.
    shot: Shot,
}

impl Loaded {
    /// 보정을 적용해 현상하고 바로 세운다. `cam`에는 `self.cam`이나 그것을 줄인 것을 준다.
    /// LUT은 보정 뒤에 입히고, 프레임은 바로 세운 다음에 두른다.
    /// `areas`는 모델로 구한 영역이다 (`areas`로 구한다). 마스크, 피부 보정 순서로 적용한다.
    // ponytail: 마스크마다 보정을 더한 그림을 따로 현상해 영역만큼 섞는다. 마스크가 겹치는 곳에서는
    // 뒤의 마스크가 앞의 것을 덮는다. 화소마다 보정 값을 달리 주는 방식보다 단순하고, 마스크 하나에 현상 한 번이 더 든다.
    pub fn develop(&self, cam: &crate::Image, look: Look, areas: &Areas) -> crate::Image {
        let upright = |params: &Params| {
            let mut image = develop::develop(cam, self.wb, self.xyz_to_cam, params);
            if let Some((lut, strength)) = look.lut {
                lut.apply(&mut image, strength);
            }
            let (transpose, flip_h, flip_v) = self.flips;
            develop::orient(image, transpose, flip_h, flip_v)
        };
        let mut image = upright(look.params);
        for (index, (mask, area)) in look.masks.iter().zip(&areas.masks).enumerate() {
            let shown = look.overlay == Some(index);
            if mask.adjust.is_neutral() && !shown {
                continue;
            }
            let alpha = mask::upscale(area, &image, mask.invert);
            if !mask.adjust.is_neutral() {
                mask::blend(
                    &mut image,
                    &upright(&look.params.plus(&mask.adjust)),
                    &alpha,
                );
            }
            if shown {
                mask::tint(&mut image, &alpha);
            }
        }
        if let Some(area) = areas.skin.as_ref().filter(|_| look.skin > 0.0) {
            let alpha = mask::upscale(area, &image, false);
            skin::smooth(&mut image, &alpha, look.skin);
        }
        match look.frame {
            Some(frame) => crate::frame::apply(&image, frame, &self.shot),
            None => image,
        }
    }

    /// 마스크가 고른 곳과 피부를 모델로 구한다. 마스크도 피부 보정도 없으면 모델이 필요 없다.
    pub fn areas(&self, look: Look) -> Result<Areas> {
        let mut areas = Areas::default();
        if look.masks.is_empty() && look.skin <= 0.0 {
            return Ok(areas);
        }
        let picture = self.model_picture();
        if !look.masks.is_empty() {
            let Some(segmenter) = look.segmenter else {
                return Err(mask::missing()
                    .unwrap_or_else(|| {
                        text!(
                            "마스크 모델이 준비되지 않았습니다",
                            "The mask model is not loaded",
                        )
                    })
                    .into());
            };
            let mut segmenter = segmenter.lock().unwrap();
            let embedding = segmenter.embed(&picture)?;
            for mask in look.masks {
                areas
                    .masks
                    .push(segmenter.select(&embedding, &mask.points)?);
            }
        }
        if look.skin > 0.0 {
            let Some(finder) = look.skin_finder else {
                return Err(skin::missing()
                    .unwrap_or_else(|| {
                        text!(
                            "피부 보정 모델이 준비되지 않았습니다",
                            "The skin model is not loaded",
                        )
                    })
                    .into());
            };
            areas.skin = Some(finder.lock().unwrap().find(&picture)?);
        }
        Ok(areas)
    }

    /// 마스크 모델과 피부 모델에 넣을 그림: 보정 없이 현상해 바로 세운 것 (긴 변 1024 이하).
    /// 화면과 내보내기가 같은 그림을 써야 같은 영역이 나온다.
    pub fn model_picture(&self) -> crate::preview::Pixels {
        let small = crate::preview::downscale(&self.cam, mask::INPUT);
        let neutral = Params::default();
        let image = self.develop(
            small.as_ref().unwrap_or(&self.cam),
            Look::new(&neutral),
            &Areas::default(),
        );
        crate::preview::shrink(&image, mask::INPUT)
    }
}

/// RAW 한 장을 읽는다. `denoise`는 모델과 강도(0~1), `progress`에는 진행 정도(0~0.6)를 알린다.
pub fn load(
    input: &Path,
    denoise: Option<(&Mutex<Denoiser>, f32)>,
    progress: &dyn Fn(f32),
) -> Result<Loaded> {
    let timing = Timing::start();
    let raw = raw::decode(input)?;
    timing.mark(t("읽기", "read"));
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
    timing.mark(t("디모자익/노이즈 제거", "demosaic/denoise"));
    progress(0.6);
    Ok(Loaded {
        cam,
        wb: raw.wb,
        xyz_to_cam: raw.xyz_to_cam,
        flips: raw.flips,
        exif: raw.exif,
        shot: raw.shot,
    })
}

/// RAW 한 장을 읽어 현상한다. 미리보기와 내보내기가 같은 과정을 쓴다.
pub fn render(
    input: &Path,
    denoise: Option<(&Mutex<Denoiser>, f32)>,
    look: Look,
    progress: &dyn Fn(f32),
) -> Result<Rendered> {
    let timing = Timing::start();
    let loaded = load(input, denoise, progress)?;
    let areas = loaded.areas(look)?;
    let image = loaded.develop(&loaded.cam, look, &areas);
    timing.mark(t("현상", "develop"));
    progress(0.75);
    Ok(Rendered {
        image,
        exif: loaded.exif,
    })
}

/// RAW 한 장을 HEIF로 바꾼다. `progress`에는 진행 정도(0~1)를 알린다.
/// 알려야 할 일이 있으면(품질을 낮춘 경우) 그 문구를 돌려준다.
pub fn convert(
    input: &Path,
    output: &Path,
    quality: u8,
    encoder_threads: usize,
    denoise: Option<(&Mutex<Denoiser>, f32)>,
    look: Look,
    progress: &dyn Fn(f32),
) -> Result<Option<Text>> {
    let timing = Timing::start();
    let Rendered { image, exif } = render(input, denoise, look, progress)?;
    let encoded = heif::encode(&image, quality, Some(&exif), encoder_threads)?;
    timing.mark(t("인코딩", "encode"));
    progress(0.98);

    // 임시 파일에 쓰고 이름을 바꾼다. 도중에 꺼져도 반쯤 쓰인 출력 파일이 남지 않는다.
    let mut tmp = output.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, encoded.bytes)?;
    std::fs::rename(&tmp, output)?;
    Ok((encoded.quality < quality).then(|| {
        let lowered = encoded.quality;
        text!(
            format!("파일이 너무 커져서 품질을 {quality}에서 {lowered}(으)로 낮췄습니다"),
            format!("The file got too large, so quality was lowered from {quality} to {lowered}"),
        )
    }))
}

/// 내보내기의 결과를 로그에 남긴다: 실패한 사진과 이유, 알릴 일, 요약.
fn record(outcome: &job::Outcome, warnings: &Warnings, seconds: f32) {
    for (file, reason) in &outcome.failed {
        log::error(format_args!("Failed: {}: {reason}", file.display()));
    }
    for (file, warning) in warnings {
        log::warn(format_args!("{}: {warning}", file.display()));
    }
    if let Some(reason) = &outcome.stopped {
        log::error(format_args!("Stopped before finishing: {reason}"));
    }
    let (done, skipped, failed) = (outcome.done, outcome.skipped, outcome.failed.len());
    log::info(format!(
        "Export finished: done {done}, skipped {skipped}, failed {failed} ({seconds:.1} s)"
    ));
}

/// 사진 한 장을 처리하는 데 잡아 둘 메모리(바이트). `size`는 센서의 가로·세로 화소 수.
/// 프레임을 두르면 그림이 커지는 만큼 더 잡는다. 사진이 가로로 서 있는지 세로로 서 있는지는
/// 헤더만으로 알 수 없으므로, 둘 중 더 커지는 쪽으로 잡는다.
/// 마스크나 피부 보정이 있으면(`extra`) 영역과 그림 한 장만큼(화소당 16바이트) 더 잡는다.
pub fn memory_needed((width, height): (usize, usize), frame: Option<&Frame>, extra: bool) -> u64 {
    let growth = frame.map_or(1.0, |frame| {
        frame.growth(width, height).max(frame.growth(height, width))
    });
    let extra = if extra { 1.25 } else { 1.0 };
    ((width * height) as f64 * BYTES_PER_PIXEL as f64 * growth * extra) as u64
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
                "  {stage}: {:.2}{}",
                last.replace(Instant::now()).elapsed().as_secs_f32(),
                t("초", " s")
            );
        }
    }
}

/// 노이즈 제거 모델을 읽는다. 모델 파일이 없으면 받는 방법을 알려 준다.
pub fn load_denoiser() -> Result<Denoiser> {
    let path = gpu::model_dir().join(gpu::DENOISE_MODEL);
    if !path.exists() {
        let path = path.display();
        return Err(text!(
            format!(
                "노이즈 제거 모델이 없습니다: {path}\nfetch-models.sh를 실행해 받으세요 (실행 파일 옆이나 저장소의 scripts 폴더에 있습니다)."
            ),
            format!(
                "The denoise model is missing: {path}\nRun fetch-models.sh to download it (next to the program, or in the scripts folder of the repository)."
            ),
        )
        .into());
    }
    Denoiser::load(&path)
}

/// GPU를 쓰지 못해 CPU로 돌고 있으면, 느리다는 것과 해결 방법을 알리는 문구.
pub fn cpu_notice(denoiser: &Denoiser) -> Option<Text> {
    let gpu::Backend::Cpu { reason } = &denoiser.backend else {
        return None;
    };
    // 2,400만 화소 사진 한 장이 48타일이다.
    let seconds = denoiser.tile_seconds * 48.0;
    Some(text!(
        format!(
            "GPU 가속을 쓸 수 없어 CPU로 실행합니다. 노이즈 제거가 느립니다 \
             (2,400만 화소 사진 한 장에 약 {seconds:.0}초).\n\
             해결: Vulkan 드라이버를 설치하세요 (NVIDIA는 전용 드라이버, AMD·Intel은 mesa-vulkan-drivers). \
             `vulkaninfo`로 확인할 수 있습니다.\n원인: {reason}"
        ),
        format!(
            "GPU acceleration is not available, so the AI runs on the CPU. Noise reduction is slow \
             (about {seconds:.0} s for one 24-megapixel photo).\n\
             Fix: install a Vulkan driver (the vendor driver for NVIDIA, mesa-vulkan-drivers for AMD and Intel). \
             You can check with `vulkaninfo`.\nCause: {reason}"
        ),
    ))
}

/// 배치가 도는 동안 주기적으로 알리는 내용.
#[derive(Debug, Clone, Copy)]
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
    segmenter: Option<&Mutex<Segmenter>>,
    skin_finder: Option<&Mutex<SkinFinder>>,
    cancel: &AtomicBool,
    mut report: impl FnMut(&Report),
) -> Result<(job::Outcome, Warnings)> {
    if photos.is_empty() {
        return Err(text!("변환할 사진이 없습니다", "No photos to convert").into());
    }
    if denoiser.is_none() && photos.iter().any(|photo| photo.denoise > 0.0) {
        return Err(text!(
            "노이즈 제거 모델이 준비되지 않았습니다",
            "The denoise model is not loaded",
        )
        .into());
    }
    let tasks: Vec<job::Task> = photos
        .iter()
        .map(|photo| job::Task {
            input: photo.input.clone(),
            output: photo.output.clone(),
        })
        .collect();
    let looks: HashMap<&Path, &Photo> = photos
        .iter()
        .map(|photo| (photo.output.as_path(), photo))
        .collect();
    // LUT 파일은 한 번씩만 읽는다. 읽지 못한 것은 그 LUT을 쓰는 사진만 실패시킨다.
    let luts: HashMap<&Path, std::result::Result<Lut, Text>> = photos
        .iter()
        .filter_map(|photo| photo.lut.as_ref())
        .map(|(file, _)| (file.as_path(), Lut::load(file).map_err(|e| Text::of(&e))))
        .collect();

    let mut system = sysinfo::System::new()?;
    system.refresh_memory();
    let limit = match settings.memory_gb {
        Some(gb) => (gb * GB) as u64,
        None => system.available_memory() / 10 * 7,
    };
    // ponytail: 첫 사진의 크기로 전체를 가늠한다. 크기가 제각각인 폴더에서는 어긋날 수 있지만
    // 실제 입장 제한은 사진마다 따로 계산하므로 안전에는 영향이 없다.
    let per_photo =
        raw::sensor_size(&tasks[0].input).map_or(24_000_000 * BYTES_PER_PIXEL, |size| {
            memory_needed(
                size,
                photos[0].frame.as_ref(),
                photos[0].needs_extra_memory(),
            )
        });
    let (workers, encoder_threads) = plan(cores(), settings.jobs, limit, per_photo, tasks.len());
    let budget = job::Budget::new(limit, workers);
    let (count, quality) = (tasks.len(), settings.quality);
    log::info(format!(
        "Export started: {count} photos ({workers} at a time, quality {quality})"
    ));
    let started = Instant::now();
    let warnings = Mutex::new(Vec::new());
    let batch = job::Batch {
        tasks: &tasks,
        workers,
        budget: &budget,
        overwrite: settings.overwrite,
        cancel,
        cost: &|task| {
            let photo = looks[task.output.as_path()];
            Ok(memory_needed(
                raw::sensor_size(&task.input)?,
                photo.frame.as_ref(),
                photo.needs_extra_memory(),
            ))
        },
        available: &|| {
            let mut system = sysinfo::System::new().expect("시스템 정보를 읽을 수 없습니다");
            system.refresh_memory();
            system.available_memory()
        },
        work: &|task, progress| {
            let photo = looks[task.output.as_path()];
            let denoise = denoiser.map(|model| (model, photo.denoise));
            let lut = match &photo.lut {
                Some((file, strength)) => Some((
                    luts[file.as_path()].as_ref().map_err(|e| e.clone())?,
                    *strength,
                )),
                None => None,
            };
            let warning = convert(
                &task.input,
                &task.output,
                settings.quality,
                encoder_threads,
                denoise,
                Look {
                    lut,
                    frame: photo.frame.as_ref(),
                    masks: &photo.masks,
                    segmenter,
                    skin: photo.skin,
                    skin_finder,
                    ..Look::new(&photo.params)
                },
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
    let warnings = warnings.into_inner().unwrap();
    record(&outcome, &warnings, started.elapsed().as_secs_f32());
    Ok((outcome, warnings))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample_arw;
    use libheif_rs::HeifContext;

    /// LUT도 마스크도 프레임도 없이 보정 값만.
    fn bare(params: &Params) -> Look<'_> {
        Look::new(params)
    }

    #[test]
    fn converts_portrait_arw_to_upright_10bit_heic() {
        let Some(input) = sample_arw() else { return };
        let dir = std::env::temp_dir().join(format!("amsil-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("out.heic");

        convert(
            &input,
            &output,
            55,
            8,
            None,
            bare(&Params::default()),
            &|_| {},
        )
        .unwrap();

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

        convert(
            &input,
            &plain,
            55,
            8,
            None,
            bare(&Params::default()),
            &|_| {},
        )
        .unwrap();
        convert(
            &input,
            &denoised,
            55,
            8,
            Some((&Mutex::new(model), 1.0)),
            bare(&Params::default()),
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

    /// 출력 이미지의 채널별 평균.
    fn means(image: &crate::Image) -> [f32; 3] {
        [0, 1, 2].map(|c| image.planes[c].iter().sum::<f32>() / image.planes[c].len() as f32)
    }

    #[test]
    fn a_lut_changes_the_picture_by_its_strength_and_an_identity_lut_does_not() {
        let Some(input) = sample_arw() else { return };
        let identity = crate::lut::Lut::parse(
            "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n",
        )
        .unwrap();
        // 빨강을 없애는 LUT
        let no_red = crate::lut::Lut::parse(
            "LUT_3D_SIZE 2\n0 0 0\n0 0 0\n0 1 0\n0 1 0\n0 0 1\n0 0 1\n0 1 1\n0 1 1\n",
        )
        .unwrap();
        let loaded = load(&input, None, &|_| {}).unwrap();
        let small = crate::preview::downscale(&loaded.cam, 400).unwrap();
        let plain = means(&loaded.develop(&small, bare(&Params::default()), &Areas::default()));

        let same = means(&loaded.develop(
            &small,
            Look {
                lut: Some((&identity, 1.0)),
                ..bare(&Params::default())
            },
            &Areas::default(),
        ));
        assert!(
            (0..3).all(|c| (same[c] - plain[c]).abs() < 1e-3),
            "{same:?} 대 {plain:?}"
        );

        let full = means(&loaded.develop(
            &small,
            Look {
                lut: Some((&no_red, 1.0)),
                ..bare(&Params::default())
            },
            &Areas::default(),
        ));
        assert!(
            full[0] < 1e-4 && (full[1] - plain[1]).abs() < 1e-3,
            "{full:?}"
        );
        let half = means(&loaded.develop(
            &small,
            Look {
                lut: Some((&no_red, 0.5)),
                ..bare(&Params::default())
            },
            &Areas::default(),
        ));
        assert!((half[0] - plain[0] / 2.0).abs() < 1e-3, "{half:?}");
    }

    #[test]
    fn a_photo_whose_lut_cannot_be_read_fails_with_the_file_name_and_the_rest_continue() {
        let Some(input) = sample_arw() else { return };
        let dir = std::env::temp_dir().join(format!("amsil-test-lut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let missing = dir.join("없는 필름.cube");
        let photos = vec![
            Photo {
                input: input.clone(),
                output: dir.join("with-lut.heic"),
                denoise: 0.0,
                params: Params::default(),
                lut: Some((missing, 1.0)),
                frame: None,
                masks: vec![],
                skin: 0.0,
            },
            Photo {
                input,
                output: dir.join("plain.heic"),
                denoise: 0.0,
                params: Params::default(),
                lut: None,
                frame: None,
                masks: vec![],
                skin: 0.0,
            },
        ];
        let (outcome, _) = run(
            &photos,
            &Settings::default(),
            None,
            None,
            None,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        assert_eq!((outcome.done, outcome.failed.len()), (1, 1));
        assert!(
            outcome.failed[0].1.to_string().contains("없는 필름.cube"),
            "{:?}",
            outcome.failed
        );
        assert!(dir.join("plain.heic").exists() && !dir.join("with-lut.heic").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_frame_goes_round_the_upright_picture() {
        use crate::frame::{Frame, Layout};
        let Some(input) = sample_arw() else { return };
        let loaded = load(&input, None, &|_| {}).unwrap();
        let small = crate::preview::downscale(&loaded.cam, 400).unwrap();
        let params = Params::default();
        let plain = loaded.develop(&small, bare(&params), &Areas::default());
        let frame = Frame {
            layout: Layout::Band,
            dark: false,
        };
        let framed = loaded.develop(
            &small,
            Look {
                frame: Some(&frame),
                ..bare(&params)
            },
            &Areas::default(),
        );
        // 세로로 찍은 사진이다. 바로 세운 다음 그 아래에 띠가 붙어야 한다.
        assert!(plain.height > plain.width);
        assert_eq!(
            (framed.width, framed.height),
            frame.size(plain.width, plain.height)
        );
        assert_eq!(framed.width, plain.width);
        assert_eq!(
            framed.planes[0][..plain.width],
            plain.planes[0][..plain.width]
        );
        assert_eq!(framed.planes[0].last(), Some(&1.0), "맨 아래는 흰 띠");
        let band = &framed.planes[0][plain.width * plain.height..];
        assert!(
            band.iter().any(|&v| v < 0.5),
            "띠에 촬영 정보가 적혀 있어야 한다"
        );
    }

    #[test]
    fn a_framed_photo_is_exported_at_the_framed_size() {
        use crate::frame::{Frame, Layout};
        let Some(input) = sample_arw() else { return };
        let dir = std::env::temp_dir().join(format!("amsil-test-frame-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = dir.join("framed.heic");
        let (params, frame) = (
            Params::default(),
            Frame {
                layout: Layout::Band,
                dark: true,
            },
        );
        let look = Look {
            frame: Some(&frame),
            ..bare(&params)
        };
        convert(&input, &output, 55, 8, None, look, &|_| {}).unwrap();
        let ((width, height), _) = stats(&output);
        std::fs::remove_dir_all(&dir).unwrap();
        // 4032×6048 사진 아래에 짧은 변의 10%(404)짜리 띠
        assert_eq!((width, height), (4032, 6048 + 404));
    }

    #[test]
    fn a_frame_raises_the_memory_estimate_by_how_much_it_enlarges_the_picture() {
        use crate::frame::{Frame, Layout};
        let plain = memory_needed((6080, 4048), None, false);
        assert_eq!(plain, 6080 * 4048 * BYTES_PER_PIXEL);
        let frame = |layout| Frame {
            layout,
            dark: false,
        };
        let band = memory_needed((6080, 4048), Some(&frame(Layout::Band)), false);
        assert!((band as f64 / plain as f64 - 1.1).abs() < 0.01, "{band}");
        // 가로 사진을 세로 9:16에 넣으면 세 배 가까이 커진다. 사진이 어느 쪽으로 서 있는지는
        // 헤더만으로 알 수 없으므로 더 커지는 쪽으로 잡는다.
        let tall = memory_needed((6080, 4048), Some(&frame(Layout::Ratio(9, 16))), false);
        assert!(tall as f64 / plain as f64 > 2.9, "{tall}");
        assert_eq!(
            tall,
            memory_needed((4048, 6080), Some(&frame(Layout::Ratio(9, 16))), false)
        );
    }

    /// 왼쪽 절반을 고른 저해상도 마스크.
    fn left_half() -> crate::mask::Low {
        crate::mask::Low {
            width: 4,
            height: 4,
            logits: [8.0, 8.0, -8.0, -8.0].repeat(4),
        }
    }

    /// 그림의 가로 `from..to`(0~1) 구간의 채널별 평균.
    fn strip_means(image: &crate::Image, from: f32, to: f32) -> [f32; 3] {
        let (x0, x1) = (
            (from * image.width as f32) as usize,
            (to * image.width as f32) as usize,
        );
        image.planes.each_ref().map(|plane| {
            let sum: f32 = plane
                .chunks_exact(image.width)
                .map(|row| row[x0..x1].iter().sum::<f32>())
                .sum();
            sum / ((x1 - x0) * image.height) as f32
        })
    }

    #[test]
    fn a_mask_adjusts_only_the_area_it_selects() {
        use crate::mask::Mask;
        let Some(input) = sample_arw() else { return };
        let loaded = load(&input, None, &|_| {}).unwrap();
        let small = crate::preview::downscale(&loaded.cam, 400).unwrap();
        let params = Params::default();
        let plain = loaded.develop(&small, bare(&params), &Areas::default());
        // 어둡게 하는 보정으로 본다. 밝게 하면 이미 밝은 하늘은 더 밝아질 여유가 없다.
        let darker = Mask {
            adjust: Params {
                exposure: -1.0,
                ..Params::default()
            },
            ..Mask::default()
        };
        let masks = [darker.clone()];
        let look = Look {
            masks: &masks,
            ..bare(&params)
        };
        let masked = loaded.develop(
            &small,
            look,
            &Areas {
                masks: vec![left_half()],
                skin: None,
            },
        );
        let (left, right) = (0.0..0.3, 0.7..1.0);
        let change = |range: &std::ops::Range<f32>, image: &crate::Image| {
            strip_means(image, range.start, range.end)[1]
                / strip_means(&plain, range.start, range.end)[1]
        };
        assert!(
            change(&left, &masked) < 0.8,
            "고른 쪽은 어두워진다: {}",
            change(&left, &masked)
        );
        assert!(
            (change(&right, &masked) - 1.0).abs() < 0.01,
            "나머지는 그대로: {}",
            change(&right, &masked)
        );

        // 뒤집으면 반대쪽이 어두워진다.
        let masks = [Mask {
            invert: true,
            ..darker
        }];
        let look = Look {
            masks: &masks,
            ..bare(&params)
        };
        let inverted = loaded.develop(
            &small,
            look,
            &Areas {
                masks: vec![left_half()],
                skin: None,
            },
        );
        assert!((change(&left, &inverted) - 1.0).abs() < 0.01);
        assert!(change(&right, &inverted) < 0.8);
    }

    #[test]
    fn the_selected_area_can_be_shown_in_red() {
        use crate::mask::Mask;
        let Some(input) = sample_arw() else { return };
        let loaded = load(&input, None, &|_| {}).unwrap();
        let small = crate::preview::downscale(&loaded.cam, 400).unwrap();
        let params = Params::default();
        let plain = loaded.develop(&small, bare(&params), &Areas::default());
        let masks = [Mask::default()];
        let look = Look {
            masks: &masks,
            overlay: Some(0),
            ..bare(&params)
        };
        let shown = loaded.develop(
            &small,
            look,
            &Areas {
                masks: vec![left_half()],
                skin: None,
            },
        );
        let (before, after) = (strip_means(&plain, 0.0, 0.3), strip_means(&shown, 0.0, 0.3));
        assert!(
            after[0] > before[0] + 0.1 && after[2] < before[2],
            "{before:?} → {after:?}"
        );
        let (before, after) = (strip_means(&plain, 0.7, 1.0), strip_means(&shown, 0.7, 1.0));
        assert!(
            (0..3).all(|c| (before[c] - after[c]).abs() < 1e-3),
            "고르지 않은 쪽은 그대로: {before:?} → {after:?}"
        );
    }

    #[test]
    fn a_photo_with_masks_fails_alone_when_the_mask_model_is_not_there() {
        use crate::mask::{Mask, Point};
        let Some(input) = sample_arw() else { return };
        let dir = std::env::temp_dir().join(format!("amsil-test-nomask-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let photo = |name: &str, masks| Photo {
            input: input.clone(),
            output: dir.join(name),
            denoise: 0.0,
            params: Params::default(),
            lut: None,
            frame: None,
            masks,
            skin: 0.0,
        };
        let click = Point {
            x: 0.5,
            y: 0.5,
            include: true,
        };
        let masked = vec![Mask {
            points: vec![click],
            ..Mask::default()
        }];
        let photos = vec![photo("masked.heic", masked), photo("plain.heic", vec![])];
        let (outcome, _) = run(
            &photos,
            &Settings::default(),
            None,
            None,
            None,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
        let made = (
            dir.join("plain.heic").exists(),
            dir.join("masked.heic").exists(),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!((outcome.done, outcome.failed.len()), (1, 1));
        assert!(
            outcome.failed[0].1.to_string().contains("마스크 모델"),
            "{:?}",
            outcome.failed
        );
        assert_eq!(made, (true, false));
    }

    #[test]
    fn masks_raise_the_memory_estimate() {
        let plain = memory_needed((6080, 4048), None, false);
        // 마스크 영역과, 보정을 달리한 그림 한 장이 더 필요하다.
        assert_eq!(memory_needed((6080, 4048), None, true), plain / 4 * 5);
    }

    #[test]
    fn a_real_click_brightens_the_building_and_leaves_the_sky() {
        use crate::mask::{Mask, Point};
        // 마스크 모델과 샘플 사진이 있을 때만 돈다.
        let photo =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110/DSC04551.ARW");
        let Ok(segmenter) = crate::mask::load_segmenter() else {
            eprintln!("마스크 모델이 없어 건너뜀");
            return;
        };
        if !photo.exists() {
            return;
        }
        let segmenter = Mutex::new(segmenter);
        let loaded = load(&photo, None, &|_| {}).unwrap();
        let small = crate::preview::downscale(&loaded.cam, 800).unwrap();
        let params = Params::default();
        let masks = [Mask {
            points: vec![Point {
                x: 0.48,
                y: 0.35,
                include: true,
            }],
            invert: false,
            adjust: Params {
                exposure: 1.0,
                ..Params::default()
            },
        }];
        let look = Look {
            masks: &masks,
            segmenter: Some(&segmenter),
            ..bare(&params)
        };
        let started = Instant::now();
        let areas = loaded.areas(look).unwrap();
        let selected = started.elapsed();
        assert_eq!(areas.masks.len(), 1);
        let (plain, masked) = (
            loaded.develop(&small, bare(&params), &Areas::default()),
            loaded.develop(&small, look, &areas),
        );
        // 원본 크기에서 마스크가 얼마나 더 걸리는지 (성능을 가늠하는 용도)
        let started = Instant::now();
        loaded.develop(&loaded.cam, bare(&params), &Areas::default());
        let without = started.elapsed();
        let started = Instant::now();
        loaded.develop(&loaded.cam, look, &areas);
        eprintln!(
            "영역 구하기 {selected:.2?}, 원본 크기 현상: 마스크 없이 {without:.2?}, 마스크 하나 {:.2?}",
            started.elapsed()
        );
        let at = |image: &crate::Image, x: f32, y: f32| {
            image.planes[1][(y * image.height as f32) as usize * image.width
                + (x * image.width as f32) as usize]
        };
        assert!(
            at(&masked, 0.48, 0.35) > at(&plain, 0.48, 0.35) * 1.2,
            "건물은 밝아진다"
        );
        assert!(
            (at(&masked, 0.1, 0.1) - at(&plain, 0.1, 0.1)).abs() < 0.01,
            "하늘은 그대로"
        );
    }

    #[test]
    fn skin_retouch_changes_only_the_skin_area() {
        let Some(input) = sample_arw() else { return };
        let loaded = load(&input, None, &|_| {}).unwrap();
        let small = crate::preview::downscale(&loaded.cam, 400).unwrap();
        let params = Params::default();
        let plain = loaded.develop(&small, bare(&params), &Areas::default());
        let look = Look {
            skin: 1.0,
            ..bare(&params)
        };
        let areas = Areas {
            masks: vec![],
            skin: Some(left_half()),
        };
        let retouched = loaded.develop(&small, look, &areas);
        // 그림의 가로 구간에서 화소마다 얼마나 달라졌는지의 평균
        let moved = |from: f32, to: f32| {
            let (x0, x1) = (
                (from * plain.width as f32) as usize,
                (to * plain.width as f32) as usize,
            );
            let mut sum = 0.0;
            for y in 0..plain.height {
                for x in x0..x1 {
                    let i = y * plain.width + x;
                    sum += (plain.planes[1][i] - retouched.planes[1][i]).abs();
                }
            }
            sum / ((x1 - x0) * plain.height) as f32
        };
        assert!(
            moved(0.0, 0.3) > 1e-4,
            "피부 쪽은 달라진다: {}",
            moved(0.0, 0.3)
        );
        assert!(
            moved(0.7, 1.0) < 1e-6,
            "나머지는 그대로: {}",
            moved(0.7, 1.0)
        );
        // 세기가 0이면 영역이 있어도 쓰지 않는다.
        let untouched = loaded.develop(&small, bare(&params), &areas);
        assert_eq!(untouched.planes, plain.planes);
    }

    #[test]
    fn skin_is_looked_for_only_when_the_retouch_is_on() {
        // 피부를 찾는 모델과 샘플 사진이 있을 때만 돈다.
        let Some(input) = sample_arw() else { return };
        let Ok(finder) = crate::skin::load_finder() else {
            eprintln!("피부를 찾는 모델이 없어 건너뜀");
            return;
        };
        let finder = Mutex::new(finder);
        let loaded = load(&input, None, &|_| {}).unwrap();
        let params = Params::default();
        let off = Look {
            skin_finder: Some(&finder),
            ..bare(&params)
        };
        assert!(loaded.areas(off).unwrap().skin.is_none());
        let on = Look { skin: 0.5, ..off };
        assert!(loaded.areas(on).unwrap().skin.is_some());
        // 모델 없이 켜면 그 이유를 알린다.
        let missing = Look {
            skin: 0.5,
            ..bare(&params)
        };
        assert!(loaded.areas(missing).is_err());
    }
}
