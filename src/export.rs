//! The actual conversion work: the processing steps for one photo, and running many at once
//! within resource limits. Shared by the command line and the GUI.

use crate::capture::{self, Capture, CaptureView};
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

/// libheif quality value. Measured on A900 photos, 55 matches JPEG q90, and from 70 up files get
/// larger than JPEG.
pub const QUALITY: u8 = 55;
/// Range of selectable quality.
pub const QUALITY_RANGE: std::ops::RangeInclusive<u8> = 40..=100;

/// Estimated memory to process one photo (bytes per sensor pixel).
// ponytail: constant set by measurement (at most 53 bytes/pixel per photo on the A900). If it is
// wrong for other cameras, the system free-memory watch backs it up.
const BYTES_PER_PIXEL: u64 = 64;

pub const GB: f64 = (1u64 << 30) as f64;

/// One photo to convert.
#[derive(Debug, Clone, PartialEq)]
pub struct Photo {
    pub input: PathBuf,
    pub output: PathBuf,
    /// Denoise strength (0..1). 0 turns it off.
    pub denoise: f32,
    /// Adjustment values (preset × strength + manual adjustments).
    pub params: Params,
    /// LUT file and strength applied last.
    pub lut: Option<(PathBuf, f32)>,
    /// Frame to put around the photo.
    pub frame: Option<Frame>,
    /// Local adjustment masks.
    pub masks: Vec<Mask>,
    /// Skin adjustment strength (0..1). 0 turns it off.
    pub skin: f32,
    /// Capture sharpening (RAW stage).
    pub capture: Capture,
}

impl Photo {
    /// Whether anything, such as masks or skin adjustment, needs one more image held while
    /// developing.
    fn needs_extra_memory(&self) -> bool {
        !self.masks.is_empty() || self.skin > 0.0
    }
}

/// Settings that apply to the whole batch.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub quality: u8,
    /// Recreate output files that already exist.
    pub overwrite: bool,
    /// Number of photos processed at once. Defaults to the CPU core count.
    pub jobs: Option<usize>,
    /// Memory limit (GB). Defaults to 70% of free memory.
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

/// Photos that converted but have something to report, and what it is.
pub type Warnings = Vec<(PathBuf, Text)>;

/// A developed photo: the upright sRGB image and the EXIF to put in the output file.
pub struct Rendered {
    pub image: crate::Image,
    pub exif: Vec<u8>,
}

/// What to apply to one photo: adjustment values, LUT, masks, frame.
#[derive(Clone, Copy)]
pub struct Look<'a> {
    /// Adjustment values (preset × strength + manual adjustments).
    pub params: &'a Params,
    /// LUT and strength applied after the adjustments.
    pub lut: Option<(&'a Lut, f32)>,
    /// Frame put around the photo at the very end.
    pub frame: Option<&'a Frame>,
    /// Local adjustment masks.
    pub masks: &'a [Mask],
    /// Model that computes the areas masks select. Needed for photos with masks.
    pub segmenter: Option<&'a Mutex<Segmenter>>,
    /// Tints red the area selected by the mask with this index (used to check the area on screen).
    pub overlay: Option<usize>,
    /// Skin adjustment strength (0..1). 0 turns it off.
    pub skin: f32,
    /// Model that finds skin. Needed for photos with skin adjustment on.
    pub skin_finder: Option<&'a Mutex<SkinFinder>>,
    /// Capture sharpening to apply when reading the RAW (used by `render`).
    pub capture: Capture,
    /// Whether to show where capture sharpening applies (in the preview).
    pub capture_view: Option<CaptureView>,
}

impl<'a> Look<'a> {
    /// Adjustment values only, with no LUT, masks, frame or capture sharpening.
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
            capture: Capture::off(),
            capture_view: None,
        }
    }
}

/// Areas computed by the models. Passed along when developing the photo.
#[derive(Default)]
pub struct Areas {
    /// The area selected by each mask in `Look::masks`.
    pub masks: Vec<Low>,
    /// Where the skin is (when skin adjustment is on).
    pub skin: Option<Low>,
}

/// A loaded photo: camera RGB before adjustments, and the values needed to develop it.
/// Reading (demosaicing, denoising) is slow and needn't be repeated each time adjustments change,
/// so it is kept separately.
pub struct Loaded {
    /// Camera RGB (linear, cropped to the output area). Already capture-sharpened.
    pub cam: crate::Image,
    /// Mask used to blend in capture sharpening (0..1, same size as `cam`). Used to show the
    /// sharpened areas. `load` leaves it empty.
    pub capture_mask: Vec<f32>,
    wb: [f32; 3],
    xyz_to_cam: [[f32; 3]; 3],
    flips: (bool, bool, bool),
    pub exif: Vec<u8>,
    /// Shooting info to print on the frame.
    shot: Shot,
}

impl Loaded {
    /// Develops with the adjustments applied and makes it upright. Pass `self.cam` or a scaled-down
    /// copy as `cam`. The LUT goes on after the adjustments, and the frame after making it upright.
    /// `areas` are the areas computed by the models (via `areas`). Masks are applied first, then
    /// skin adjustment.
    // ponytail: for each mask, an image with its adjustments added is developed separately and
    // blended in over its area. Where masks overlap, later masks cover earlier ones. Simpler than
    // per-pixel adjustment values, at the cost of one extra develop per mask.
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
        if let Some(view) = look.capture_view
            && let Some(area) = self.capture_area(cam)
        {
            match view {
                CaptureView::Overlay => mask::tint(&mut image, &area),
                CaptureView::Map => image.planes = [area.clone(), area.clone(), area],
            }
        }
        match look.frame {
            Some(frame) => crate::frame::apply(&image, frame, &self.shot),
            None => image,
        }
    }

    /// The capture sharpening blend mask, resized to `cam` (the original or a scaled-down copy) and
    /// made upright. None if there is no mask (read with `load`).
    fn capture_area(&self, cam: &crate::Image) -> Option<Vec<f32>> {
        if self.capture_mask.is_empty() {
            return None;
        }
        let (w, h) = (self.cam.width, self.cam.height);
        let plane = if (cam.width, cam.height) == (w, h) {
            self.capture_mask.clone()
        } else {
            resize_plane(&self.capture_mask, w, h, cam.width, cam.height)
        };
        let (transpose, flip_h, flip_v) = self.flips;
        let image = crate::Image {
            width: cam.width,
            height: cam.height,
            planes: [plane, Vec::new(), Vec::new()],
        };
        let [plane, _, _] = develop::orient(image, transpose, flip_h, flip_v).planes;
        Some(plane)
    }

    /// Computes mask selections and skin with the models. No model is needed without masks or skin
    /// adjustment.
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

    /// Image fed to the mask and skin models: developed without adjustments and made upright (long
    /// side at most 1024). Screen and export must use the same image to get the same areas.
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

/// Resizes one plane (averaging covered pixels when shrinking, nearest pixel when enlarging).
fn resize_plane(src: &[f32], w: usize, h: usize, out_w: usize, out_h: usize) -> Vec<f32> {
    let mut out = vec![0f32; out_w * out_h];
    for oy in 0..out_h {
        let (y0, y1) = (
            oy * h / out_h,
            ((oy + 1) * h / out_h).max(oy * h / out_h + 1).min(h),
        );
        for ox in 0..out_w {
            let (x0, x1) = (
                ox * w / out_w,
                ((ox + 1) * w / out_w).max(ox * w / out_w + 1).min(w),
            );
            let mut sum = 0f32;
            for y in y0..y1 {
                sum += src[y * w + x0..y * w + x1].iter().sum::<f32>();
            }
            out[oy * out_w + ox] = sum / ((y1 - y0) * (x1 - x0)) as f32;
        }
    }
    out
}

/// A RAW read and demosaiced (or denoised), plus the inputs to capture sharpening. Kept separately
/// so changing the capture sharpening value doesn't re-read the RAW.
pub struct Read {
    /// Camera RGB before capture sharpening (linear, cropped to the output area).
    plain: crate::Image,
    prepared: capture::Prepared,
    wb: [f32; 3],
    xyz_to_cam: [[f32; 3]; 3],
    flips: (bool, bool, bool),
    pub exif: Vec<u8>,
    shot: Shot,
}

impl Read {
    /// Applies capture sharpening so the photo can be developed.
    pub fn sharpened(&self, capture: &Capture) -> Loaded {
        let timing = Timing::start();
        let (cam, capture_mask) = capture::sharpen(&self.plain, &self.prepared, capture);
        timing.mark(t("캡처 샤픈", "capture sharpening"));
        Loaded {
            cam,
            capture_mask,
            wb: self.wb,
            xyz_to_cam: self.xyz_to_cam,
            flips: self.flips,
            exif: self.exif.clone(),
            shot: self.shot.clone(),
        }
    }

    /// Capture sharpening radius and contrast threshold derived from the photo.
    pub fn auto(&self) -> (f32, f32) {
        (self.prepared.auto_radius, self.prepared.auto_threshold)
    }
}

/// Reads one RAW and demosaics it (or denoises it), preparing the capture sharpening inputs.
/// `denoise` is the model and strength (0..1); `progress` receives progress (0..0.6).
pub fn read(
    input: &Path,
    denoise: Option<(&Mutex<Denoiser>, f32)>,
    progress: &dyn Fn(f32),
) -> Result<Read> {
    let timing = Timing::start();
    let raw = raw::decode(input)?;
    timing.mark(t("읽기", "read"));
    progress(0.15);
    let plain = match denoise {
        Some((model, strength)) if strength > 0.0 => {
            let packed = raw.packed_bayer();
            let (tiles, done) = (denoise::tile_count(&packed) as f32, AtomicUsize::new(0));
            let full = {
                // There is one GPU, so photos go through it one at a time.
                // ponytail: whoever grabs it first keeps it until done. If work requested from the
                // screen must go first, switch to a priority queue.
                let mut model = model.lock().unwrap();
                denoise::run(&packed, |tile| {
                    let out = model.infer(tile);
                    progress(0.15 + 0.45 * done.fetch_add(1, SeqCst) as f32 / tiles);
                    out
                })?
            };
            let denoised = raw.crop_model_output(&full)?;
            // At strength 1, the regular demosaic isn't needed.
            if strength < 1.0 {
                denoise::mix(raw.demosaic()?, &denoised, strength)
            } else {
                denoised
            }
        }
        _ => raw.demosaic()?,
    };
    timing.mark(t("디모자익/노이즈 제거", "demosaic/denoise"));
    let prepared = capture::prepare(&plain, raw.wb, &raw.sensor());
    timing.mark(t("캡처 샤픈 준비", "capture sharpening setup"));
    progress(0.6);
    Ok(Read {
        plain,
        prepared,
        wb: raw.wb,
        xyz_to_cam: raw.xyz_to_cam,
        flips: raw.flips,
        exif: raw.exif,
        shot: raw.shot,
    })
}

/// Reads one RAW and capture-sharpens it (`read` + `Read::sharpened`). Leaves the sharpened-area
/// mask empty.
pub fn load(
    input: &Path,
    denoise: Option<(&Mutex<Denoiser>, f32)>,
    capture: &Capture,
    progress: &dyn Fn(f32),
) -> Result<Loaded> {
    let mut loaded = read(input, denoise, progress)?.sharpened(capture);
    loaded.capture_mask = Vec::new();
    Ok(loaded)
}

/// Reads and develops one RAW. Preview and export use the same steps.
pub fn render(
    input: &Path,
    denoise: Option<(&Mutex<Denoiser>, f32)>,
    look: Look,
    progress: &dyn Fn(f32),
) -> Result<Rendered> {
    let timing = Timing::start();
    let loaded = load(input, denoise, &look.capture, progress)?;
    let areas = loaded.areas(look)?;
    let image = loaded.develop(&loaded.cam, look, &areas);
    timing.mark(t("현상", "develop"));
    progress(0.75);
    Ok(Rendered {
        image,
        exif: loaded.exif,
    })
}

/// Converts one RAW to HEIF. `progress` receives progress (0..1).
/// Returns a message if there is something to report (when quality was lowered).
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

    // Write to a temp file, then rename. A crash midway leaves no half-written output file.
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

/// Logs the export result: failed photos and why, warnings, and a summary.
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

/// Memory to reserve for processing one photo (bytes). `size` is the sensor width and height in
/// pixels. A frame adds as much as it enlarges the image. Whether the photo is landscape or
/// portrait can't be told from the header alone, so the larger of the two is reserved.
/// With masks or skin adjustment (`extra`), reserves an area and one more image on top (16 bytes
/// per pixel).
pub fn memory_needed((width, height): (usize, usize), frame: Option<&Frame>, extra: bool) -> u64 {
    let growth = frame.map_or(1.0, |frame| {
        frame.growth(width, height).max(frame.growth(height, width))
    });
    let extra = if extra { 1.25 } else { 1.0 };
    ((width * height) as f64 * BYTES_PER_PIXEL as f64 * growth * extra) as u64
}

/// Number of logical cores.
pub fn cores() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}

/// Time per stage. Printed only when the `AMSIL_TIMING` environment variable is set (for
/// performance tuning).
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

/// Loads the denoise model. If the model file is missing, explains how to download it.
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

/// If running on the CPU because the GPU is unavailable, a message saying it is slow and how to
/// fix it.
pub fn cpu_notice(denoiser: &Denoiser) -> Option<Text> {
    let gpu::Backend::Cpu { reason } = &denoiser.backend else {
        return None;
    };
    // One 24-megapixel photo is 48 tiles.
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

/// What is reported periodically while the batch runs.
#[derive(Debug, Clone, Copy)]
pub struct Report {
    pub status: job::Status,
    /// Time left (seconds). None if not known yet.
    pub remaining: Option<f64>,
    /// Time since the last progress (seconds).
    pub idle: f64,
    /// This program's CPU usage (% of all cores).
    pub cpu: f32,
    /// Memory this program uses, and the limit (bytes).
    pub memory: u64,
    pub memory_limit: u64,
    /// Share of time the AI model was actually running (%).
    pub gpu: f32,
    /// System memory is low, so fewer photos run at once.
    pub low_memory: bool,
}

/// Measures resource usage and delays new jobs when system memory runs low.
struct Monitor {
    system: sysinfo::System,
    pid: sysinfo::Pid,
    cores: f32,
    memory_limit: u64,
    started: Instant,
    eta: job::Eta,
    /// Time of the last measurement and the cumulative GPU time at that point.
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

        // Measure resources at most once a second.
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

            // When system-wide free memory drops below 10%, start new jobs one at a time.
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

/// Decides how many photos to process at once and how many threads each encoder uses.
///
/// Runs only as many photos at once as memory allows, and gives the spare cores to encoder
/// threads.
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

/// Converts the photos. Reports progress through `report` and returns warnings with the result.
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
    // Each LUT file is read only once. One that fails to load fails only the photos that use it.
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
    // ponytail: the first photo's size is used to size up the whole batch. This can be off in
    // folders with mixed sizes, but actual admission is computed per photo, so safety is
    // unaffected.
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
            let mut system = sysinfo::System::new().expect("cannot read system information");
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
                    capture: photo.capture,
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

    /// Adjustment values only, with no LUT, masks or frame.
    fn bare(params: &Params) -> Look<'_> {
        Look::new(params)
    }

    /// Mean absolute Laplacian of the green plane, over the 64×64 tiles with the highest 15%
    /// contrast (detail) and the lowest 25% (flat areas).
    fn detail_and_flat(image: &crate::Image) -> (f64, f64) {
        let (w, h) = (image.width, image.height);
        let g = &image.planes[1];
        let mut tiles: Vec<(f64, f64)> = Vec::new(); // (contrast, mean Laplacian)
        for ty in (64..h - 128).step_by(64) {
            for tx in (64..w - 128).step_by(64) {
                let (mut lap, mut grad) = (0f64, 0f64);
                for y in ty..ty + 64 {
                    for x in tx..tx + 64 {
                        let c = g[y * w + x] as f64;
                        lap += (4.0 * c
                            - g[(y - 1) * w + x] as f64
                            - g[(y + 1) * w + x] as f64
                            - g[y * w + x - 1] as f64
                            - g[y * w + x + 1] as f64)
                            .abs();
                        grad += (g[y * w + x + 2] as f64 - g[y * w + x - 2] as f64).abs();
                    }
                }
                tiles.push((grad, lap / 4096.0));
            }
        }
        tiles.sort_by(|a, b| a.0.total_cmp(&b.0));
        let n = tiles.len();
        let mean = |part: &[(f64, f64)]| part.iter().map(|t| t.1).sum::<f64>() / part.len() as f64;
        (mean(&tiles[n - n * 15 / 100..]), mean(&tiles[..n / 4]))
    }

    /// Renders images for demosaic comparison (read by `scripts/sharpness/demosaic.py`).
    /// `cargo test --release -- --ignored render_for_demosaic_comparison`
    ///
    /// For each sample, reconstructs color with RCD (current), rawler's PPG (former) and the AI
    /// denoise model (level 1, when present), and writes a 2048×2048 crop developed without capture
    /// sharpening or adjustments to `demosaic/`.
    #[test]
    #[ignore = "needs sample photos"]
    fn render_for_demosaic_comparison() {
        use crate::raw16::{PHOTOS, SIDE};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let out = crate::raw16::dir().join("demosaic");
        std::fs::create_dir_all(&out).unwrap();
        let model = load_denoiser().ok().map(Mutex::new);
        let params = Params::default();
        for (photo, y0, x0) in PHOTOS {
            let name = photo.rsplit('/').next().unwrap();
            let path = root.join(format!("DCIM/{photo}.ARW"));
            let raw = raw::decode(&path).unwrap();
            let upright = |cam: &crate::Image| {
                let image = develop::develop(cam, raw.wb, raw.xyz_to_cam, &params);
                let (transpose, flip_h, flip_v) = raw.flips;
                develop::orient(image, transpose, flip_h, flip_v)
            };
            let mut methods = vec![
                ("rcd", upright(&raw.demosaic().unwrap())),
                ("ppg", upright(&raw.demosaic_ppg_for_tests().unwrap())),
            ];
            if let Some(model) = &model {
                let loaded = read(&path, Some((model, 1.0)), &|_| {})
                    .unwrap()
                    .sharpened(&Capture::off());
                methods.push(("ai", upright(&loaded.cam)));
            }
            for (method, image) in methods {
                let file = out.join(format!("{name}-{method}.raw"));
                crate::raw16::write(&image, (x0, y0), (SIDE, SIDE), &file);
                eprintln!("{}", file.display());
            }
        }
    }

    /// Renders images for sharpness measurement (read by `scripts/sharpness/measure.py`).
    /// `cargo test --release -- --ignored render_for_sharpness_measurement`
    ///
    /// Develops 6 samples without adjustments for each capture sharpening condition and writes a
    /// detailed 2048×2048 crop as 16-bit (`-pre` before compression, `-post` after compressing at
    /// quality 55 and decoding). Compressed size and time per photo go to `results.tsv`.
    #[test]
    #[ignore = "needs sample photos and takes a few minutes"]
    fn render_for_sharpness_measurement() {
        use crate::raw16::{PHOTOS, SIDE};
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let out = crate::raw16::dir();
        let base = Capture::default();
        let t = |threshold: f32| Capture {
            threshold: Some(threshold),
            ..base
        };
        // (name, capture sharpening, creative sharpening added on top)
        let conditions = [
            ("off", Capture::off(), 0.0),
            (
                "it20",
                Capture {
                    threshold: None,
                    ..base
                },
                0.0,
            ),
            (
                "it10",
                Capture {
                    iterations: 10,
                    threshold: None,
                    ..base
                },
                0.0,
            ),
            (
                "it40",
                Capture {
                    iterations: 40,
                    threshold: None,
                    ..base
                },
                0.0,
            ),
            ("it20-t0.2", t(0.2), 0.0),
            ("it20-t0.3", t(0.3), 0.0),
            ("it20-t0.4", t(0.4), 0.0),
            ("it20-t0.5", t(0.5), 0.0),
            (
                "it10-t0.2",
                Capture {
                    iterations: 10,
                    threshold: Some(0.2),
                    ..base
                },
                0.0,
            ),
            ("it20-t0.4-s0.1", t(0.4), 0.1),
            ("it20-t0.4-s0.3", t(0.4), 0.3),
        ];
        let mut results = String::from("photo\tcondition\tbytes\tseconds\tradius\tthreshold\n");
        for (photo, y0, x0) in PHOTOS {
            let name = photo.rsplit('/').next().unwrap();
            let read = read(&root.join(format!("DCIM/{photo}.ARW")), None, &|_| {}).unwrap();
            let (radius, threshold) = read.auto();
            for (label, capture, sharpen) in conditions {
                let started = std::time::Instant::now();
                let loaded = read.sharpened(&capture);
                let params = Params {
                    sharpen,
                    ..Params::default()
                };
                let image = loaded.develop(&loaded.cam, Look::new(&params), &Areas::default());
                let seconds = started.elapsed().as_secs_f32();
                let crop = |image: &crate::Image, suffix: &str| {
                    let path = out.join(format!("{name}-{label}-{suffix}.raw"));
                    crate::raw16::write(image, (x0, y0), (SIDE, SIDE), &path);
                };
                crop(&image, "pre");
                let encoded = heif::encode(&image, 55, None, cores()).unwrap();
                crop(&heif::decode(&encoded.bytes).unwrap(), "post");
                results.push_str(&format!(
                    "{name}\t{label}\t{}\t{seconds:.2}\t{radius:.3}\t{threshold:.3}\n",
                    encoded.bytes.len()
                ));
                eprintln!(
                    "{name} {label}: {} bytes, {seconds:.2} s",
                    encoded.bytes.len()
                );
            }
        }
        std::fs::write(out.join("results.tsv"), results).unwrap();
    }

    #[test]
    fn the_sharpened_area_is_shown_in_red_or_as_a_map() {
        let Some(path) = sample_arw() else { return };
        let loaded = read(&path, None, &|_| {})
            .unwrap()
            .sharpened(&Capture::default());
        let params = Params::default();
        let develop = |view| {
            let look = Look {
                capture_view: view,
                ..Look::new(&params)
            };
            loaded.develop(&loaded.cam, look, &Areas::default())
        };
        let (plain, red, map) = (
            develop(None),
            develop(Some(CaptureView::Overlay)),
            develop(Some(CaptureView::Map)),
        );
        assert_eq!((map.width, map.height), (plain.width, plain.height));
        // A monochrome map is gray.
        let brightest = (0..map.planes[0].len())
            .max_by(|&a, &b| map.planes[0][a].total_cmp(&map.planes[0][b]))
            .unwrap();
        assert!(map.planes[0][brightest] > 0.5);
        assert_eq!(map.planes[0][brightest], map.planes[2][brightest]);
        // White areas turn red in the red overlay; black areas stay as they are.
        let redness = |image: &crate::Image, i: usize| image.planes[0][i] - image.planes[2][i];
        assert!(redness(&red, brightest) > redness(&plain, brightest) + 0.05);
        let darkest = (0..map.planes[0].len())
            .min_by(|&a, &b| map.planes[0][a].total_cmp(&map.planes[0][b]))
            .unwrap();
        assert!(map.planes[0][darkest] < 0.01);
        assert!((red.planes[0][darkest] - plain.planes[0][darkest]).abs() < 0.01);
        // Developing at a small size still fits the same shape.
        let small = crate::preview::downscale(&loaded.cam, 1200).unwrap();
        let look = Look {
            capture_view: Some(CaptureView::Map),
            ..Look::new(&params)
        };
        let small_map = loaded.develop(&small, look, &Areas::default());
        assert_eq!(small_map.width.max(small_map.height), 1200);
    }

    #[test]
    fn capture_sharpening_adds_detail_but_not_in_flat_areas() {
        let Some(path) = sample_arw() else { return };
        let read = read(&path, None, &|_| {}).unwrap();
        let off = read.sharpened(&Capture::off());
        let on = read.sharpened(&Capture::default());
        let (detail_off, flat_off) = detail_and_flat(&off.cam);
        let (detail_on, flat_on) = detail_and_flat(&on.cam);
        eprintln!("detail {detail_off:.5} → {detail_on:.5}, flat {flat_off:.5} → {flat_on:.5}");
        // The default (threshold 0.4) sharpens only clear edges. On this photo (f/2.8, mostly out
        // of focus), detail rises a little.
        assert!(
            detail_on > detail_off * 1.03,
            "detail {detail_on} {detail_off}"
        );
        assert!(flat_on < flat_off * 1.02, "flat {flat_on} {flat_off}");
        // With the threshold derived from the photo (RawTherapee's auto), it sharpens wider and
        // harder.
        let auto = read.sharpened(&Capture {
            threshold: None,
            ..Capture::default()
        });
        let (detail_auto, flat_auto) = detail_and_flat(&auto.cam);
        eprintln!("auto threshold: detail {detail_auto:.5}, flat {flat_auto:.5}");
        assert!(
            detail_auto > detail_on * 1.2,
            "auto {detail_auto} default {detail_on}"
        );
        assert!(flat_auto < flat_off * 1.05, "flat {flat_auto} {flat_off}");
        let (radius, threshold) = read.auto();
        eprintln!("auto radius {radius}, auto threshold {threshold}");
        assert!((0.4..=2.0).contains(&radius) && threshold >= 0.0);
        // The mask exists only when sharpening; load leaves it empty.
        assert_eq!(on.capture_mask.len(), on.cam.width * on.cam.height);
        assert!(
            load(&path, None, &Capture::default(), &|_| {})
                .unwrap()
                .capture_mask
                .is_empty()
        );
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
        // A portrait photo shot as 6048×4032 comes out upright.
        assert_eq!((handle.width(), handle.height()), (4032, 6048));
        assert_eq!(handle.luma_bits_per_pixel(), 10);
        assert_eq!(handle.number_of_metadata_blocks(b"Exif"), 1);
        // No temp file is left behind.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Reads the output file and returns its per-channel means (0..255) and size.
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
                "channel {c}: {mean_a:?} vs {mean_b:?}"
            );
        }
        // If it really went through the model, the result can't be exactly the same.
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
            "with plenty of memory, as many as cores"
        );
        assert_eq!(
            plan(32, None, 4 * GIB, photo, 309),
            (3, 10),
            "memory for 3 photos means 3, with 10 encoder threads each"
        );
        assert_eq!(
            plan(32, Some(4), 40 * GIB, photo, 309),
            (4, 8),
            "reduced with --jobs"
        );
        assert_eq!(
            plan(8, None, GIB, photo, 309),
            (1, 8),
            "one at a time if memory is short of even one photo"
        );
        assert_eq!(
            plan(32, None, 40 * GIB, photo, 2),
            (2, 16),
            "with only two photos, they split the cores"
        );
    }

    /// Per-channel means of the output image.
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
        // LUT that removes red
        let no_red = crate::lut::Lut::parse(
            "LUT_3D_SIZE 2\n0 0 0\n0 0 0\n0 1 0\n0 1 0\n0 0 1\n0 0 1\n0 1 1\n0 1 1\n",
        )
        .unwrap();
        let loaded = load(&input, None, &Capture::off(), &|_| {}).unwrap();
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
            "{same:?} vs {plain:?}"
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
                capture: Capture::off(),
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
                capture: Capture::off(),
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
        let loaded = load(&input, None, &Capture::off(), &|_| {}).unwrap();
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
        // A portrait photo. The strip must be attached below after making it upright.
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
        assert_eq!(
            framed.planes[0].last(),
            Some(&1.0),
            "the bottom is the white strip"
        );
        let band = &framed.planes[0][plain.width * plain.height..];
        assert!(
            band.iter().any(|&v| v < 0.5),
            "the strip should carry the shooting info"
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
        // A strip 10% of the short side (404) below a 4032×6048 photo
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
        // Putting a landscape photo in a portrait 9:16 frame nearly triples its size. Orientation
        // can't be told from the header alone, so the larger case is reserved.
        let tall = memory_needed((6080, 4048), Some(&frame(Layout::Ratio(9, 16))), false);
        assert!(tall as f64 / plain as f64 > 2.9, "{tall}");
        assert_eq!(
            tall,
            memory_needed((4048, 6080), Some(&frame(Layout::Ratio(9, 16))), false)
        );
    }

    /// Low-resolution mask selecting the left half.
    fn left_half() -> crate::mask::Low {
        crate::mask::Low {
            width: 4,
            height: 4,
            logits: [8.0, 8.0, -8.0, -8.0].repeat(4),
        }
    }

    /// Per-channel means over the horizontal span `from..to` (0..1) of the image.
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
        let loaded = load(&input, None, &Capture::off(), &|_| {}).unwrap();
        let small = crate::preview::downscale(&loaded.cam, 400).unwrap();
        let params = Params::default();
        let plain = loaded.develop(&small, bare(&params), &Areas::default());
        // Test with a darkening adjustment. When brightening, an already bright sky has no room to
        // get brighter.
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
            "the selected side gets darker: {}",
            change(&left, &masked)
        );
        assert!(
            (change(&right, &masked) - 1.0).abs() < 0.01,
            "the rest is unchanged: {}",
            change(&right, &masked)
        );

        // Inverted, the other side gets darker.
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
        let loaded = load(&input, None, &Capture::off(), &|_| {}).unwrap();
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
            "the unselected side is unchanged: {before:?} → {after:?}"
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
            capture: Capture::off(),
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
        // Needs a mask area plus one more image with different adjustments.
        assert_eq!(memory_needed((6080, 4048), None, true), plain / 4 * 5);
    }

    #[test]
    fn a_real_click_brightens_the_building_and_leaves_the_sky() {
        use crate::mask::{Mask, Point};
        // Runs only when the mask model and sample photo are present.
        let photo =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110/DSC04551.ARW");
        let Ok(segmenter) = crate::mask::load_segmenter() else {
            eprintln!("skipped: no mask model");
            return;
        };
        if !photo.exists() {
            return;
        }
        let segmenter = Mutex::new(segmenter);
        let loaded = load(&photo, None, &Capture::off(), &|_| {}).unwrap();
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
        // How much longer a mask takes at full size (to gauge performance)
        let started = Instant::now();
        loaded.develop(&loaded.cam, bare(&params), &Areas::default());
        let without = started.elapsed();
        let started = Instant::now();
        loaded.develop(&loaded.cam, look, &areas);
        eprintln!(
            "areas {selected:.2?}, full-size develop: no mask {without:.2?}, one mask {:.2?}",
            started.elapsed()
        );
        let at = |image: &crate::Image, x: f32, y: f32| {
            image.planes[1][(y * image.height as f32) as usize * image.width
                + (x * image.width as f32) as usize]
        };
        assert!(
            at(&masked, 0.48, 0.35) > at(&plain, 0.48, 0.35) * 1.2,
            "the building gets brighter"
        );
        assert!(
            (at(&masked, 0.1, 0.1) - at(&plain, 0.1, 0.1)).abs() < 0.01,
            "the sky is unchanged"
        );
    }

    #[test]
    fn skin_retouch_changes_only_the_skin_area() {
        let Some(input) = sample_arw() else { return };
        let loaded = load(&input, None, &Capture::off(), &|_| {}).unwrap();
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
        // Mean per-pixel change over a horizontal span of the image
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
            "the skin side changes: {}",
            moved(0.0, 0.3)
        );
        assert!(
            moved(0.7, 1.0) < 1e-6,
            "the rest is unchanged: {}",
            moved(0.7, 1.0)
        );
        // At strength 0, the area is unused even if present.
        let untouched = loaded.develop(&small, bare(&params), &areas);
        assert_eq!(untouched.planes, plain.planes);
    }

    #[test]
    fn skin_is_looked_for_only_when_the_retouch_is_on() {
        // Runs only when the skin model and sample photo are present.
        let Some(input) = sample_arw() else { return };
        let Ok(finder) = crate::skin::load_finder() else {
            eprintln!("skipped: no skin model");
            return;
        };
        let finder = Mutex::new(finder);
        let loaded = load(&input, None, &Capture::off(), &|_| {}).unwrap();
        let params = Params::default();
        let off = Look {
            skin_finder: Some(&finder),
            ..bare(&params)
        };
        assert!(loaded.areas(off).unwrap().skin.is_none());
        let on = Look { skin: 0.5, ..off };
        assert!(loaded.areas(on).unwrap().skin.is_some());
        // Turned on without the model, it reports why.
        let missing = Look {
            skin: 0.5,
            ..bare(&params)
        };
        assert!(loaded.areas(missing).is_err());
    }
}
