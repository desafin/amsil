//! 화면과 무관한 프로그램 상태.
//!
//! 사용자가 한 일과 백그라운드 작업의 결과는 모두 `Action`으로 들어온다. `App::handle`이 상태를 바꾸고,
//! 화면 밖에서 해야 할 일(썸네일 만들기, 내보내기 등)을 `Effect`로 돌려준다. 화면 코드는 상태를 읽어
//! 그리기만 한다.

use crate::export::{self, Report};
use crate::job::Outcome;
use crate::preview::Pixels;
use crate::project::{Config, Edit, Folder, Project};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

pub struct App {
    config_path: PathBuf,
    pub config: Config,
    /// 열려 있는 프로젝트. 없으면 시작 화면이다.
    pub open: Option<Open>,
    /// 상태 줄에 보여 줄 알림 (오류 포함).
    pub notice: Option<String>,
    /// AI 모델이 어디서 도는지.
    pub backend: Backend,
}

/// AI 모델(노이즈 제거)의 실행 환경.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// 아직 확인 중.
    Unknown,
    Gpu,
    /// GPU를 못 써서 CPU로 돈다. 느리다는 안내와 해결 방법.
    Cpu(String),
    /// 모델을 쓸 수 없다 (파일이 없는 경우 등). 이유와 해결 방법.
    Unavailable(String),
}

/// 열려 있는 프로젝트의 작업 상태.
pub struct Open {
    /// 프로젝트 파일의 위치.
    pub path: PathBuf,
    pub project: Project,
    /// 가져온 모든 폴더의 사진.
    pub photos: Vec<Photo>,
    pub selected: BTreeSet<usize>,
    /// 미리보기에 띄운 사진.
    pub current: Option<usize>,
    /// 노이즈 제거 슬라이더의 값. [적용]을 눌러야 사진에 반영된다.
    pub denoise_level: f32,
    pub preview: Option<Preview>,
    pub export: ExportState,
    /// 미리보기 요청 번호. 늦게 도착한 옛 결과를 버리는 데 쓴다.
    generation: u64,
}

pub struct Photo {
    /// `project.folders`에서의 위치.
    pub folder: usize,
    /// 폴더 기준 상대 경로.
    pub relative: String,
    pub thumbnail: Option<Pixels>,
}

pub struct Preview {
    pub index: usize,
    /// 요청 번호. 화면 쪽에서 그림이 바뀌었는지 아는 데 쓴다.
    pub generation: u64,
    pub pixels: Pixels,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportState {
    Idle,
    /// 이미 있는 출력 파일을 어떻게 할지 묻는 중.
    Confirm {
        photos: Vec<export::Photo>,
        existing: usize,
    },
    Running {
        total: usize,
        report: Option<ReportView>,
    },
    Finished {
        outcome: Outcome,
        warnings: export::Warnings,
    },
    Failed(String),
}

/// `Report`에서 화면에 필요한 것 (비교할 수 있게 따로 둔다).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ReportView {
    pub finished: usize,
    pub progress: f64,
    pub running: usize,
    pub remaining: Option<f64>,
    pub idle: f64,
    pub cpu: f32,
    pub memory: u64,
    pub memory_limit: u64,
    pub gpu: f32,
    pub low_memory: bool,
}

/// 사용자가 한 일, 또는 백그라운드 작업이 알려 온 결과.
pub enum Action {
    NewProject(PathBuf),
    OpenProject(PathBuf),
    CloseProject,
    ImportFolder(PathBuf),
    /// `extend`가 참이면 지금 선택에 더한다.
    Select {
        index: usize,
        extend: bool,
    },
    SelectAll,
    SetDenoiseLevel(f32),
    /// 슬라이더의 레벨을 선택한 사진들에 적용한다.
    ApplyDenoise,
    SetExportFolder(PathBuf),
    SetQuality(u8),
    Export {
        only_selected: bool,
    },
    /// 이미 있는 파일을 덮어쓸지(true) 건너뛸지(false).
    ConfirmExport {
        overwrite: bool,
    },
    CancelExport,
    /// 내보내기 결과 표시를 닫는다.
    DismissExport,
    /// 화면 갈무리를 저장한다.
    SaveScreenshot(Pixels),

    Thumbnail {
        index: usize,
        pixels: Option<Pixels>,
    },
    PreviewReady {
        generation: u64,
        index: usize,
        result: std::result::Result<Pixels, String>,
    },
    ExportProgress(Report),
    ExportFinished(std::result::Result<(Outcome, export::Warnings), String>),
    Backend(Backend),
}

/// 화면 밖에서 해야 할 일.
#[derive(Debug, PartialEq)]
pub enum Effect {
    /// (사진 번호, 파일 경로)들의 썸네일을 만든다.
    Thumbnails(Vec<(usize, PathBuf)>),
    Preview {
        generation: u64,
        index: usize,
        path: PathBuf,
        denoise: f32,
    },
    Export {
        photos: Vec<export::Photo>,
        settings: export::Settings,
    },
    CancelExport,
}

impl App {
    /// `config_path`의 설정을 읽어 시작 화면 상태로 만든다.
    pub fn new(config_path: PathBuf) -> Self {
        Self {
            config: Config::load(&config_path),
            config_path,
            open: None,
            notice: None,
            backend: Backend::Unknown,
        }
    }

    pub fn handle(&mut self, action: Action) -> Vec<Effect> {
        match self.try_handle(action) {
            Ok(effects) => effects,
            Err(error) => {
                self.notice = Some(error.to_string());
                Vec::new()
            }
        }
    }

    fn try_handle(&mut self, action: Action) -> crate::Result<Vec<Effect>> {
        // 프로젝트가 없어도 되는 동작들
        match action {
            Action::NewProject(path) => {
                let path = path.with_extension("amsil");
                let name = path
                    .file_stem()
                    .map_or("이름 없음".into(), |n| n.to_string_lossy().into_owned());
                let project = Project::new(&name);
                project.save(&path)?;
                return self.enter(path, project);
            }
            Action::OpenProject(path) => {
                let project = Project::load(&path)?;
                return self.enter(path, project);
            }
            Action::Backend(backend) => {
                self.backend = backend;
                return Ok(Vec::new());
            }
            Action::SaveScreenshot(pixels) => {
                let path = self.save_screenshot(&pixels)?;
                self.notice = Some(format!("스크린샷을 저장했습니다: {}", path.display()));
                return Ok(Vec::new());
            }
            _ => {}
        }

        let Some(open) = &mut self.open else {
            return Ok(Vec::new());
        };
        let mut effects = Vec::new();
        match action {
            Action::NewProject(_)
            | Action::OpenProject(_)
            | Action::SaveScreenshot(_)
            | Action::Backend(_) => {
                unreachable!()
            }
            Action::CloseProject => {
                if matches!(open.export, ExportState::Running { .. }) {
                    return Err(
                        "내보내는 중에는 프로젝트를 닫을 수 없습니다. 먼저 취소하세요.".into(),
                    );
                }
                open.save()?;
                self.open = None;
            }
            Action::ImportFolder(path) => {
                if open
                    .project
                    .folders
                    .iter()
                    .any(|folder| folder.path == path)
                {
                    return Err("이미 가져온 폴더입니다".into());
                }
                let files = crate::job::scan(&path)?;
                if files.is_empty() {
                    return Err(
                        format!("이 폴더에는 RAW 파일이 없습니다: {}", path.display()).into(),
                    );
                }
                let folder = open.project.folders.len();
                open.project.folders.push(Folder {
                    path,
                    photos: Default::default(),
                });
                effects.push(open.add_photos(folder, files));
                open.save()?;
            }
            Action::Select { index, extend } => {
                if index >= open.photos.len() {
                    return Ok(effects);
                }
                if !extend {
                    open.selected.clear();
                }
                open.selected.insert(index);
                open.current = Some(index);
                open.denoise_level = open.edit(index).denoise;
                effects.push(open.request_preview(index));
            }
            Action::SelectAll => open.selected = (0..open.photos.len()).collect(),
            Action::SetDenoiseLevel(level) => open.denoise_level = level.clamp(0.0, 1.0),
            Action::ApplyDenoise => {
                let level = open.denoise_level;
                for index in open.selected.clone() {
                    let (folder, relative) = (
                        open.photos[index].folder,
                        open.photos[index].relative.clone(),
                    );
                    let mut edit = open.edit(index);
                    edit.denoise = level;
                    open.project.set_edit(folder, &relative, edit);
                }
                open.save()?;
                if let Some(current) = open.current {
                    effects.push(open.request_preview(current));
                }
            }
            Action::SetExportFolder(folder) => {
                open.project.export.folder = Some(folder);
                open.save()?;
            }
            Action::SetQuality(quality) => {
                open.project.export.quality = quality;
                open.save()?;
            }
            Action::Export { only_selected } => {
                if !matches!(
                    open.export,
                    ExportState::Idle | ExportState::Finished { .. } | ExportState::Failed(_)
                ) {
                    return Ok(effects);
                }
                let Some(out) = open.project.export.folder.clone() else {
                    return Err("출력 폴더를 먼저 정하세요".into());
                };
                let names = open.project.export_names();
                let photos: Vec<export::Photo> = (0..open.photos.len())
                    .filter(|index| !only_selected || open.selected.contains(index))
                    .map(|index| {
                        let photo = &open.photos[index];
                        export::Photo {
                            input: open.photo_path(index),
                            output: crate::job::output_path(
                                &out.join(&names[photo.folder]),
                                Path::new(&photo.relative),
                            ),
                            denoise: open.edit(index).denoise,
                        }
                    })
                    .collect();
                if photos.is_empty() {
                    return Err("내보낼 사진이 없습니다".into());
                }
                let existing = photos.iter().filter(|photo| photo.output.exists()).count();
                if existing > 0 {
                    open.export = ExportState::Confirm { photos, existing };
                } else {
                    effects.push(open.start_export(photos, false));
                }
            }
            Action::ConfirmExport { overwrite } => {
                if let ExportState::Confirm { photos, .. } =
                    std::mem::replace(&mut open.export, ExportState::Idle)
                {
                    effects.push(open.start_export(photos, overwrite));
                }
            }
            Action::CancelExport => match open.export {
                ExportState::Running { .. } => effects.push(Effect::CancelExport),
                ExportState::Confirm { .. } => open.export = ExportState::Idle,
                _ => {}
            },
            Action::DismissExport => {
                if !matches!(open.export, ExportState::Running { .. }) {
                    open.export = ExportState::Idle;
                }
            }
            Action::Thumbnail { index, pixels } => {
                if let Some(photo) = open.photos.get_mut(index) {
                    photo.thumbnail = pixels;
                }
            }
            Action::PreviewReady {
                generation,
                index,
                result,
            } => {
                // 그 사이에 다른 사진을 골랐다면 버린다.
                if generation == open.generation {
                    open.preview = Some(Preview {
                        index,
                        generation,
                        pixels: result?,
                    });
                }
            }
            Action::ExportProgress(report) => {
                if let ExportState::Running { report: slot, .. } = &mut open.export {
                    *slot = Some(ReportView {
                        finished: report.status.finished,
                        progress: report.status.progress,
                        running: report.status.running,
                        remaining: report.remaining,
                        idle: report.idle,
                        cpu: report.cpu,
                        memory: report.memory,
                        memory_limit: report.memory_limit,
                        gpu: report.gpu,
                        low_memory: report.low_memory,
                    });
                }
            }
            Action::ExportFinished(result) => {
                open.export = match result {
                    Ok((outcome, warnings)) => ExportState::Finished { outcome, warnings },
                    Err(error) => ExportState::Failed(error),
                };
            }
        }
        Ok(effects)
    }

    /// 프로젝트를 열어 작업 화면으로 들어간다. 폴더를 다시 훑어 사진 목록을 만든다.
    fn enter(&mut self, path: PathBuf, project: Project) -> crate::Result<Vec<Effect>> {
        let mut open = Open {
            path: path.clone(),
            project,
            photos: Vec::new(),
            selected: BTreeSet::new(),
            current: None,
            denoise_level: 0.0,
            preview: None,
            export: ExportState::Idle,
            generation: 0,
        };
        let mut wanted = Vec::new();
        let mut missing = Vec::new();
        for folder in 0..open.project.folders.len() {
            // ponytail: 여기서 폴더를 직접 훑는다. 느린 디스크의 큰 폴더라면 화면이 잠깐 멈출 수 있다.
            match crate::job::scan(&open.project.folders[folder].path) {
                Ok(files) => {
                    if let Effect::Thumbnails(list) = open.add_photos(folder, files) {
                        wanted.extend(list);
                    }
                }
                Err(_) => missing.push(open.project.folders[folder].path.display().to_string()),
            }
        }
        self.open = Some(open);
        self.config.remember(&path);
        self.config.save(&self.config_path)?;
        self.notice = (!missing.is_empty())
            .then(|| format!("폴더를 찾을 수 없습니다: {}", missing.join(", ")));
        Ok(if wanted.is_empty() {
            Vec::new()
        } else {
            vec![Effect::Thumbnails(wanted)]
        })
    }

    /// 화면 갈무리를 PNG로 저장한다. 프로젝트가 열려 있으면 그 옆의 `스크린샷` 폴더에, 아니면 설정 폴더에 둔다.
    fn save_screenshot(&self, pixels: &Pixels) -> crate::Result<PathBuf> {
        let base = match &self.open {
            Some(open) => open.path.parent().unwrap_or(Path::new(".")).to_owned(),
            None => self
                .config_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_owned(),
        };
        let dir = base.join("스크린샷");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join(
            chrono::Local::now()
                .format("%Y%m%d-%H%M%S%.3f.png")
                .to_string(),
        );
        image::save_buffer(
            &path,
            &pixels.rgba,
            pixels.width as u32,
            pixels.height as u32,
            image::ColorType::Rgba8,
        )?;
        Ok(path)
    }
}

impl Open {
    /// 사진 파일의 전체 경로.
    pub fn photo_path(&self, index: usize) -> PathBuf {
        let photo = &self.photos[index];
        self.project.folders[photo.folder]
            .path
            .join(&photo.relative)
    }

    /// 가장 최근에 요청한 미리보기의 번호.
    #[cfg(test)]
    pub fn preview_generation(&self) -> u64 {
        self.generation
    }

    /// 사진에 저장된 편집 내용.
    pub fn edit(&self, index: usize) -> Edit {
        let photo = &self.photos[index];
        self.project.edit(photo.folder, &photo.relative)
    }

    fn save(&self) -> crate::Result<()> {
        self.project.save(&self.path)
    }

    /// 폴더의 사진들을 목록 끝에 붙이고, 썸네일을 만들어 달라는 요청을 돌려준다.
    fn add_photos(&mut self, folder: usize, files: Vec<PathBuf>) -> Effect {
        let first = self.photos.len();
        self.photos.extend(files.iter().map(|file| Photo {
            folder,
            relative: file.to_string_lossy().into_owned(),
            thumbnail: None,
        }));
        Effect::Thumbnails(
            (first..self.photos.len())
                .map(|index| (index, self.photo_path(index)))
                .collect(),
        )
    }

    fn request_preview(&mut self, index: usize) -> Effect {
        self.generation += 1;
        Effect::Preview {
            generation: self.generation,
            index,
            path: self.photo_path(index),
            denoise: self.edit(index).denoise,
        }
    }

    fn start_export(&mut self, photos: Vec<export::Photo>, overwrite: bool) -> Effect {
        self.export = ExportState::Running {
            total: photos.len(),
            report: None,
        };
        let settings = export::Settings {
            quality: self.project.export.quality,
            overwrite,
            ..export::Settings::default()
        };
        Effect::Export { photos, settings }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 임시 폴더: 설정 파일, 프로젝트 파일, 가짜 RAW 파일이 든 사진 폴더.
    struct Fixture {
        dir: PathBuf,
        app: App,
    }

    impl Fixture {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("amsil-app-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for file in [
                "사진/a.ARW",
                "사진/sub/b.ARW",
                "사진/c.ARW",
                "사진/메모.txt",
            ] {
                let path = dir.join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, b"").unwrap();
            }
            let app = App::new(dir.join("config/config.toml"));
            Self { dir, app }
        }

        /// 프로젝트를 만들고 사진 폴더를 가져온 상태.
        fn with_photos(name: &str) -> Self {
            let mut fx = Self::new(name);
            fx.app.handle(Action::NewProject(fx.dir.join("여행.amsil")));
            fx.app.handle(Action::ImportFolder(fx.dir.join("사진")));
            fx
        }

        fn open(&self) -> &Open {
            self.app.open.as_ref().unwrap()
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn pixels() -> Pixels {
        Pixels {
            width: 1,
            height: 1,
            rgba: vec![1, 2, 3, 255],
        }
    }

    #[test]
    fn starts_on_the_start_screen() {
        let fx = Fixture::new("start");
        assert!(fx.app.open.is_none());
        assert!(fx.app.config.recent.is_empty());
    }

    #[test]
    fn new_project_is_named_after_its_file_saved_and_remembered() {
        let mut fx = Fixture::new("new");
        let path = fx.dir.join("가을 여행.amsil");
        let effects = fx.app.handle(Action::NewProject(path.clone()));
        assert!(effects.is_empty());
        assert_eq!(fx.open().project.name, "가을 여행");
        assert_eq!(
            Project::load(&path).unwrap().name,
            "가을 여행",
            "만들자마자 파일로 저장된다"
        );
        assert_eq!(fx.app.config.recent, std::slice::from_ref(&path));
        assert_eq!(
            Config::load(&fx.dir.join("config/config.toml")).recent,
            [path]
        );
    }

    #[test]
    fn a_project_file_gets_the_amsil_extension() {
        let mut fx = Fixture::new("ext");
        fx.app.handle(Action::NewProject(fx.dir.join("여행")));
        assert_eq!(fx.open().path, fx.dir.join("여행.amsil"));
    }

    #[test]
    fn importing_a_folder_lists_its_raw_files_and_asks_for_thumbnails() {
        let mut fx = Fixture::new("import");
        fx.app.handle(Action::NewProject(fx.dir.join("p.amsil")));
        let effects = fx.app.handle(Action::ImportFolder(fx.dir.join("사진")));

        let names: Vec<&str> = fx
            .open()
            .photos
            .iter()
            .map(|p| p.relative.as_str())
            .collect();
        assert_eq!(names, ["a.ARW", "c.ARW", "sub/b.ARW"]);
        let photos = fx.dir.join("사진");
        assert_eq!(
            effects,
            [Effect::Thumbnails(vec![
                (0, photos.join("a.ARW")),
                (1, photos.join("c.ARW")),
                (2, photos.join("sub/b.ARW"))
            ])]
        );
        assert_eq!(
            Project::load(&fx.open().path).unwrap().folders[0].path,
            photos,
            "가져온 폴더가 저장된다"
        );
    }

    #[test]
    fn importing_the_same_folder_twice_is_refused() {
        let mut fx = Fixture::with_photos("twice");
        let effects = fx.app.handle(Action::ImportFolder(fx.dir.join("사진")));
        assert!(effects.is_empty());
        assert_eq!(fx.open().photos.len(), 3);
        assert!(fx.app.notice.as_ref().unwrap().contains("이미"));
    }

    #[test]
    fn a_folder_without_raw_files_is_not_added() {
        let mut fx = Fixture::new("empty");
        fx.app.handle(Action::NewProject(fx.dir.join("p.amsil")));
        std::fs::create_dir_all(fx.dir.join("빈 폴더")).unwrap();
        fx.app.handle(Action::ImportFolder(fx.dir.join("빈 폴더")));
        assert!(fx.open().project.folders.is_empty());
        assert!(fx.app.notice.as_ref().unwrap().contains("RAW"));
    }

    #[test]
    fn reopening_a_project_brings_back_folders_photos_and_edits() {
        let mut fx = Fixture::with_photos("reopen");
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        fx.app.handle(Action::SetDenoiseLevel(0.6));
        fx.app.handle(Action::ApplyDenoise);
        let path = fx.open().path.clone();
        fx.app.handle(Action::CloseProject);
        assert!(fx.app.open.is_none());

        let effects = fx.app.handle(Action::OpenProject(path));
        assert_eq!(fx.open().photos.len(), 3);
        assert_eq!(fx.open().edit(1).denoise, 0.6);
        assert_eq!(fx.open().edit(0).denoise, 0.0);
        assert!(matches!(&effects[..], [Effect::Thumbnails(list)] if list.len() == 3));
    }

    #[test]
    fn opening_a_bad_project_stays_on_the_start_screen_with_a_notice() {
        let mut fx = Fixture::new("bad");
        let path = fx.dir.join("깨진.amsil");
        std::fs::write(&path, "version = 99\nname = \"x\"\n").unwrap();
        fx.app.handle(Action::OpenProject(path));
        assert!(fx.app.open.is_none());
        assert!(fx.app.notice.as_ref().unwrap().contains("버전"));
    }

    #[test]
    fn selecting_a_photo_shows_it_and_asks_for_a_preview_with_its_denoise_level() {
        let mut fx = Fixture::with_photos("select");
        let effects = fx.app.handle(Action::Select {
            index: 2,
            extend: false,
        });
        assert_eq!(fx.open().current, Some(2));
        assert_eq!(fx.open().selected.iter().copied().collect::<Vec<_>>(), [2]);
        let [
            Effect::Preview {
                index: 2,
                path,
                denoise,
                ..
            },
        ] = &effects[..]
        else {
            panic!("{effects:?}")
        };
        assert_eq!(path, &fx.dir.join("사진/sub/b.ARW"));
        assert_eq!(*denoise, 0.0);

        fx.app.handle(Action::Select {
            index: 0,
            extend: true,
        });
        assert_eq!(
            fx.open().selected.iter().copied().collect::<Vec<_>>(),
            [0, 2]
        );
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        assert_eq!(
            fx.open().selected.iter().copied().collect::<Vec<_>>(),
            [1],
            "그냥 누르면 그 사진만 선택된다"
        );
        fx.app.handle(Action::SelectAll);
        assert_eq!(fx.open().selected.len(), 3);
    }

    #[test]
    fn only_the_latest_preview_request_is_shown() {
        let mut fx = Fixture::with_photos("stale");
        let first = fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        let second = fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        let generation = |effects: &[Effect]| match effects {
            [Effect::Preview { generation, .. }] => *generation,
            other => panic!("{other:?}"),
        };
        // 먼저 요청한 사진의 결과가 늦게 도착해도 버린다.
        fx.app.handle(Action::PreviewReady {
            generation: generation(&first),
            index: 0,
            result: Ok(pixels()),
        });
        assert!(fx.open().preview.is_none());
        fx.app.handle(Action::PreviewReady {
            generation: generation(&second),
            index: 1,
            result: Ok(pixels()),
        });
        assert_eq!(fx.open().preview.as_ref().unwrap().index, 1);
    }

    #[test]
    fn applying_denoise_stores_the_level_on_selected_photos_and_refreshes_the_preview() {
        let mut fx = Fixture::with_photos("denoise");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::Select {
            index: 2,
            extend: true,
        });
        fx.app.handle(Action::SetDenoiseLevel(0.8));
        assert_eq!(
            fx.open().edit(0).denoise,
            0.0,
            "슬라이더만 움직여서는 사진이 바뀌지 않는다"
        );

        let effects = fx.app.handle(Action::ApplyDenoise);
        assert_eq!(
            [
                fx.open().edit(0).denoise,
                fx.open().edit(1).denoise,
                fx.open().edit(2).denoise
            ],
            [0.8, 0.0, 0.8]
        );
        assert!(
            matches!(&effects[..], [Effect::Preview { index: 2, denoise, .. }] if *denoise == 0.8),
            "{effects:?}"
        );
        let saved = Project::load(&fx.open().path).unwrap();
        assert_eq!(saved.edit(0, "a.ARW").denoise, 0.8);
        assert_eq!(saved.edit(0, "sub/b.ARW").denoise, 0.8);
    }

    #[test]
    fn selecting_a_photo_moves_the_slider_to_its_stored_level() {
        let mut fx = Fixture::with_photos("slider");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::SetDenoiseLevel(0.4));
        fx.app.handle(Action::ApplyDenoise);
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        assert_eq!(fx.open().denoise_level, 0.0);
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        assert_eq!(fx.open().denoise_level, 0.4);
    }

    #[test]
    fn backend_status_is_kept_even_without_a_project() {
        let mut fx = Fixture::new("backend");
        assert_eq!(fx.app.backend, Backend::Unknown);
        fx.app
            .handle(Action::Backend(Backend::Cpu("느립니다".into())));
        assert_eq!(fx.app.backend, Backend::Cpu("느립니다".into()));
    }

    #[test]
    fn thumbnails_arrive_one_by_one() {
        let mut fx = Fixture::with_photos("thumbs");
        fx.app.handle(Action::Thumbnail {
            index: 1,
            pixels: Some(pixels()),
        });
        assert!(fx.open().photos[1].thumbnail.is_some());
        assert!(fx.open().photos[0].thumbnail.is_none());
        fx.app.handle(Action::Thumbnail {
            index: 99,
            pixels: Some(pixels()),
        }); // 범위 밖은 무시
    }

    #[test]
    fn export_needs_a_folder_first() {
        let mut fx = Fixture::with_photos("nofolder");
        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        assert!(effects.is_empty());
        assert_eq!(fx.open().export, ExportState::Idle);
        assert!(fx.app.notice.as_ref().unwrap().contains("출력 폴더"));
    }

    #[test]
    fn export_mirrors_the_folder_structure_and_uses_each_photos_denoise_level() {
        let mut fx = Fixture::with_photos("export");
        fx.app.handle(Action::Select {
            index: 2,
            extend: false,
        });
        fx.app.handle(Action::SetDenoiseLevel(0.5));
        fx.app.handle(Action::ApplyDenoise);
        let out = fx.dir.join("결과");
        fx.app.handle(Action::SetExportFolder(out.clone()));
        fx.app.handle(Action::SetQuality(60));

        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        let [Effect::Export { photos, settings }] = &effects[..] else {
            panic!("{effects:?}")
        };
        let src = fx.dir.join("사진");
        assert_eq!(
            photos,
            &[
                export::Photo {
                    input: src.join("a.ARW"),
                    output: out.join("사진/a.heic"),
                    denoise: 0.0
                },
                export::Photo {
                    input: src.join("c.ARW"),
                    output: out.join("사진/c.heic"),
                    denoise: 0.0
                },
                export::Photo {
                    input: src.join("sub/b.ARW"),
                    output: out.join("사진/sub/b.heic"),
                    denoise: 0.5
                },
            ]
        );
        assert_eq!((settings.quality, settings.overwrite), (60, false));
        assert_eq!(
            fx.open().export,
            ExportState::Running {
                total: 3,
                report: None
            }
        );
        assert_eq!(
            Project::load(&fx.open().path).unwrap().export.folder,
            Some(out)
        );
    }

    #[test]
    fn exporting_only_the_selection() {
        let mut fx = Fixture::with_photos("selected");
        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        let effects = fx.app.handle(Action::Export {
            only_selected: true,
        });
        let [Effect::Export { photos, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(photos.len(), 1);
        assert!(photos[0].input.ends_with("c.ARW"));
    }

    #[test]
    fn existing_outputs_are_asked_about_before_exporting() {
        let mut fx = Fixture::with_photos("confirm");
        let out = fx.dir.join("결과");
        std::fs::create_dir_all(out.join("사진")).unwrap();
        std::fs::write(out.join("사진/a.heic"), b"old").unwrap();
        fx.app.handle(Action::SetExportFolder(out));

        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        assert!(effects.is_empty(), "물어보기 전에는 시작하지 않는다");
        assert!(
            matches!(&fx.open().export, ExportState::Confirm { existing: 1, photos } if photos.len() == 3)
        );

        let effects = fx.app.handle(Action::ConfirmExport { overwrite: true });
        let [Effect::Export { settings, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert!(settings.overwrite);
        assert!(matches!(
            fx.open().export,
            ExportState::Running { total: 3, .. }
        ));
    }

    #[test]
    fn export_reports_progress_then_the_result_and_can_be_cancelled_and_dismissed() {
        let mut fx = Fixture::with_photos("progress");
        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        fx.app.handle(Action::Export {
            only_selected: false,
        });
        assert!(
            fx.app
                .handle(Action::Export {
                    only_selected: false
                })
                .is_empty(),
            "도는 중에는 또 시작하지 않는다"
        );

        let report = Report {
            status: crate::job::Status {
                total: 3,
                finished: 1,
                progress: 1.5,
                running: 2,
            },
            remaining: Some(12.0),
            idle: 0.2,
            cpu: 90.0,
            memory: 1 << 30,
            memory_limit: 8 << 30,
            gpu: 0.0,
            low_memory: false,
        };
        fx.app.handle(Action::ExportProgress(report));
        let ExportState::Running {
            report: Some(view), ..
        } = &fx.open().export
        else {
            panic!()
        };
        assert_eq!(
            (view.finished, view.progress, view.remaining),
            (1, 1.5, Some(12.0))
        );

        assert_eq!(fx.app.handle(Action::CancelExport), [Effect::CancelExport]);

        let outcome = Outcome {
            done: 2,
            skipped: 0,
            failed: vec![("c.ARW".into(), "읽을 수 없음".into())],
            stopped: None,
        };
        fx.app
            .handle(Action::ExportFinished(Ok((outcome.clone(), vec![]))));
        assert_eq!(
            fx.open().export,
            ExportState::Finished {
                outcome,
                warnings: vec![]
            }
        );
        fx.app.handle(Action::DismissExport);
        assert_eq!(fx.open().export, ExportState::Idle);

        fx.app.handle(Action::Export {
            only_selected: false,
        });
        fx.app
            .handle(Action::ExportFinished(Err("모델이 없습니다".into())));
        assert_eq!(
            fx.open().export,
            ExportState::Failed("모델이 없습니다".into())
        );
    }

    #[test]
    fn a_project_cannot_be_closed_while_exporting() {
        let mut fx = Fixture::with_photos("busy");
        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        fx.app.handle(Action::Export {
            only_selected: false,
        });
        fx.app.handle(Action::CloseProject);
        assert!(fx.app.open.is_some());
        assert!(fx.app.notice.as_ref().unwrap().contains("내보내는 중"));
    }

    #[test]
    fn screenshot_is_saved_as_png_next_to_the_project() {
        let mut fx = Fixture::with_photos("shot");
        fx.app.handle(Action::SaveScreenshot(Pixels {
            width: 2,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255],
        }));
        let files: Vec<PathBuf> = std::fs::read_dir(fx.dir.join("스크린샷"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].extension().unwrap(), "png");
        let image = image::open(&files[0]).unwrap().into_rgba8();
        assert_eq!((image.width(), image.height()), (2, 1));
        assert_eq!(image.into_raw(), [255, 0, 0, 255, 0, 0, 255, 255]);
        assert!(fx.app.notice.as_ref().unwrap().contains("스크린샷"));
    }
}
