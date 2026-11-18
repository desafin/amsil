//! `Effect`를 백그라운드 스레드에서 실행하고, 결과를 `Action`으로 돌려보낸다.

use crate::app::{Action, Backend, Effect};
use crate::gpu::Denoiser;
use crate::{export, preview};
use rayon::prelude::*;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering::SeqCst};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};

/// 썸네일의 긴 변 (화소).
const THUMBNAIL_SIDE: usize = 160;
/// 미리보기의 긴 변 (화소).
const PREVIEW_SIDE: usize = 2048;

type SharedDenoiser = Arc<Mutex<Denoiser>>;

#[derive(Clone)]
pub struct Workers {
    tx: Sender<Action>,
    /// 결과가 도착했으니 화면을 다시 그리라고 알린다.
    wake: Arc<dyn Fn() + Send + Sync>,
    /// 노이즈 제거 모델. 처음 필요할 때 읽고 미리보기와 내보내기가 함께 쓴다.
    denoiser: Arc<Mutex<Option<std::result::Result<SharedDenoiser, String>>>>,
    cancel: Arc<AtomicBool>,
    /// 가장 최근의 미리보기 요청 번호.
    latest_preview: Arc<AtomicU64>,
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

    /// 모델을 읽어 GPU를 쓸 수 있는지 알아본다. 프로그램을 켤 때 한 번 부른다.
    pub fn probe_backend(&self) {
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
        });
    }

    fn denoiser(&self) -> std::result::Result<SharedDenoiser, String> {
        self.denoiser
            .lock()
            .unwrap()
            .get_or_insert_with(|| {
                export::load_denoiser()
                    .map(|denoiser| Arc::new(Mutex::new(denoiser)))
                    .map_err(|e| e.to_string())
            })
            .clone()
    }

    pub fn run(&self, effect: Effect) {
        let workers = self.clone();
        match effect {
            Effect::Thumbnails(list) => {
                std::thread::spawn(move || {
                    list.into_par_iter().for_each(|(index, path)| {
                        let pixels = preview::thumbnail(&path, THUMBNAIL_SIDE).ok().flatten();
                        workers.send(Action::Thumbnail { index, pixels });
                    });
                });
            }
            Effect::Preview {
                generation,
                index,
                path,
                denoise,
            } => {
                self.latest_preview.store(generation, SeqCst);
                std::thread::spawn(move || {
                    let result = (|| {
                        let denoiser = if denoise > 0.0 {
                            Some(workers.denoiser()?)
                        } else {
                            None
                        };
                        // 그 사이에 다른 사진을 골랐으면 무거운 일을 시작하지 않는다.
                        if workers.latest_preview.load(SeqCst) != generation {
                            return Err("취소됨".to_string());
                        }
                        let model = denoiser.as_deref().map(|model| (model, denoise));
                        let rendered =
                            export::render(&path, model, &|_| {}).map_err(|e| e.to_string())?;
                        Ok(preview::shrink(&rendered.image, PREVIEW_SIDE))
                    })();
                    if workers.latest_preview.load(SeqCst) == generation {
                        workers.send(Action::PreviewReady {
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
                        export::run(
                            &photos,
                            &settings,
                            denoiser.as_deref(),
                            &workers.cancel,
                            |report| {
                                workers.send(Action::ExportProgress(*report));
                            },
                        )
                        .map_err(|e| e.to_string())
                    })();
                    workers.send(Action::ExportFinished(result));
                });
            }
            Effect::CancelExport => self.cancel.store(true, SeqCst),
        }
    }
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
        } = &seen[0]
        else {
            panic!("썸네일이 아니다")
        };
        assert_eq!(thumb.height, THUMBNAIL_SIDE);

        workers.run(Effect::Preview {
            generation: 7,
            index: 0,
            path: sample.clone(),
            denoise: 0.0,
        });
        let seen = wait_for(&rx, |action| matches!(action, Action::PreviewReady { .. }));
        let Action::PreviewReady {
            generation: 7,
            index: 0,
            result: Ok(preview),
        } = seen.last().unwrap()
        else {
            panic!("미리보기가 아니다")
        };
        assert_eq!(
            (preview.width, preview.height),
            (1365, 2048),
            "세로 사진을 긴 변 2048로 줄인 크기"
        );

        let dir = std::env::temp_dir().join(format!("amsil-worker-{}", std::process::id()));
        let output = dir.join("sub/out.heic");
        let photos = vec![export::Photo {
            input: sample,
            output: output.clone(),
            denoise: 0.0,
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
        });
        workers.run(Effect::Preview {
            generation: 2,
            index: 1,
            path: sample,
            denoise: 0.0,
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
}
