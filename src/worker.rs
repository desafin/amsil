//! `Effect`를 백그라운드 스레드에서 실행하고, 결과를 `Action`으로 돌려보낸다.

use crate::app::{Action, Backend, Effect};
use crate::capture::Capture;
use crate::gpu::Denoiser;
use crate::lang::Text;
use crate::lut::Lut;
use crate::mask::{self, Low, Mask, Point, Segmenter};
use crate::recommend::{self, Classifier};
use crate::skin::{self, SkinFinder};
use crate::{export, preview};
use rayon::prelude::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// 썸네일의 긴 변 (화소).
const THUMBNAIL_SIDE: usize = 160;
/// 사진을 분류할 때 쓰는 그림의 긴 변 (화소). 짧은 변이 모델 입력(224)보다 작아지지 않게 넉넉히 잡는다.
const CLASSIFY_SIDE: usize = 448;
/// 미리보기의 긴 변 (화소)의 기본값. 화면 쪽에서 GPU가 받을 수 있는 크기를 알려 주면 그 값으로 바뀐다.
const PREVIEW_SIDE: usize = 2048;

type SharedDenoiser = Arc<Mutex<Denoiser>>;
/// (파일 경로, 읽은 LUT)
type CachedLut = (PathBuf, Arc<Lut>);

/// 보정을 바꿀 때 바로 보여 주는 작은 미리보기의 긴 변 (화소).
const QUICK_SIDE: usize = 1200;

/// 읽어 둔 사진. 보정만 바꿀 때는 다시 읽지 않고 이것으로 현상만 다시 한다.
struct Base {
    path: PathBuf,
    denoise: f32,
    /// RAW 읽기와 디모자익(또는 노이즈 제거), 캡처 샤픈의 재료.
    read: export::Read,
    /// 캡처 샤픈한 것. 캡처 샤픈 값이 같으면 다시 쓴다.
    sharpened: Mutex<Option<Arc<Sharpened>>>,
}

/// 캡처 샤픈까지 한 사진과, 그 그림에서 모델로 구한 것들.
struct Sharpened {
    capture: Capture,
    loaded: export::Loaded,
    /// 빠른 미리보기용으로 줄인 카메라 RGB. 원본이 이미 작으면 없다.
    small: Option<crate::Image>,
    /// 마스크 모델의 인코더 출력. 이 사진에 마스크를 처음 쓸 때 만든다.
    embedding: Mutex<Option<Arc<mask::Embedding>>>,
    /// 누른 자리들로 이미 구해 둔 영역. 보정 슬라이더를 끄는 동안 다시 계산하지 않게 한다.
    areas: Mutex<Vec<(Vec<Point>, Low)>>,
    /// 피부인 곳. 이 사진에 피부 보정을 처음 쓸 때 구한다.
    skin: Mutex<Option<Low>>,
}

#[derive(Clone)]
pub struct Workers {
    tx: Sender<Action>,
    /// 결과가 도착했으니 화면을 다시 그리라고 알린다.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// 노이즈 제거 모델. 처음 필요할 때 읽고 미리보기와 내보내기가 함께 쓴다.
    denoiser: Arc<Mutex<Option<SharedDenoiser>>>,
    cancel: Arc<AtomicBool>,
    /// 가장 최근의 미리보기 요청 번호.
    latest_preview: Arc<AtomicU64>,
    /// 미리보기의 긴 변 한도.
    preview_side: Arc<AtomicU64>,
    /// 가장 최근의 보정 전 그림 요청 번호.
    latest_original: Arc<AtomicU64>,
    /// 보정 전 그림은 한 번에 한 장만 읽는다 (사진을 빠르게 넘길 때 무거운 읽기가 쌓이지 않게).
    original_lock: Arc<Mutex<()>>,
    /// 미리보기 중인 사진 한 장.
    base: Arc<Mutex<Option<Arc<Base>>>>,
    /// 마지막으로 읽은 LUT. 강도 슬라이더를 끄는 동안 파일을 다시 읽지 않게 한다.
    lut: Arc<Mutex<Option<CachedLut>>>,
    /// 사진 분류 모델. 처음 필요할 때 읽는다.
    classifier: Arc<Mutex<Option<Arc<Mutex<Classifier>>>>>,
    /// 마스크 모델. 처음 필요할 때 읽는다.
    segmenter: Arc<Mutex<Option<Arc<Mutex<Segmenter>>>>>,
    /// 피부를 찾는 모델. 처음 필요할 때 읽는다.
    skin_finder: Arc<Mutex<Option<Arc<Mutex<SkinFinder>>>>>,
}

impl Workers {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> (Self, Receiver<Action>) {
        let (tx, rx) = channel();
        let workers = Self {
            tx,
            wake: Arc::new(wake),
            denoiser: Arc::default(),
            cancel: Arc::default(),
            latest_preview: Arc::default(),
            preview_side: Arc::new(AtomicU64::new(PREVIEW_SIDE as u64)),
            latest_original: Arc::default(),
            original_lock: Arc::default(),
            base: Arc::default(),
            lut: Arc::default(),
            classifier: Arc::default(),
            segmenter: Arc::default(),
            skin_finder: Arc::default(),
        };
        (workers, rx)
    }

    fn send(&self, action: Action) {
        // 받는 쪽이 사라졌다면 프로그램이 끝나는 중이다.
        let _ = self.tx.send(action);
        (self.wake)();
    }

    /// 화면 갈무리처럼 화면 쪽에서 나중에 도착하는 결과를 넣는 통로.
    pub fn sender(&self) -> impl Fn(Action) + Send + Sync + 'static {
        let workers = self.clone();
        move |action| workers.send(action)
    }

    /// 미리보기를 얼마나 크게 만들지 정한다. 확대해서 볼 수 있도록 GPU 텍스처가 허용하는 만큼 크게 준다.
    pub fn set_preview_side(&self, side: usize) {
        self.preview_side.store(side as u64, SeqCst);
    }

    /// 모델을 읽어 GPU를 쓸 수 있는지 알아본다. 프로그램을 켤 때와 언어를 바꿀 때 한다.
    fn probe_backend(&self) {
        let workers = self.clone();
        std::thread::spawn(move || {
            let backend = match workers.denoiser() {
                Ok(denoiser) => match export::cpu_notice(&denoiser.lock().unwrap()) {
                    Some(notice) => Backend::Cpu(notice),
                    None => Backend::Gpu,
                },
                Err(reason) => Backend::Unavailable(reason),
            };
            workers.send(Action::Backend(backend));
            workers.send(Action::Recommender(workers.classifier().err()));
            // 마스크 모델은 크고, 쓰지 않는 사람도 많다. 여기서는 파일이 있는지만 본다.
            workers.send(Action::Segmenter(mask::missing()));
            workers.send(Action::SkinModel(skin::missing()));
        });
    }

    fn denoiser(&self) -> std::result::Result<SharedDenoiser, Text> {
        let mut slot = self.denoiser.lock().unwrap();
        if let Some(denoiser) = slot.as_ref() {
            return Ok(denoiser.clone());
        }
        // 실패는 기억해 두지 않는다. 모델 파일을 받아 놓으면 다음에는 읽힌다.
        let denoiser = Arc::new(Mutex::new(
            export::load_denoiser().map_err(|e| Text::of(&e))?,
        ));
        *slot = Some(denoiser.clone());
        Ok(denoiser)
    }

    fn classifier(&self) -> std::result::Result<Arc<Mutex<Classifier>>, Text> {
        let mut slot = self.classifier.lock().unwrap();
        if let Some(classifier) = slot.as_ref() {
            return Ok(classifier.clone());
        }
        let classifier = Arc::new(Mutex::new(
            recommend::load_classifier().map_err(|e| Text::of(&e))?,
        ));
        *slot = Some(classifier.clone());
        Ok(classifier)
    }

    fn segmenter(&self) -> std::result::Result<Arc<Mutex<Segmenter>>, Text> {
        let mut slot = self.segmenter.lock().unwrap();
        if let Some(segmenter) = slot.as_ref() {
            return Ok(segmenter.clone());
        }
        let segmenter = Arc::new(Mutex::new(
            mask::load_segmenter().map_err(|e| Text::of(&e))?,
        ));
        *slot = Some(segmenter.clone());
        Ok(segmenter)
    }

    fn skin_finder(&self) -> std::result::Result<Arc<Mutex<SkinFinder>>, Text> {
        let mut slot = self.skin_finder.lock().unwrap();
        if let Some(finder) = slot.as_ref() {
            return Ok(finder.clone());
        }
        let finder = Arc::new(Mutex::new(skin::load_finder().map_err(|e| Text::of(&e))?));
        *slot = Some(finder.clone());
        Ok(finder)
    }

    /// 미리보기에 쓸 영역(마스크가 고른 곳, 피부)을 구한다. 모델의 결과는 사진에 붙여 두고 다시 쓴다.
    fn areas(
        &self,
        base: &Sharpened,
        masks: &[Mask],
        skin: f32,
    ) -> std::result::Result<export::Areas, Text> {
        /// 사진 한 장에 기억해 두는 마스크 영역의 수.
        const REMEMBERED: usize = 16;
        let mut areas = export::Areas::default();
        if !masks.is_empty() {
            let segmenter = self.segmenter()?;
            let mut segmenter = segmenter.lock().unwrap();
            let embedding = {
                let mut slot = base.embedding.lock().unwrap();
                match slot.as_ref() {
                    Some(embedding) => embedding.clone(),
                    None => {
                        let embedding = Arc::new(
                            segmenter
                                .embed(&base.loaded.model_picture())
                                .map_err(|e| Text::of(&e))?,
                        );
                        *slot = Some(embedding.clone());
                        embedding
                    }
                }
            };
            let mut known = base.areas.lock().unwrap();
            for mask in masks {
                if let Some((_, area)) = known.iter().find(|(points, _)| *points == mask.points) {
                    areas.masks.push(area.clone());
                    continue;
                }
                let area = segmenter
                    .select(&embedding, &mask.points)
                    .map_err(|e| Text::of(&e))?;
                if known.len() >= REMEMBERED {
                    known.remove(0);
                }
                known.push((mask.points.clone(), area.clone()));
                areas.masks.push(area);
            }
        }
        if skin > 0.0 {
            let mut slot = base.skin.lock().unwrap();
            if slot.is_none() {
                let finder = self.skin_finder()?;
                let found = finder
                    .lock()
                    .unwrap()
                    .find(&base.loaded.model_picture())
                    .map_err(|e| Text::of(&e))?;
                *slot = Some(found);
            }
            areas.skin = slot.clone();
        }
        Ok(areas)
    }

    /// 사진을 읽는다. 방금 읽은 사진(같은 노이즈 제거 레벨)이면 다시 읽지 않는다. 새로 읽었으면 `true`.
    /// 여러 요청이 동시에 와도 한 번만 읽도록, 읽는 동안 잠금을 쥐고 있는다.
    fn base(&self, path: &Path, denoise: f32) -> std::result::Result<(Arc<Base>, bool), Text> {
        let mut slot = self.base.lock().unwrap();
        if let Some(base) = slot
            .as_ref()
            .filter(|base| base.path == path && base.denoise == denoise)
        {
            return Ok((base.clone(), false));
        }
        let denoiser = if denoise > 0.0 {
            Some(self.denoiser()?)
        } else {
            None
        };
        let model = denoiser.as_deref().map(|model| (model, denoise));
        let read = export::read(path, model, &|_| {}).map_err(|e| Text::of(&e))?;
        let base = Arc::new(Base {
            path: path.to_owned(),
            denoise,
            read,
            sharpened: Mutex::default(),
        });
        *slot = Some(base.clone());
        Ok((base, true))
    }

    /// 읽어 둔 사진을 캡처 샤픈한다. 같은 값으로 이미 했으면 다시 쓴다.
    fn sharpened(&self, base: &Base, capture: &Capture) -> Arc<Sharpened> {
        let mut slot = base.sharpened.lock().unwrap();
        if let Some(done) = slot.as_ref().filter(|done| done.capture == *capture) {
            return done.clone();
        }
        let loaded = base.read.sharpened(capture);
        let small = preview::downscale(&loaded.cam, QUICK_SIDE);
        let done = Arc::new(Sharpened {
            capture: *capture,
            loaded,
            small,
            embedding: Mutex::default(),
            areas: Mutex::default(),
            skin: Mutex::default(),
        });
        *slot = Some(done.clone());
        done
    }

    // ponytail: 경로만 보고 재사용한다. 프로그램을 켜 둔 채 LUT 파일을 고치면 사진을 다시 골라도 옛 내용이 보인다.
    fn lut(&self, path: &Path) -> std::result::Result<Arc<Lut>, Text> {
        let mut slot = self.lut.lock().unwrap();
        if let Some((_, lut)) = slot.as_ref().filter(|(cached, _)| cached == path) {
            return Ok(lut.clone());
        }
        let lut = Arc::new(Lut::load(path).map_err(|e| Text::of(&e))?);
        *slot = Some((path.to_owned(), lut.clone()));
        Ok(lut)
    }

    pub fn run(&self, effect: Effect) {
        let workers = self.clone();
        match effect {
            Effect::Thumbnails(list) => {
                std::thread::spawn(move || {
                    // 분류 모델이 없으면 썸네일만 만든다.
                    let classifier = workers.classifier().ok();
                    list.into_par_iter().for_each(|(index, path)| {
                        // 분류에 쓸 만큼 큰 그림을 한 번 읽어 분류하고, 썸네일 크기로 줄인다.
                        let picture = preview::thumbnail(&path, CLASSIFY_SIDE).ok().flatten();
                        let guess = picture.as_ref().zip(classifier.as_ref()).and_then(
                            |(picture, classifier)| {
                                classifier.lock().unwrap().classify(picture).ok()
                            },
                        );
                        let pixels =
                            picture.map(|picture| match picture.fit_within(THUMBNAIL_SIDE) {
                                Some(small) => small,
                                None => picture,
                            });
                        workers.send(Action::Thumbnail {
                            index,
                            pixels,
                            guess,
                        });
                    });
                });
            }
            Effect::Preview {
                generation,
                index,
                path,
                denoise,
                params,
                lut,
                frame,
                masks,
                overlay,
                skin,
                capture,
                capture_view,
            } => {
                self.latest_preview.store(generation, SeqCst);
                std::thread::spawn(move || {
                    // 그 사이에 더 새로운 요청이 왔으면 이 요청은 버린다.
                    let stale = || workers.latest_preview.load(SeqCst) != generation;
                    let send = |pixels| {
                        workers.send(Action::PreviewReady {
                            generation,
                            index,
                            result: Ok(pixels),
                        });
                    };
                    let result = (|| -> std::result::Result<(), Text> {
                        let lut = match &lut {
                            Some((file, strength)) => Some((workers.lut(file)?, *strength)),
                            None => None,
                        };
                        let look = export::Look {
                            lut: lut.as_ref().map(|(lut, strength)| (&**lut, *strength)),
                            frame: frame.as_ref(),
                            masks: &masks,
                            overlay,
                            skin,
                            capture_view,
                            ..export::Look::new(&params)
                        };
                        let (base, new) = workers.base(&path, denoise)?;
                        if new {
                            let (radius, threshold) = base.read.auto();
                            workers.send(Action::CaptureAuto {
                                index,
                                radius,
                                threshold,
                            });
                        }
                        if stale() {
                            return Ok(());
                        }
                        let base = workers.sharpened(&base, &capture);
                        if stale() {
                            return Ok(());
                        }
                        let areas = workers.areas(&base, &masks, skin)?;
                        if stale() {
                            return Ok(());
                        }
                        // 먼저 줄인 사본으로 빨리 보여 주고, 이어서 원본 크기로 다시 만든다.
                        if let Some(small) = &base.small {
                            send(preview::shrink(
                                &base.loaded.develop(small, look, &areas),
                                usize::MAX,
                            ));
                            if stale() {
                                return Ok(());
                            }
                        }
                        let full = base.loaded.develop(&base.loaded.cam, look, &areas);
                        if !stale() {
                            send(preview::shrink(
                                &full,
                                workers.preview_side.load(SeqCst) as usize,
                            ));
                        }
                        Ok(())
                    })();
                    if let Err(error) = result
                        && !stale()
                    {
                        workers.send(Action::PreviewReady {
                            generation,
                            index,
                            result: Err(error),
                        });
                    }
                });
            }
            Effect::Original {
                generation,
                index,
                path,
                jpeg,
            } => {
                self.latest_original.store(generation, SeqCst);
                std::thread::spawn(move || {
                    let stale = || workers.latest_original.load(SeqCst) != generation;
                    let _one_at_a_time = workers.original_lock.lock().unwrap();
                    // 기다리는 사이에 더 새로운 요청이 왔으면 읽지 않는다.
                    if stale() {
                        return;
                    }
                    // 읽어 둔 사진(`base`)은 보정 후 미리보기의 것이라 건드리지 않는다.
                    // 여기서 바꾸면 보정 후 쪽이 노이즈 제거를 다시 하게 된다.
                    let side = workers.preview_side.load(SeqCst) as usize;
                    if let Some(jpeg) = jpeg {
                        let result = camera_jpeg(&jpeg, side);
                        if !stale() {
                            workers.send(Action::OriginalReady {
                                generation,
                                index,
                                result,
                            });
                        }
                        return;
                    }
                    // 보정 전 그림에는 캡처 샤픈도 걸지 않는다 (비교하면 효과가 보이게).
                    let result =
                        export::load(&path, None, &crate::capture::Capture::off(), &|_| {})
                            .map(|loaded| {
                                let params = crate::edit::Params::default();
                                let full = loaded.develop(
                                    &loaded.cam,
                                    export::Look::new(&params),
                                    &export::Areas::default(),
                                );
                                preview::shrink(&full, workers.preview_side.load(SeqCst) as usize)
                            })
                            .map_err(|e| Text::of(&e));
                    if !stale() {
                        workers.send(Action::OriginalReady {
                            generation,
                            index,
                            result,
                        });
                    }
                });
            }
            Effect::Export { photos, settings } => {
                self.cancel.store(false, SeqCst);
                std::thread::spawn(move || {
                    let result = (|| {
                        let needs_model = photos.iter().any(|photo| photo.denoise > 0.0);
                        let denoiser = if needs_model {
                            Some(workers.denoiser()?)
                        } else {
                            None
                        };
                        // 마스크나 피부 모델을 읽지 못하면, 그것을 쓰는 사진만 그 이유와 함께 실패로 남는다.
                        let segmenter = if photos.iter().any(|photo| !photo.masks.is_empty()) {
                            workers.segmenter().ok()
                        } else {
                            None
                        };
                        let skin_finder = if photos.iter().any(|photo| photo.skin > 0.0) {
                            workers.skin_finder().ok()
                        } else {
                            None
                        };
                        export::run(
                            &photos,
                            &settings,
                            denoiser.as_deref(),
                            segmenter.as_deref(),
                            skin_finder.as_deref(),
                            &workers.cancel,
                            |report| {
                                workers.send(Action::ExportProgress(*report));
                            },
                        )
                        .map_err(|e| Text::of(&e))
                    })();
                    workers.send(Action::ExportFinished(result));
                });
            }
            Effect::CancelExport => self.cancel.store(true, SeqCst),
            Effect::ProbeBackend => self.probe_backend(),
        }
    }
}

/// 카메라 JPEG를 EXIF 방향대로 세워 긴 변이 `side` 안에 들어오게 줄인다 (전후 비교의 보정 전 칸).
pub(crate) fn camera_jpeg(path: &Path, side: usize) -> std::result::Result<preview::Pixels, Text> {
    use image::ImageDecoder;
    let read = || -> crate::Result<image::RgbaImage> {
        let mut decoder = image::ImageReader::open(path)?
            .with_guessed_format()?
            .into_decoder()?;
        let orientation = decoder.orientation()?;
        let mut image = image::DynamicImage::from_decoder(decoder)?;
        image.apply_orientation(orientation);
        Ok(image.to_rgba8())
    };
    let rgba = read().map_err(|e| {
        let shown = path.display();
        crate::lang::text!(
            format!("카메라 JPEG를 읽을 수 없습니다 ({shown}): {e}"),
            format!("Cannot read the camera JPEG ({shown}): {e}"),
        )
    })?;
    let (w, h) = (rgba.width() as usize, rgba.height() as usize);
    let long = w.max(h);
    let rgba = if long > side {
        let (out_w, out_h) = ((w * side / long).max(1), (h * side / long).max(1));
        image::imageops::resize(
            &rgba,
            out_w as u32,
            out_h as u32,
            image::imageops::FilterType::Triangle,
        )
    } else {
        rgba
    };
    Ok(preview::Pixels {
        width: rgba.width() as usize,
        height: rgba.height() as usize,
        rgba: rgba.into_raw(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// 조건에 맞는 결과가 올 때까지 기다린다. 그 사이에 온 것들도 함께 돌려준다.
    fn wait_for(rx: &Receiver<Action>, mut wanted: impl FnMut(&Action) -> bool) -> Vec<Action> {
        let mut seen = Vec::new();
        loop {
            let action = rx
                .recv_timeout(Duration::from_secs(120))
                .expect("백그라운드 작업이 결과를 보내지 않았다");
            let done = wanted(&action);
            seen.push(action);
            if done {
                return seen;
            }
        }
    }

    #[test]
    fn the_camera_jpeg_is_turned_upright_and_fitted_to_the_preview() {
        let path =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110/DSC04545.JPG");
        if !path.exists() {
            return;
        }
        let pixels = camera_jpeg(&path, 2048).unwrap();
        // 6048×4032로 저장된 세로 사진이 EXIF 방향대로 선다.
        assert_eq!((pixels.width, pixels.height), (1365, 2048));
        assert_eq!(pixels.rgba.len(), 1365 * 2048 * 4);
        let error = camera_jpeg(&path.with_extension("없음"), 2048).unwrap_err();
        assert!(error.to_string().contains("카메라 JPEG"), "{error}");
    }

    #[test]
    fn thumbnails_previews_and_exports_come_back_as_actions() {
        let Some(sample) = crate::sample_arw() else {
            return;
        };
        let woken = Arc::new(AtomicU64::new(0));
        let counter = woken.clone();
        let (workers, rx) = Workers::new(move || {
            counter.fetch_add(1, SeqCst);
        });

        workers.run(Effect::Thumbnails(vec![(3, sample.clone())]));
        let seen = wait_for(&rx, |action| matches!(action, Action::Thumbnail { .. }));
        let Action::Thumbnail {
            index: 3,
            pixels: Some(thumb),
            guess,
        } = &seen[0]
        else {
            panic!("썸네일이 아니다")
        };
        assert_eq!(thumb.height, THUMBNAIL_SIDE);
        // 분류 모델이 있으면 사진의 종류도 함께 온다.
        assert_eq!(
            guess.is_some(),
            crate::gpu::model_dir()
                .join(crate::recommend::MODEL)
                .exists()
        );

        // 미리보기는 사진의 사본으로 한다 (아래에서 지워 본다).
        let dir = std::env::temp_dir().join(format!("amsil-worker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let copy = dir.join("copy.ARW");
        std::fs::copy(&sample, &copy).unwrap();
        workers.run(Effect::Preview {
            generation: 7,
            index: 0,
            path: copy.clone(),
            denoise: 0.0,
            params: Default::default(),
            lut: None,
            frame: None,
            masks: Vec::new(),
            overlay: None,
            skin: 0.0,
            capture: Capture::off(),
            capture_view: None,
        });
        // 빠른 작은 미리보기가 먼저 오고, 원본 크기에 가까운 것이 뒤따른다.
        let mut seen = wait_for(&rx, |action| {
            matches!(action, Action::PreviewReady { generation: 7, .. })
        });
        seen.extend(wait_for(
            &rx,
            |action| matches!(action, Action::PreviewReady { generation: 7, result: Ok(preview), .. } if preview.height == 2048),
        ));
        let sizes: Vec<(usize, usize)> = seen
            .iter()
            .filter_map(|action| match action {
                Action::PreviewReady {
                    result: Ok(preview),
                    ..
                } => Some((preview.width, preview.height)),
                _ => None,
            })
            .collect();
        assert_eq!(
            sizes,
            [(800, 1200), (1365, 2048)],
            "세로 사진: 긴 변 1200짜리가 먼저, 2048짜리가 나중"
        );
        // 사진을 새로 읽으면 사진에서 구한 캡처 샤픈 값도 알려 준다.
        assert!(
            seen.iter().any(|action| matches!(
                action,
                Action::CaptureAuto { index: 0, radius, threshold }
                    if (0.4..=2.0).contains(radius) && *threshold >= 0.0
            )),
            "CaptureAuto가 오지 않았다"
        );

        // 같은 사진의 보정만 바꾸면 파일을 다시 읽지 않는다. 파일을 지워도 미리보기가 나오는 것으로 확인한다.
        // (걸린 시간으로 재면 다른 테스트가 함께 돌 때 흔들린다.)
        std::fs::remove_file(&copy).unwrap();
        let mono = crate::edit::Params {
            mono: 1.0,
            ..Default::default()
        };
        workers.run(Effect::Preview {
            generation: 8,
            index: 0,
            path: copy.clone(),
            denoise: 0.0,
            params: mono,
            lut: None,
            frame: None,
            masks: Vec::new(),
            overlay: None,
            skin: 0.0,
            capture: Capture::off(),
            capture_view: None,
        });
        let seen = wait_for(&rx, |action| {
            matches!(action, Action::PreviewReady { generation: 8, .. })
        });
        let Action::PreviewReady {
            result: Ok(quick), ..
        } = seen.last().unwrap()
        else {
            panic!("보정만 바꿨는데 (지운) 사진 파일을 다시 읽으려 했다")
        };
        let px = &quick.rgba[..4];
        assert!(
            px[0].abs_diff(px[1]) <= 1 && px[1].abs_diff(px[2]) <= 1,
            "흑백이어야 한다: {px:?}"
        );
        let full_mono = wait_for(
            &rx,
            |action| matches!(action, Action::PreviewReady { generation: 8, result: Ok(preview), .. } if preview.height == 2048),
        );
        let Some(Action::PreviewReady {
            result: Ok(full_mono),
            ..
        }) = full_mono.last()
        else {
            unreachable!()
        };

        // 캡처 샤픈 값만 바꿔도 (지운) 파일을 다시 읽지 않고, 그림은 달라진다.
        workers.run(Effect::Preview {
            generation: 9,
            index: 0,
            path: copy.clone(),
            denoise: 0.0,
            params: mono,
            lut: None,
            frame: None,
            masks: Vec::new(),
            overlay: None,
            skin: 0.0,
            capture: Capture::default(),
            capture_view: None,
        });
        let seen = wait_for(
            &rx,
            |action| matches!(action, Action::PreviewReady { generation: 9, result, .. } if result.as_ref().is_err_and(|_| true) || result.as_ref().is_ok_and(|p| p.height == 2048)),
        );
        let Some(Action::PreviewReady {
            result: Ok(sharpened),
            ..
        }) = seen.last()
        else {
            panic!(
                "캡처 샤픈만 바꿨는데 (지운) 사진 파일을 다시 읽으려 했다: {:?}",
                seen.last().map(|_| ())
            )
        };
        assert!(
            sharpened.rgba != full_mono.rgba,
            "캡처 샤픈을 켜면 그림이 달라야 한다"
        );

        let output = dir.join("sub/out.heic");
        let photos = vec![export::Photo {
            input: sample,
            output: output.clone(),
            denoise: 0.0,
            params: Default::default(),
            lut: None,
            frame: None,
            masks: Vec::new(),
            skin: 0.0,
            capture: crate::capture::Capture::default(),
        }];
        workers.run(Effect::Export {
            photos,
            settings: export::Settings::default(),
        });
        let seen = wait_for(&rx, |action| matches!(action, Action::ExportFinished(_)));
        let Action::ExportFinished(Ok((outcome, _))) = seen.last().unwrap() else {
            panic!("내보내기 실패")
        };
        assert_eq!((outcome.done, outcome.failed.len()), (1, 0));
        assert!(output.exists());
        assert!(
            seen.iter()
                .any(|action| matches!(action, Action::ExportProgress(_))),
            "진행 상황도 알려야 한다"
        );
        assert!(woken.load(SeqCst) >= 3, "결과가 올 때마다 화면을 깨운다");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_older_preview_request_is_dropped_when_a_newer_one_exists() {
        let Some(sample) = crate::sample_arw() else {
            return;
        };
        let (workers, rx) = Workers::new(|| {});
        workers.run(Effect::Preview {
            generation: 1,
            index: 0,
            path: sample.clone(),
            denoise: 0.0,
            params: Default::default(),
            lut: None,
            frame: None,
            masks: Vec::new(),
            overlay: None,
            skin: 0.0,
            capture: Capture::off(),
            capture_view: None,
        });
        workers.run(Effect::Preview {
            generation: 2,
            index: 1,
            path: sample,
            denoise: 0.0,
            params: Default::default(),
            lut: None,
            frame: None,
            masks: Vec::new(),
            overlay: None,
            skin: 0.0,
            capture: Capture::off(),
            capture_view: None,
        });
        let seen = wait_for(&rx, |action| {
            matches!(action, Action::PreviewReady { generation: 2, .. })
        });
        assert!(
            !seen
                .iter()
                .any(|action| matches!(action, Action::PreviewReady { generation: 1, .. })),
            "옛 요청의 결과는 보내지 않는다"
        );
    }

    #[test]
    fn a_preview_marks_the_area_a_mask_selects() {
        // 마스크 모델과 샘플 사진이 있을 때만 돈다.
        let photo =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110/DSC04551.ARW");
        if !photo.exists() || mask::missing().is_some() {
            eprintln!("마스크 모델이나 샘플 사진이 없어 건너뜀");
            return;
        }
        let (workers, rx) = Workers::new(|| {});
        let tower = Point {
            x: 0.48,
            y: 0.35,
            include: true,
        };
        // 첫 번째로 오는 빠른 미리보기에서, 건물(누른 자리)과 하늘의 색을 본다.
        let colours = |generation: u64, overlay: Option<usize>| {
            workers.run(Effect::Preview {
                generation,
                index: 0,
                path: photo.clone(),
                denoise: 0.0,
                params: Default::default(),
                lut: None,
                frame: None,
                masks: vec![Mask {
                    points: vec![tower],
                    ..Mask::default()
                }],
                overlay,
                skin: 0.0,
                capture: Capture::off(),
                capture_view: None,
            });
            let seen = wait_for(
                &rx,
                |action| matches!(action, Action::PreviewReady { generation: g, .. } if *g == generation),
            );
            let Some(Action::PreviewReady {
                result: Ok(pixels), ..
            }) = seen.last()
            else {
                panic!("미리보기가 오지 않았다")
            };
            let at = |x: f32, y: f32| {
                let i = ((y * pixels.height as f32) as usize * pixels.width
                    + (x * pixels.width as f32) as usize)
                    * 4;
                [pixels.rgba[i], pixels.rgba[i + 1], pixels.rgba[i + 2]]
            };
            (at(0.48, 0.35), at(0.1, 0.1))
        };
        let (tower_plain, sky_plain) = colours(1, None);
        let (tower_shown, sky_shown) = colours(2, Some(0));
        assert!(
            tower_shown[0] > tower_plain[0] + 30 && tower_shown[2] < tower_plain[2],
            "고른 건물은 붉게 표시된다: {tower_plain:?} → {tower_shown:?}"
        );
        assert_eq!(sky_shown, sky_plain, "하늘은 그대로");
    }

    #[test]
    fn the_unedited_photo_is_the_preview_of_a_photo_without_edits() {
        let Some(sample) = crate::sample_arw() else {
            return;
        };
        let (workers, rx) = Workers::new(|| {});
        workers.run(Effect::Original {
            generation: 3,
            index: 5,
            path: sample.clone(),
            jpeg: None,
        });
        let seen = wait_for(&rx, |action| matches!(action, Action::OriginalReady { .. }));
        let Some(Action::OriginalReady {
            generation: 3,
            index: 5,
            result: Ok(original),
        }) = seen.last()
        else {
            panic!("보정 전 그림이 오지 않았다");
        };
        assert_eq!(
            (original.width, original.height),
            (1365, 2048),
            "보정 후 미리보기와 같은 크기 (긴 변 2048)"
        );

        workers.run(Effect::Preview {
            generation: 1,
            index: 5,
            path: sample,
            denoise: 0.0,
            params: Default::default(),
            lut: None,
            frame: None,
            masks: Vec::new(),
            overlay: None,
            skin: 0.0,
            capture: Capture::off(),
            capture_view: None,
        });
        let seen_preview = wait_for(
            &rx,
            |action| matches!(action, Action::PreviewReady { result: Ok(preview), .. } if preview.height == 2048),
        );
        let Some(Action::PreviewReady {
            result: Ok(preview),
            ..
        }) = seen_preview.last()
        else {
            unreachable!()
        };
        assert!(
            original.rgba == preview.rgba,
            "아무 보정도 하지 않은 사진의 미리보기와 화소까지 같아야 한다"
        );
    }
}
