//! Runs an `Effect` on a background thread and sends the result back as an `Action`.

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

/// Long side of a thumbnail (pixels).
const THUMBNAIL_SIDE: usize = 160;
/// Long side of the image used to classify a photo (pixels). Generous so the short side doesn't
/// drop below the model input (224).
const CLASSIFY_SIDE: usize = 448;
/// Default long side of the preview (pixels). Replaced by the size the GPU accepts once the UI
/// reports it.
const PREVIEW_SIDE: usize = 2048;

type SharedDenoiser = Arc<Mutex<Denoiser>>;
/// (file path, loaded LUT)
type CachedLut = (PathBuf, Arc<Lut>);

/// Long side of the small preview shown right away when adjustments change (pixels).
const QUICK_SIDE: usize = 1200;

/// A loaded photo. When only adjustments change, it is re-developed from this without re-reading.
struct Base {
    path: PathBuf,
    denoise: f32,
    /// Result of RAW reading and demosaicing (or denoising); input to capture sharpening.
    read: export::Read,
    /// The capture-sharpened image. Reused while the capture sharpening value is unchanged.
    sharpened: Mutex<Option<Arc<Sharpened>>>,
}

/// A capture-sharpened photo and what the models derived from it.
struct Sharpened {
    capture: Capture,
    loaded: export::Loaded,
    /// Camera RGB scaled down for the quick preview. None if the original is already small.
    small: Option<crate::Image>,
    /// Mask model encoder output. Made the first time a mask is used on this photo.
    embedding: Mutex<Option<Arc<mask::Embedding>>>,
    /// Areas already computed from the clicked points. Avoids recomputing while an adjustment
    /// slider is dragged.
    areas: Mutex<Vec<(Vec<Point>, Low)>>,
    /// Where the skin is. Found the first time skin adjustment is used on this photo.
    skin: Mutex<Option<Low>>,
}

#[derive(Clone)]
pub struct Workers {
    tx: Sender<Action>,
    /// Signals that a result has arrived and the screen should be redrawn.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// Denoising model. Loaded when first needed and shared by preview and export.
    denoiser: Arc<Mutex<Option<SharedDenoiser>>>,
    cancel: Arc<AtomicBool>,
    /// Number of the latest preview request.
    latest_preview: Arc<AtomicU64>,
    /// Limit on the preview's long side.
    preview_side: Arc<AtomicU64>,
    /// Number of the latest before-adjustment image request.
    latest_original: Arc<AtomicU64>,
    /// Before-adjustment images are read one at a time (so heavy reads don't pile up when flipping
    /// through photos quickly).
    original_lock: Arc<Mutex<()>>,
    /// The one photo being previewed.
    base: Arc<Mutex<Option<Arc<Base>>>>,
    /// The last loaded LUT. Avoids re-reading the file while the strength slider is dragged.
    lut: Arc<Mutex<Option<CachedLut>>>,
    /// Photo classification model. Loaded when first needed.
    classifier: Arc<Mutex<Option<Arc<Mutex<Classifier>>>>>,
    /// Mask model. Loaded when first needed.
    segmenter: Arc<Mutex<Option<Arc<Mutex<Segmenter>>>>>,
    /// Skin detection model. Loaded when first needed.
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
        // If the receiver is gone, the program is shutting down.
        let _ = self.tx.send(action);
        (self.wake)();
    }

    /// Channel for results that arrive later from the UI side, such as screen captures.
    pub fn sender(&self) -> impl Fn(Action) + Send + Sync + 'static {
        let workers = self.clone();
        move |action| workers.send(action)
    }

    /// Sets how large previews are made. As large as the GPU texture allows, so they can be viewed
    /// zoomed in.
    pub fn set_preview_side(&self, side: usize) {
        self.preview_side.store(side as u64, SeqCst);
    }

    /// Loads the models to check whether the GPU can be used. Done at startup and on language
    /// change.
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
            // The mask model is large and many people never use it. Only check that the file
            // exists here.
            workers.send(Action::Segmenter(mask::missing()));
            workers.send(Action::SkinModel(skin::missing()));
        });
    }

    fn denoiser(&self) -> std::result::Result<SharedDenoiser, Text> {
        let mut slot = self.denoiser.lock().unwrap();
        if let Some(denoiser) = slot.as_ref() {
            return Ok(denoiser.clone());
        }
        // Failures aren't remembered. Once the model file is downloaded, it loads next time.
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

    /// Computes the areas for a preview (mask selections, skin). Model results are kept on the
    /// photo and reused.
    fn areas(
        &self,
        base: &Sharpened,
        masks: &[Mask],
        skin: f32,
    ) -> std::result::Result<export::Areas, Text> {
        /// Number of mask areas remembered per photo.
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

    /// Reads the photo. Skips re-reading if it was just read (at the same denoise level). `true` if
    /// it was newly read. Holds the lock while reading so concurrent requests read only once.
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

    /// Capture-sharpens the loaded photo. Reuses the result if already done with the same value.
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

    // ponytail: reuse is keyed on the path alone. If a LUT file is edited while the program is
    // running, the old contents still show even after picking the photo again.
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
                    // Without the classification model, only make the thumbnail.
                    let classifier = workers.classifier().ok();
                    list.into_par_iter().for_each(|(index, path)| {
                        // Read one image large enough to classify, classify it, then shrink it to
                        // thumbnail size.
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
                    // If a newer request arrived in the meantime, drop this one.
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
                        // Show a scaled-down copy quickly first, then remake it at full size.
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
                    // If a newer request arrived while waiting, don't read.
                    if stale() {
                        return;
                    }
                    // The loaded photo (`base`) belongs to the after-adjustment preview, so leave
                    // it alone. Changing it here would make the after side denoise again.
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
                    // No capture sharpening on the before image either (so the effect shows when
                    // comparing).
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
                        // If the mask or skin model fails to load, only photos that use it fail,
                        // with that reason.
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

/// Rotates the camera JPEG to its EXIF orientation and scales it so the long side fits in `side`
/// (the before pane of the before/after comparison).
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

    /// Waits until a matching result arrives. Also returns everything that came in the meantime.
    fn wait_for(rx: &Receiver<Action>, mut wanted: impl FnMut(&Action) -> bool) -> Vec<Action> {
        let mut seen = Vec::new();
        loop {
            let action = rx
                .recv_timeout(Duration::from_secs(120))
                .expect("the background job sent no result");
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
        // A portrait photo stored as 6048×4032 stands upright per its EXIF orientation.
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
            panic!("not a thumbnail")
        };
        assert_eq!(thumb.height, THUMBNAIL_SIDE);
        // With the classification model, the photo's category comes along too.
        assert_eq!(
            guess.is_some(),
            crate::gpu::model_dir()
                .join(crate::recommend::MODEL)
                .exists()
        );

        // Preview a copy of the photo (it gets deleted below).
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
        // The quick small preview comes first, followed by one near full size.
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
            "portrait photo: long side 1200 first, then 2048"
        );
        // A newly read photo also reports the capture sharpening value derived from it.
        assert!(
            seen.iter().any(|action| matches!(
                action,
                Action::CaptureAuto { index: 0, radius, threshold }
                    if (0.4..=2.0).contains(radius) && *threshold >= 0.0
            )),
            "CaptureAuto did not arrive"
        );

        // Changing only the adjustments of the same photo doesn't re-read the file. Checked by
        // getting a preview after the file is deleted. (Timing it would be flaky when other tests
        // run alongside.)
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
            panic!("only adjustments changed, yet it tried to re-read the (deleted) photo file")
        };
        let px = &quick.rgba[..4];
        assert!(
            px[0].abs_diff(px[1]) <= 1 && px[1].abs_diff(px[2]) <= 1,
            "should be monochrome: {px:?}"
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

        // Changing only capture sharpening doesn't re-read the (deleted) file, and the image
        // changes.
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
                "only capture sharpening changed, yet it re-read the (deleted) photo file: {:?}",
                seen.last().map(|_| ())
            )
        };
        assert!(
            sharpened.rgba != full_mono.rgba,
            "turning on capture sharpening should change the image"
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
            panic!("export failed")
        };
        assert_eq!((outcome.done, outcome.failed.len()), (1, 0));
        assert!(output.exists());
        assert!(
            seen.iter()
                .any(|action| matches!(action, Action::ExportProgress(_))),
            "should report progress too"
        );
        assert!(woken.load(SeqCst) >= 3, "wakes the screen for every result");
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
            "results of stale requests are not sent"
        );
    }

    #[test]
    fn a_preview_marks_the_area_a_mask_selects() {
        // Runs only when the mask model and sample photo are present.
        let photo =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110/DSC04551.ARW");
        if !photo.exists() || mask::missing().is_some() {
            eprintln!("skipped: no mask model or sample photo");
            return;
        }
        let (workers, rx) = Workers::new(|| {});
        let tower = Point {
            x: 0.48,
            y: 0.35,
            include: true,
        };
        // In the first quick preview, check the colors of the building (clicked point) and the sky.
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
                panic!("no preview arrived")
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
            "the selected building is tinted red: {tower_plain:?} → {tower_shown:?}"
        );
        assert_eq!(sky_shown, sky_plain, "the sky is unchanged");
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
            panic!("no before image arrived");
        };
        assert_eq!(
            (original.width, original.height),
            (1365, 2048),
            "same size as the after preview (long side 2048)"
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
            "should match the preview of an unadjusted photo pixel for pixel"
        );
    }
}
