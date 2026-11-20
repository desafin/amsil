//! 화면과 무관한 프로그램 상태.
//!
//! 사용자가 한 일과 백그라운드 작업의 결과는 모두 `Action`으로 들어온다. `App::handle`이 상태를 바꾸고,
//! 화면 밖에서 해야 할 일(썸네일 만들기, 내보내기 등)을 `Effect`로 돌려준다. 화면 코드는 상태를 읽어
//! 그리기만 한다.

use crate::capture::{self, Capture, CaptureView};
use crate::edit::Params;
use crate::export::{self, Report};
use crate::frame::Frame;
use crate::job::Outcome;
use crate::lang::{self, Lang, Text, t, text};
use crate::log;
use crate::mask::{Mask, Point};
use crate::preset::{self, Preset};
use crate::preview::Pixels;
use crate::project::{Before, Config, Edit, Folder, LutRef, Project, Split, Theme};
use crate::recommend::{self, Guess};
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
    /// 고를 수 있는 프리셋: 기본 프리셋 뒤에 사용자가 만든 것.
    pub presets: Vec<Preset>,
    /// 프리셋 추천(사진 분류)을 쓸 수 없는 이유. 쓸 수 있으면 None.
    pub recommend_problem: Option<Text>,
    /// 마스크를 쓸 수 없는 이유. 쓸 수 있으면 None.
    pub mask_problem: Option<Text>,
    /// 피부 보정을 쓸 수 없는 이유. 쓸 수 있으면 None.
    pub skin_problem: Option<Text>,
}

/// AI 모델(노이즈 제거)의 실행 환경.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// 아직 확인 중.
    Unknown,
    Gpu,
    /// GPU를 못 써서 CPU로 돈다. 느리다는 안내와 해결 방법.
    Cpu(Text),
    /// 모델을 쓸 수 없다 (파일이 없는 경우 등). 이유와 해결 방법.
    Unavailable(Text),
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
    /// 지금 보는 사진에서 고쳐 쓰는 중인 마스크의 번호. 미리보기를 누르면 이 마스크에 자리가 더해진다.
    pub mask: Option<usize>,
    /// 고쳐 쓰는 마스크가 고른 영역을 미리보기에 붉게 표시할지.
    pub show_mask: bool,
    /// 전후 비교를 켰는가. 켜면 미리보기를 보정 전과 보정 후 두 칸으로 나눈다.
    pub compare: bool,
    /// 비교할 때 보여 줄 보정 전 그림 (한 장만 둔다).
    pub original: Option<Original>,
    /// 마지막으로 요청한 보정 전 그림이 카메라 JPEG인가.
    original_jpeg: bool,
    /// (사진 번호, 그 사진의 카메라 JPEG). 보정 전 그림을 요청할 때 지금 사진에 대해 찾아 둔다.
    pub sidecar: Option<(usize, Option<PathBuf>)>,
    /// 사진에서 구한 캡처 샤픈의 (사진 번호, 반경, 대비 문턱). 가장 최근에 읽은 사진의 것만 둔다.
    pub capture_auto: Option<(usize, f32, f32)>,
    /// 샤픈 영역 보기 (저장하지 않는다).
    pub capture_view: Option<CaptureView>,
    pub export: ExportState,
    /// 미리보기 요청 번호. 늦게 도착한 옛 결과를 버리는 데 쓴다.
    generation: u64,
    preview_serial: u64,
    /// 보정 전 그림의 요청 번호. 늦게 도착한 옛 결과를 버리는 데 쓴다.
    original_generation: u64,
}

pub struct Photo {
    /// `project.folders`에서의 위치.
    pub folder: usize,
    /// 폴더 기준 상대 경로.
    pub relative: String,
    pub thumbnail: Option<Pixels>,
    /// 사진이 무엇을 찍은 것인지에 대한 짐작. 아직 분류하지 않았으면 None.
    pub guess: Option<Guess>,
}

pub struct Preview {
    pub index: usize,
    /// 그림이 바뀔 때마다 달라지는 번호. 화면 쪽에서 텍스처를 다시 올릴지 아는 데 쓴다.
    pub serial: u64,
    pub pixels: Pixels,
}

/// 같은 폴더에 같은 이름으로 있는 카메라 JPEG (`.jpg`, `.jpeg`, 대소문자 무관). 없으면 None.
pub fn sidecar_jpeg(raw: &Path) -> Option<PathBuf> {
    let (dir, stem) = (raw.parent()?, raw.file_stem()?);
    let mut found: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_stem() == Some(stem)
                && path.extension().is_some_and(|ext| {
                    let ext = ext.to_string_lossy().to_lowercase();
                    ext == "jpg" || ext == "jpeg"
                })
        })
        .collect();
    found.sort();
    found.into_iter().next()
}

/// 아무 보정도 하지 않은 사진 (전후 비교의 보정 전 칸). 카메라 JPEG일 수도 있다.
pub struct Original {
    pub index: usize,
    /// 카메라 JPEG를 읽은 것인가.
    pub jpeg: bool,
    /// 그림이 바뀔 때마다 달라지는 번호. 화면 쪽에서 텍스처를 다시 올릴지 아는 데 쓴다.
    pub serial: u64,
    /// 만들지 못했으면 그 이유.
    pub pixels: std::result::Result<Pixels, Text>,
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
    /// 선택한 사진들의 프리셋을 정한다 (None이면 없앤다).
    SetPreset(Option<String>),
    /// 선택한 사진마다 그 내용에 맞는 프리셋을 적용한다. 맞는 것이 없으면 프리셋을 없앤다.
    ApplyRecommended,
    /// 프리셋의 강도. 슬라이더를 끄는 동안에는 `commit`이 거짓이고, 놓을 때 참으로 한 번 온다 (그때 저장한다).
    SetStrength {
        value: f32,
        commit: bool,
    },
    /// 수동 조정.
    SetAdjust {
        params: Params,
        commit: bool,
    },
    /// 선택한 사진들에 입힐 LUT 파일을 정한다 (None이면 없앤다).
    SetLut(Option<PathBuf>),
    /// LUT의 강도.
    SetLutStrength {
        value: f32,
        commit: bool,
    },
    /// 선택한 사진들에 두를 프레임을 정한다 (None이면 없앤다).
    SetFrame(Option<Frame>),
    /// 지금 보는 사진에 새 마스크를 만들고 고쳐 쓰기 시작한다.
    AddMask,
    /// 고쳐 쓸 마스크를 고른다 (None이면 고쳐 쓰기를 마친다).
    SelectMask(Option<usize>),
    /// 고쳐 쓰는 마스크를 지운다.
    RemoveMask,
    /// 고쳐 쓰는 마스크에 누른 자리를 더한다. 좌표는 사진의 왼쪽 위가 (0, 0), 오른쪽 아래가 (1, 1)이다.
    AddMaskPoint {
        x: f32,
        y: f32,
        include: bool,
    },
    /// 마지막으로 누른 자리를 없앤다.
    UndoMaskPoint,
    SetMaskInvert(bool),
    /// 고쳐 쓰는 마스크의 영역에만 더할 보정.
    SetMaskAdjust {
        params: Params,
        commit: bool,
    },
    /// 고른 영역을 미리보기에 표시할지.
    ShowMask(bool),
    /// 전후 비교를 켜거나 끈다.
    SetCompare(bool),
    /// 선택한 사진들의 피부 보정 세기 (0~1).
    SetSkin {
        value: f32,
        commit: bool,
    },
    /// 선택한 사진들의 캡처 샤픈 값. 범위를 벗어난 값은 범위 안으로 들인다.
    SetCapture {
        value: Capture,
        commit: bool,
    },
    /// 샤픈 영역 보기를 바꾼다 (None이면 끈다).
    SetCaptureView(Option<CaptureView>),
    /// 전후 비교의 보정 전 칸에 보여 줄 것 (설정 파일에 저장한다).
    SetCompareBefore(Before),
    /// 워커가 사진을 읽으며 구한 캡처 샤픈의 반경과 대비 문턱 (값을 정하지 않았을 때 쓰는 것).
    CaptureAuto {
        index: usize,
        radius: f32,
        threshold: f32,
    },
    /// 프리셋, 수동 조정, LUT, 피부 보정을 지운다 (노이즈 제거, 프레임, 마스크는 그대로).
    ResetLook,
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
    /// 화면과 메시지의 언어를 바꾼다.
    SetLanguage(Lang),
    /// 화면 쪽에서 알릴 것이 생겼다 (상태 줄에 보여 준다).
    Notify(String),
    /// 화면을 밝게 또는 어둡게.
    SetTheme(Theme),
    /// 전후 비교에서 미리보기를 나누는 방향을 정한다 (설정에 저장한다).
    SetCompareSplit(Split),

    Thumbnail {
        index: usize,
        pixels: Option<Pixels>,
        /// 사진의 종류에 대한 짐작 (분류 모델이 있을 때).
        guess: Option<Guess>,
    },
    PreviewReady {
        generation: u64,
        index: usize,
        result: std::result::Result<Pixels, Text>,
    },
    OriginalReady {
        generation: u64,
        index: usize,
        result: std::result::Result<Pixels, Text>,
    },
    ExportProgress(Report),
    ExportFinished(std::result::Result<(Outcome, export::Warnings), Text>),
    Backend(Backend),
    /// 프리셋 추천을 쓸 수 있는지. 쓸 수 없으면 그 이유.
    Recommender(Option<Text>),
    /// 마스크를 쓸 수 있는지. 쓸 수 없으면 그 이유.
    Segmenter(Option<Text>),
    /// 피부 보정을 쓸 수 있는지. 쓸 수 없으면 그 이유.
    SkinModel(Option<Text>),
}

/// 화면 밖에서 해야 할 일.
#[derive(Debug, PartialEq)]
// 효과는 동작 하나에 몇 개만 만든다. 변형마다 크기가 달라도 상관없다.
#[allow(clippy::large_enum_variant)]
pub enum Effect {
    /// (사진 번호, 파일 경로)들의 썸네일을 만든다.
    Thumbnails(Vec<(usize, PathBuf)>),
    Preview {
        generation: u64,
        index: usize,
        path: PathBuf,
        denoise: f32,
        /// 보정 값 (프리셋 × 강도 + 수동 조정).
        params: Params,
        /// 마지막에 입힐 LUT 파일과 강도.
        lut: Option<(PathBuf, f32)>,
        /// 사진에 두를 프레임.
        frame: Option<Frame>,
        /// 부분 보정 마스크.
        masks: Vec<Mask>,
        /// 이 번호의 마스크가 고른 영역을 붉게 표시한다.
        overlay: Option<usize>,
        /// 피부 보정의 세기.
        skin: f32,
        /// 캡처 샤픈 (RAW 단계).
        capture: Capture,
        /// 캡처 샤픈이 걸리는 곳을 보여 줄지.
        capture_view: Option<CaptureView>,
    },
    /// 아무 보정도 하지 않은 사진을 만든다 (전후 비교의 보정 전 칸). `jpeg`가 있으면 그 카메라 JPEG를 읽는다.
    /// 결과는 `Action::OriginalReady`로 온다.
    Original {
        generation: u64,
        index: usize,
        path: PathBuf,
        jpeg: Option<PathBuf>,
    },
    Export {
        photos: Vec<export::Photo>,
        settings: export::Settings,
    },
    CancelExport,
    /// AI를 GPU로 돌릴 수 있는지 (다시) 알아본다. 결과는 `Action::Backend`로 온다.
    ProbeBackend,
}

impl App {
    /// `config_path`의 설정을 읽어 시작 화면 상태로 만든다.
    pub fn new(config_path: PathBuf) -> Self {
        // 사용자 프리셋은 설정 파일 옆의 presets 폴더에 둔다.
        let (user, problems) = preset::load_dir(&config_path.with_file_name("presets"));
        let mut presets = preset::builtin();
        presets.extend(user);
        Self {
            config: Config::load(&config_path),
            config_path,
            open: None,
            notice: (!problems.is_empty()).then(|| problems.join("\n")),
            backend: Backend::Unknown,
            presets,
            recommend_problem: None,
            mask_problem: None,
            skin_problem: None,
        }
    }

    pub fn handle(&mut self, action: Action) -> Vec<Effect> {
        match self.try_handle(action) {
            Ok(effects) => effects,
            Err(error) => {
                // 화면에 띄운 오류는 로그에도 남긴다 (알림은 다음 알림에 덮여 사라진다).
                log::error(&error);
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
                    .map_or(t("이름 없음", "Untitled").into(), |n| {
                        n.to_string_lossy().into_owned()
                    });
                let project = Project::new(&name);
                project.save(&path)?;
                return self.enter(path, project);
            }
            Action::OpenProject(path) => {
                let project = Project::load(&path)?;
                return self.enter(path, project);
            }
            Action::Backend(backend) => {
                match &backend {
                    Backend::Unknown => {}
                    Backend::Gpu => log::info("AI backend: GPU"),
                    Backend::Cpu(notice) | Backend::Unavailable(notice) => log::warn(notice),
                }
                self.backend = backend;
                return Ok(Vec::new());
            }
            Action::Recommender(problem) => {
                problem.iter().for_each(log::warn);
                self.recommend_problem = problem;
                return Ok(Vec::new());
            }
            Action::Segmenter(problem) => {
                problem.iter().for_each(log::warn);
                self.mask_problem = problem;
                return Ok(Vec::new());
            }
            Action::SkinModel(problem) => {
                problem.iter().for_each(log::warn);
                self.skin_problem = problem;
                return Ok(Vec::new());
            }
            Action::SaveScreenshot(pixels) => {
                let path = self.save_screenshot(&pixels)?;
                let path = path.display();
                self.notice = Some(t(
                    format!("스크린샷을 저장했습니다: {path}"),
                    format!("Screenshot saved: {path}"),
                ));
                return Ok(Vec::new());
            }
            Action::Notify(text) => {
                self.notice = Some(text);
                return Ok(Vec::new());
            }
            Action::SetTheme(theme) => {
                self.config.theme = Some(theme);
                self.config.save(&self.config_path)?;
                return Ok(Vec::new());
            }
            Action::SetCompareSplit(split) => {
                self.config.compare_split = split;
                self.config.save(&self.config_path)?;
                return Ok(Vec::new());
            }
            Action::SetLanguage(chosen) => {
                lang::set(chosen);
                self.config.lang = Some(chosen);
                self.config.save(&self.config_path)?;
                // 앞서 받아 둔 AI 안내 문구는 옛 언어로 되어 있다. 새 언어로 다시 받는다.
                return Ok(vec![Effect::ProbeBackend]);
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
            | Action::SetLanguage(_)
            | Action::Notify(_)
            | Action::SetTheme(_)
            | Action::SetCompareSplit(_)
            | Action::Recommender(_)
            | Action::Segmenter(_)
            | Action::SkinModel(_)
            | Action::Backend(_) => {
                unreachable!()
            }
            Action::CloseProject => {
                if matches!(open.export, ExportState::Running { .. }) {
                    return Err(text!(
                        "내보내는 중에는 프로젝트를 닫을 수 없습니다. 먼저 취소하세요.",
                        "The project cannot be closed while exporting. Cancel the export first.",
                    )
                    .into());
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
                    return Err(text!(
                        "이미 가져온 폴더입니다",
                        "This folder has already been imported",
                    )
                    .into());
                }
                let files = crate::job::scan(&path)?;
                if files.is_empty() {
                    let path = path.display();
                    return Err(text!(
                        format!("이 폴더에는 RAW 파일이 없습니다: {path}"),
                        format!("No RAW files in this folder: {path}"),
                    )
                    .into());
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
                // 마스크는 사진마다 따로다. 다른 사진으로 가면 고쳐 쓰기를 마친다.
                open.mask = None;
                open.denoise_level = open.edit(index).denoise;
                effects.push(open.request_preview(index, &self.presets));
                effects.extend(open.request_original(self.config.compare_before));
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
                    effects.push(open.request_preview(current, &self.presets));
                }
            }
            Action::SetPreset(preset) => {
                open.change_look(true, &self.presets, &mut effects, |edit| {
                    edit.preset = preset.clone()
                })?;
            }
            Action::ApplyRecommended => {
                for index in open.selected.clone() {
                    // 아직 분류하지 않은 사진은 건드리지 않는다.
                    let Some(guess) = open.photos[index].guess else {
                        continue;
                    };
                    let (folder, relative) = (
                        open.photos[index].folder,
                        open.photos[index].relative.clone(),
                    );
                    let mut edit = open.edit(index);
                    edit.preset = recommend::preset_for(&self.presets, &guess)
                        .map(|preset| preset.id.clone());
                    open.project.set_edit(folder, &relative, edit);
                }
                open.save()?;
                if let Some(current) = open.current {
                    effects.push(open.request_preview(current, &self.presets));
                }
            }
            Action::SetStrength { value, commit } => {
                open.change_look(commit, &self.presets, &mut effects, |edit| {
                    edit.strength = value
                })?;
            }
            Action::SetAdjust { params, commit } => {
                open.change_look(commit, &self.presets, &mut effects, |edit| {
                    edit.adjust = params
                })?;
            }
            Action::SetLut(file) => {
                open.change_look(true, &self.presets, &mut effects, |edit| {
                    edit.lut = file.clone().map(|file| LutRef {
                        file,
                        strength: 1.0,
                    })
                })?;
            }
            Action::SetLutStrength { value, commit } => {
                open.change_look(commit, &self.presets, &mut effects, |edit| {
                    if let Some(lut) = &mut edit.lut {
                        lut.strength = value.clamp(0.0, 1.0);
                    }
                })?;
            }
            Action::SetFrame(frame) => {
                open.change_look(true, &self.presets, &mut effects, |edit| edit.frame = frame)?;
            }
            Action::AddMask => {
                if let Some(current) = open.current {
                    open.mask = Some(open.edit(current).masks.len());
                    open.show_mask = true;
                    open.change_masks(true, &self.presets, &mut effects, |masks| {
                        masks.push(Mask::default())
                    })?;
                }
            }
            Action::SelectMask(choice) => {
                if let Some(current) = open.current {
                    open.mask = choice.filter(|&index| index < open.edit(current).masks.len());
                    effects.push(open.request_preview(current, &self.presets));
                }
            }
            Action::RemoveMask => {
                if let Some(active) = open.mask.take() {
                    open.change_masks(true, &self.presets, &mut effects, |masks| {
                        if active < masks.len() {
                            masks.remove(active);
                        }
                    })?;
                }
            }
            Action::AddMaskPoint { x, y, include } => {
                open.change_mask(true, &self.presets, &mut effects, |mask| {
                    mask.points.push(Point {
                        x: x.clamp(0.0, 1.0),
                        y: y.clamp(0.0, 1.0),
                        include,
                    })
                })?;
            }
            Action::UndoMaskPoint => {
                open.change_mask(true, &self.presets, &mut effects, |mask| {
                    mask.points.pop();
                })?;
            }
            Action::SetMaskInvert(invert) => {
                open.change_mask(true, &self.presets, &mut effects, |mask| {
                    mask.invert = invert
                })?;
            }
            Action::SetMaskAdjust { params, commit } => {
                open.change_mask(commit, &self.presets, &mut effects, |mask| {
                    mask.adjust = params
                })?;
            }
            Action::ShowMask(show) => {
                open.show_mask = show;
                if let Some(current) = open.current {
                    effects.push(open.request_preview(current, &self.presets));
                }
            }
            Action::SetCompare(on) => {
                open.compare = on;
                // 비교하는 동안에는 프레임을 빼므로 보정 후 그림을 다시 받는다.
                if let Some(current) = open.current {
                    effects.push(open.request_preview(current, &self.presets));
                }
                effects.extend(open.request_original(self.config.compare_before));
            }
            Action::SetSkin { value, commit } => {
                open.change_look(commit, &self.presets, &mut effects, |edit| {
                    edit.skin = value.clamp(0.0, 1.0)
                })?;
            }
            Action::SetCapture { value, commit } => {
                let value = Capture {
                    iterations: value
                        .iterations
                        .clamp(*capture::ITERATIONS.start(), *capture::ITERATIONS.end()),
                    radius: value
                        .radius
                        .map(|r| r.clamp(*capture::RADIUS.start(), *capture::RADIUS.end())),
                    threshold: value
                        .threshold
                        .map(|t| t.clamp(*capture::THRESHOLD.start(), *capture::THRESHOLD.end())),
                    corner: value
                        .corner
                        .clamp(*capture::CORNER.start(), *capture::CORNER.end()),
                    ..value
                };
                if !value.enabled {
                    open.capture_view = None;
                }
                open.change_look(commit, &self.presets, &mut effects, |edit| {
                    edit.capture = value
                })?;
            }
            Action::SetCompareBefore(before) => {
                self.config.compare_before = before;
                self.config.save(&self.config_path)?;
                effects.extend(open.request_original(before));
            }
            Action::SetCaptureView(view) => {
                open.capture_view = view;
                if let Some(current) = open.current {
                    effects.push(open.request_preview(current, &self.presets));
                }
            }
            Action::CaptureAuto {
                index,
                radius,
                threshold,
            } => open.capture_auto = Some((index, radius, threshold)),
            Action::ResetLook => {
                open.change_look(true, &self.presets, &mut effects, |edit| {
                    *edit = Edit {
                        denoise: edit.denoise,
                        frame: edit.frame,
                        masks: std::mem::take(&mut edit.masks),
                        capture: edit.capture,
                        ..Edit::default()
                    };
                })?;
            }
            Action::SetExportFolder(folder) => {
                open.project.export.folder = Some(folder);
                open.save()?;
            }
            Action::SetQuality(quality) => {
                let range = export::QUALITY_RANGE;
                open.project.export.quality = quality.clamp(*range.start(), *range.end());
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
                    return Err(text!(
                        "출력 폴더를 먼저 정하세요",
                        "Choose an output folder first"
                    )
                    .into());
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
                            params: open.params(index, &self.presets),
                            lut: open.lut(index),
                            frame: open.edit(index).frame,
                            masks: open.edit(index).masks,
                            skin: open.edit(index).skin,
                            capture: open.edit(index).capture,
                        }
                    })
                    .collect();
                if photos.is_empty() {
                    return Err(text!("내보낼 사진이 없습니다", "No photos to export").into());
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
            Action::Thumbnail {
                index,
                pixels,
                guess,
            } => {
                if let Some(photo) = open.photos.get_mut(index) {
                    photo.thumbnail = pixels;
                    photo.guess = guess;
                }
            }
            Action::PreviewReady {
                generation,
                index,
                result,
            } => {
                // 그 사이에 다른 사진을 골랐다면 버린다.
                if generation == open.generation {
                    open.preview_serial += 1;
                    open.preview = Some(Preview {
                        index,
                        serial: open.preview_serial,
                        pixels: result?,
                    });
                }
            }
            Action::OriginalReady {
                generation,
                index,
                result,
            } => {
                // 그 사이에 다른 사진을 골랐다면 버린다.
                if generation == open.original_generation {
                    if let Err(error) = &result {
                        log::error(error);
                    }
                    open.preview_serial += 1;
                    open.original = Some(Original {
                        index,
                        jpeg: open.original_jpeg,
                        serial: open.preview_serial,
                        pixels: result,
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
                    Err(error) => ExportState::Failed(error.to_string()),
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
            mask: None,
            show_mask: true,
            compare: false,
            original: None,
            original_jpeg: false,
            sidecar: None,
            capture_auto: None,
            capture_view: None,
            export: ExportState::Idle,
            generation: 0,
            preview_serial: 0,
            original_generation: 0,
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
        self.notice = (!missing.is_empty()).then(|| {
            let missing = missing.join(", ");
            t(
                format!("폴더를 찾을 수 없습니다: {missing}"),
                format!("Folder not found: {missing}"),
            )
        });
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
        let dir = base.join(t("스크린샷", "screenshots"));
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

    /// 가장 최근에 요청한 보정 전 그림의 번호.
    #[cfg(test)]
    pub fn original_generation(&self) -> u64 {
        self.original_generation
    }

    /// 비교 중이면 지금 사진의 보정 전 그림을 요청한다. 이미 들고 있으면 (만들지 못한 경우는 빼고) 하지 않는다.
    /// `before`가 카메라 JPEG인데 그 사진의 JPEG가 없으면 보정 전 RAW를 요청한다.
    fn request_original(&mut self, before: Before) -> Option<Effect> {
        let current = self.current.filter(|_| self.compare)?;
        // 사진 폴더에 파일이 나중에 생길 수도 있으니 요청할 때마다 다시 찾는다 (폴더를 한 번 훑는다).
        self.sidecar = Some((current, sidecar_jpeg(&self.photo_path(current))));
        let jpeg = match before {
            Before::Jpeg => self.sidecar.as_ref().and_then(|(_, jpeg)| jpeg.clone()),
            Before::Raw => None,
        };
        if self.original.as_ref().is_some_and(|original| {
            original.index == current && original.jpeg == jpeg.is_some() && original.pixels.is_ok()
        }) {
            return None;
        }
        self.original_generation += 1;
        self.original_jpeg = jpeg.is_some();
        Some(Effect::Original {
            generation: self.original_generation,
            index: current,
            path: self.photo_path(current),
            jpeg,
        })
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
            guess: None,
        }));
        Effect::Thumbnails(
            (first..self.photos.len())
                .map(|index| (index, self.photo_path(index)))
                .collect(),
        )
    }

    /// 사진에 최종으로 적용할 보정 값.
    pub fn params(&self, index: usize, presets: &[Preset]) -> Params {
        let edit = self.edit(index);
        preset::resolve(presets, edit.preset.as_deref(), edit.strength, &edit.adjust)
    }

    /// 사진에 입힐 LUT 파일과 강도.
    fn lut(&self, index: usize) -> Option<(PathBuf, f32)> {
        self.edit(index).lut.map(|lut| (lut.file, lut.strength))
    }

    fn request_preview(&mut self, index: usize, presets: &[Preset]) -> Effect {
        self.generation += 1;
        let edit = self.edit(index);
        // 마스크를 고쳐 쓰는 동안과 전후를 비교하는 동안에는 프레임 없이 보여 준다
        // (누른 자리가 어긋나지 않게, 두 칸의 크기가 같게).
        let editing = self.mask.filter(|_| self.current == Some(index));
        Effect::Preview {
            generation: self.generation,
            index,
            path: self.photo_path(index),
            denoise: edit.denoise,
            params: self.params(index, presets),
            lut: self.lut(index),
            // 영역(마스크, 샤픈)을 보는 동안에는 프레임을 뺀다 (표시가 사진과 어긋나지 않게).
            frame: edit
                .frame
                .filter(|_| editing.is_none() && !self.compare && self.capture_view.is_none()),
            masks: edit.masks,
            overlay: editing.filter(|_| self.show_mask),
            skin: edit.skin,
            capture: edit.capture,
            capture_view: self.capture_view.filter(|_| edit.capture.enabled),
        }
    }

    /// 지금 보는 사진의 마스크 목록을 바꾸고 미리보기를 다시 요청한다. `commit`이면 파일에도 저장한다.
    fn change_masks(
        &mut self,
        commit: bool,
        presets: &[Preset],
        effects: &mut Vec<Effect>,
        change: impl FnOnce(&mut Vec<Mask>),
    ) -> crate::Result<()> {
        let Some(current) = self.current else {
            return Ok(());
        };
        let (folder, relative) = (
            self.photos[current].folder,
            self.photos[current].relative.clone(),
        );
        let mut edit = self.edit(current);
        change(&mut edit.masks);
        self.project.set_edit(folder, &relative, edit);
        if commit {
            self.save()?;
        }
        effects.push(self.request_preview(current, presets));
        Ok(())
    }

    /// 고쳐 쓰는 중인 마스크를 바꾼다. 고쳐 쓰는 마스크가 없으면 아무 일도 하지 않는다.
    fn change_mask(
        &mut self,
        commit: bool,
        presets: &[Preset],
        effects: &mut Vec<Effect>,
        change: impl FnOnce(&mut Mask),
    ) -> crate::Result<()> {
        let Some(active) = self.mask else {
            return Ok(());
        };
        self.change_masks(commit, presets, effects, |masks| {
            if let Some(mask) = masks.get_mut(active) {
                change(mask)
            }
        })
    }

    /// 선택한 사진들의 편집 내용을 바꾸고 미리보기를 다시 요청한다. `commit`이면 파일에도 저장한다.
    fn change_look(
        &mut self,
        commit: bool,
        presets: &[Preset],
        effects: &mut Vec<Effect>,
        change: impl Fn(&mut Edit),
    ) -> crate::Result<()> {
        for index in self.selected.clone() {
            let (folder, relative) = (
                self.photos[index].folder,
                self.photos[index].relative.clone(),
            );
            let mut edit = self.edit(index);
            change(&mut edit);
            self.project.set_edit(folder, &relative, edit);
        }
        if commit {
            self.save()?;
        }
        if let Some(current) = self.current {
            effects.push(self.request_preview(current, presets));
        }
        Ok(())
    }

    fn start_export(&mut self, photos: Vec<export::Photo>, overwrite: bool) -> Effect {
        self.export = ExportState::Running {
            total: photos.len(),
            report: None,
        };
        let settings = export::Settings {
            // 손으로 고친 프로젝트 파일에 범위 밖의 값이 있을 수 있다.
            quality: self
                .project
                .export
                .quality
                .clamp(*export::QUALITY_RANGE.start(), *export::QUALITY_RANGE.end()),
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
    fn the_compare_split_is_remembered_in_the_config() {
        let mut fx = Fixture::new("split");
        let effects = fx.app.handle(Action::SetCompareSplit(Split::Side));
        assert!(effects.is_empty());
        assert_eq!(fx.app.config.compare_split, Split::Side);
        assert_eq!(
            Config::load(&fx.dir.join("config/config.toml")).compare_split,
            Split::Side,
            "프로젝트를 열지 않아도 저장된다"
        );
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

    /// 효과 가운데 보정 전 그림 요청의 (요청 번호, 사진 번호).
    fn original_requests(effects: &[Effect]) -> Vec<(u64, usize)> {
        effects
            .iter()
            .filter_map(|effect| match effect {
                Effect::Original {
                    generation, index, ..
                } => Some((*generation, *index)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn the_camera_jpeg_is_found_whatever_the_case_of_its_extension() {
        let dir = std::env::temp_dir().join(format!("amsil-sidecar-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in [
            "a.ARW", "a.jpg", "b.ARW", "b.JPEG", "c.ARW", "d.arw", "d.JPG",
        ] {
            std::fs::write(dir.join(name), b"").unwrap();
        }
        let found = [
            sidecar_jpeg(&dir.join("a.ARW")),
            sidecar_jpeg(&dir.join("b.ARW")),
            sidecar_jpeg(&dir.join("c.ARW")),
            sidecar_jpeg(&dir.join("d.arw")),
        ];
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(
            found,
            [
                Some(dir.join("a.jpg")),
                Some(dir.join("b.JPEG")),
                None,
                Some(dir.join("d.JPG"))
            ]
        );
    }

    #[test]
    fn the_before_pane_shows_the_camera_jpeg_when_chosen_and_present() {
        let mut fx = Fixture::with_photos("compare-jpeg");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::SetCompare(true));
        // 사진 a 옆에 카메라 JPEG를 둔다. b에는 없다.
        let a = fx.open().photo_path(0);
        let jpeg = a.with_extension("JPG");
        std::fs::write(&jpeg, b"").unwrap();
        let effects = fx.app.handle(Action::SetCompareBefore(Before::Jpeg));
        assert_eq!(fx.app.config.compare_before, Before::Jpeg);
        assert!(
            effects
                .iter()
                .any(|e| matches!(e, Effect::Original { index: 0, jpeg: Some(path), .. } if *path == jpeg)),
            "{effects:?}"
        );
        // JPEG가 없는 사진은 보정 전 RAW를 보여 준다.
        let effects = fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::Original {
                    index: 1,
                    jpeg: None,
                    ..
                }
            )),
            "{effects:?}"
        );
        // 고른 것을 되돌리면 a의 보정 전도 RAW로 다시 요청한다.
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        let effects = fx.app.handle(Action::SetCompareBefore(Before::Raw));
        assert!(
            effects.iter().any(|e| matches!(
                e,
                Effect::Original {
                    index: 0,
                    jpeg: None,
                    ..
                }
            )),
            "{effects:?}"
        );
    }

    #[test]
    fn comparing_asks_for_the_unedited_photo_once_per_photo() {
        let mut fx = Fixture::with_photos("compare");
        let effects = fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        assert!(
            original_requests(&effects).is_empty(),
            "비교를 켜기 전에는 요청하지 않는다"
        );

        let effects = fx.app.handle(Action::SetCompare(true));
        assert!(fx.open().compare);
        let requests = original_requests(&effects);
        let [(generation, 0)] = requests[..] else {
            panic!("{effects:?}")
        };
        let path = fx.open().photo_path(0);
        assert!(effects.contains(&Effect::Original {
            generation,
            index: 0,
            path,
            jpeg: None,
        }));
        fx.app.handle(Action::OriginalReady {
            generation,
            index: 0,
            result: Ok(pixels()),
        });
        assert_eq!(fx.open().original.as_ref().unwrap().index, 0);

        // 보정을 바꾸거나 비교를 껐다 켜도 다시 만들지 않는다.
        let effects = fx.app.handle(Action::SetStrength {
            value: 0.5,
            commit: true,
        });
        assert!(original_requests(&effects).is_empty());
        fx.app.handle(Action::SetCompare(false));
        let effects = fx.app.handle(Action::SetCompare(true));
        assert!(original_requests(&effects).is_empty());

        // 다른 사진으로 가면 그 사진의 것을 요청한다.
        let effects = fx.app.handle(Action::Select {
            index: 2,
            extend: false,
        });
        assert!(matches!(original_requests(&effects)[..], [(_, 2)]));
    }

    #[test]
    fn comparing_shows_the_edited_photo_without_its_frame() {
        let mut fx = Fixture::with_photos("compare-frame");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        let frame = Frame {
            layout: crate::frame::Layout::Band,
            dark: false,
        };
        fx.app.handle(Action::SetFrame(Some(frame)));
        let shown_frame = |effects: &[Effect]| {
            effects.iter().find_map(|effect| match effect {
                Effect::Preview { frame, .. } => Some(*frame),
                _ => None,
            })
        };
        let effects = fx.app.handle(Action::SetCompare(true));
        assert_eq!(
            shown_frame(&effects),
            Some(None),
            "비교하는 동안에는 두 칸의 크기가 같도록 프레임을 뺀다"
        );
        let effects = fx.app.handle(Action::SetCompare(false));
        assert_eq!(shown_frame(&effects), Some(Some(frame)));
        assert_eq!(
            fx.open().edit(0).frame,
            Some(frame),
            "저장된 프레임은 그대로"
        );
    }

    #[test]
    fn comparing_handles_a_late_or_failed_unedited_photo() {
        let mut fx = Fixture::with_photos("compare-late");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        let first = original_requests(&fx.app.handle(Action::SetCompare(true)))[0].0;
        let second = original_requests(&fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        }))[0]
            .0;
        fx.app.handle(Action::OriginalReady {
            generation: first,
            index: 0,
            result: Ok(pixels()),
        });
        assert!(fx.open().original.is_none(), "옛 사진의 결과는 버린다");

        fx.app.handle(Action::OriginalReady {
            generation: second,
            index: 1,
            result: Err(Text::of("broken")),
        });
        let original = fx.open().original.as_ref().unwrap();
        assert_eq!(original.index, 1);
        assert_eq!(original.pixels, Err(Text::of("broken")));
        // 만들지 못한 사진은 다시 고르면 다시 요청한다.
        let effects = fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        assert!(matches!(original_requests(&effects)[..], [(_, 1)]));
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
            .handle(Action::Backend(Backend::Cpu(Text::of("느립니다"))));
        assert_eq!(fx.app.backend, Backend::Cpu(Text::of("느립니다")));
    }

    #[test]
    fn choosing_a_preset_applies_it_to_the_selected_photos_and_previews_the_result() {
        let mut fx = Fixture::with_photos("preset");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::Select {
            index: 2,
            extend: true,
        });
        let effects = fx.app.handle(Action::SetPreset(Some("food".into())));

        assert_eq!(fx.open().edit(0).preset.as_deref(), Some("food"));
        assert_eq!(fx.open().edit(1).preset, None);
        assert_eq!(fx.open().edit(2).preset.as_deref(), Some("food"));
        let food = fx
            .app
            .presets
            .iter()
            .find(|p| p.id == "food")
            .unwrap()
            .params;
        let [
            Effect::Preview {
                index: 2, params, ..
            },
        ] = &effects[..]
        else {
            panic!("{effects:?}")
        };
        assert_eq!(*params, food, "미리보기는 프리셋을 적용한 값으로 만든다");
        assert_eq!(
            Project::load(&fx.open().path)
                .unwrap()
                .edit(0, "a.ARW")
                .preset
                .as_deref(),
            Some("food")
        );

        fx.app.handle(Action::SetPreset(None));
        assert_eq!(fx.open().edit(0).preset, None);
    }

    #[test]
    fn strength_and_adjustments_preview_at_once_but_are_saved_when_the_slider_is_released() {
        let mut fx = Fixture::with_photos("sliders");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::SetPreset(Some("food".into())));
        let food = fx
            .app
            .presets
            .iter()
            .find(|p| p.id == "food")
            .unwrap()
            .params;
        let on_disk = |fx: &Fixture| Project::load(&fx.open().path).unwrap().edit(0, "a.ARW");

        // 끄는 중: 화면에는 바로 반영되고 파일은 그대로
        let effects = fx.app.handle(Action::SetStrength {
            value: 0.5,
            commit: false,
        });
        assert_eq!(fx.open().edit(0).strength, 0.5);
        assert_eq!(on_disk(&fx).strength, 1.0);
        let [Effect::Preview { params, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(params.temp, food.temp * 0.5);

        // 놓으면 저장
        fx.app.handle(Action::SetStrength {
            value: 0.6,
            commit: true,
        });
        assert_eq!(on_disk(&fx).strength, 0.6);

        let adjust = Params {
            exposure: 0.4,
            ..Params::default()
        };
        let effects = fx.app.handle(Action::SetAdjust {
            params: adjust,
            commit: false,
        });
        assert_eq!(on_disk(&fx).adjust, Params::default());
        let [Effect::Preview { params, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(params.exposure, 0.4);
        fx.app.handle(Action::SetAdjust {
            params: adjust,
            commit: true,
        });
        assert_eq!(on_disk(&fx).adjust.exposure, 0.4);
    }

    #[test]
    fn resetting_the_look_keeps_capture_sharpening() {
        let mut fx = Fixture::with_photos("reset-capture");
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        let off = Capture::off();
        let (folder, relative) = {
            let open = fx.open();
            (open.photos[1].folder, open.photos[1].relative.clone())
        };
        let mut edit = fx.open().edit(1);
        edit.capture = off;
        fx.app
            .open
            .as_mut()
            .unwrap()
            .project
            .set_edit(folder, &relative, edit);
        fx.app.handle(Action::ResetLook);
        assert_eq!(fx.open().edit(1).capture, off);
    }

    #[test]
    fn capture_settings_go_to_every_selected_photo_and_the_preview() {
        let mut fx = Fixture::with_photos("capture");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::Select {
            index: 1,
            extend: true,
        });
        let value = Capture {
            iterations: 40,
            ..Capture::default()
        };
        let effects = fx.app.handle(Action::SetCapture {
            value,
            commit: true,
        });
        assert_eq!(fx.open().edit(0).capture, value);
        assert_eq!(fx.open().edit(1).capture, value);
        assert!(
            matches!(&effects[..], [Effect::Preview { capture, .. }] if *capture == value),
            "{effects:?}"
        );
        // 범위를 벗어난 값은 범위 안으로 들인다.
        fx.app.handle(Action::SetCapture {
            value: Capture {
                iterations: 500,
                radius: Some(9.0),
                threshold: Some(-1.0),
                corner: 3.0,
                ..Capture::default()
            },
            commit: true,
        });
        let kept = fx.open().edit(0).capture;
        assert_eq!(
            (kept.iterations, kept.radius, kept.threshold, kept.corner),
            (100, Some(2.0), Some(0.0), 0.5)
        );
    }

    #[test]
    fn the_automatic_capture_values_are_kept_with_their_photo() {
        let mut fx = Fixture::with_photos("capture-auto");
        fx.app.handle(Action::CaptureAuto {
            index: 0,
            radius: 0.68,
            threshold: 0.12,
        });
        assert_eq!(fx.open().capture_auto, Some((0, 0.68, 0.12)));
    }

    #[test]
    fn viewing_the_sharpened_area_leaves_the_frame_out() {
        let mut fx = Fixture::with_photos("capture-view");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::SetFrame(Some(Frame {
            layout: crate::frame::Layout::Band,
            dark: false,
        })));
        let effects = fx
            .app
            .handle(Action::SetCaptureView(Some(CaptureView::Overlay)));
        assert!(
            matches!(
                &effects[..],
                [Effect::Preview {
                    frame: None,
                    capture_view: Some(CaptureView::Overlay),
                    ..
                }]
            ),
            "{effects:?}"
        );
        let effects = fx.app.handle(Action::SetCaptureView(None));
        assert!(
            matches!(
                &effects[..],
                [Effect::Preview {
                    frame: Some(_),
                    capture_view: None,
                    ..
                }]
            ),
            "{effects:?}"
        );
    }

    #[test]
    fn reset_clears_the_look_but_keeps_denoise() {
        let mut fx = Fixture::with_photos("reset");
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        fx.app.handle(Action::SetDenoiseLevel(0.5));
        fx.app.handle(Action::ApplyDenoise);
        fx.app.handle(Action::SetPreset(Some("mono".into())));
        fx.app.handle(Action::SetStrength {
            value: 0.7,
            commit: true,
        });
        fx.app.handle(Action::ResetLook);
        let edit = fx.open().edit(1);
        assert_eq!(
            (edit.preset, edit.strength, edit.adjust),
            (None, 1.0, Params::default())
        );
        assert_eq!(edit.denoise, 0.5);
    }

    #[test]
    fn export_uses_each_photos_look() {
        let mut fx = Fixture::with_photos("export-look");
        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        fx.app.handle(Action::SetPreset(Some("mono".into())));
        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        let [Effect::Export { photos, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(photos[1].params.mono, 1.0);
        assert!(photos[0].params.is_neutral() && photos[2].params.is_neutral());
    }

    #[test]
    fn a_lut_is_applied_to_selected_photos_previewed_and_exported() {
        let mut fx = Fixture::with_photos("lut");
        let lut = fx.dir.join("필름.cube");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });

        let effects = fx.app.handle(Action::SetLut(Some(lut.clone())));
        let stored = fx.open().edit(0).lut.unwrap();
        assert_eq!((stored.file.clone(), stored.strength), (lut.clone(), 1.0));
        let [
            Effect::Preview {
                lut: Some((file, strength)),
                ..
            },
        ] = &effects[..]
        else {
            panic!("{effects:?}")
        };
        assert_eq!((file, *strength), (&lut, 1.0));

        fx.app.handle(Action::SetLutStrength {
            value: 0.4,
            commit: true,
        });
        assert_eq!(
            Project::load(&fx.open().path)
                .unwrap()
                .edit(0, "a.ARW")
                .lut
                .unwrap()
                .strength,
            0.4
        );

        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        let [Effect::Export { photos, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(photos[0].lut, Some((lut, 0.4)));
        assert_eq!(photos[1].lut, None);
        fx.app.handle(Action::CancelExport);
    }

    #[test]
    fn removing_the_lut_and_resetting_the_look_both_clear_it() {
        let mut fx = Fixture::with_photos("lut-clear");
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        fx.app.handle(Action::SetLut(Some("/luts/a.cube".into())));
        fx.app.handle(Action::SetLut(None));
        assert_eq!(fx.open().edit(1).lut, None);
        fx.app.handle(Action::SetLut(Some("/luts/a.cube".into())));
        fx.app.handle(Action::ResetLook);
        assert_eq!(fx.open().edit(1).lut, None);
    }

    #[test]
    fn user_presets_in_the_config_folder_are_offered_after_the_builtin_ones() {
        let dir =
            std::env::temp_dir().join(format!("amsil-app-user-presets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("presets")).unwrap();
        std::fs::write(
            dir.join("presets/warm.toml"),
            "name = \"따뜻하게\"\n[params]\ntemp = 0.3\n",
        )
        .unwrap();
        std::fs::write(dir.join("presets/bad.toml"), "name = 1\n").unwrap();
        let app = App::new(dir.join("config.toml"));
        std::fs::remove_dir_all(&dir).unwrap();
        let ids: Vec<&str> = app.presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(
            ids,
            ["food", "portrait", "landscape", "night", "mono", "warm"]
        );
        assert!(
            app.notice.as_ref().unwrap().contains("bad"),
            "읽지 못한 프리셋은 알린다"
        );
    }

    #[test]
    fn thumbnails_arrive_one_by_one() {
        let mut fx = Fixture::with_photos("thumbs");
        fx.app.handle(Action::Thumbnail {
            index: 1,
            pixels: Some(pixels()),
            guess: None,
        });
        assert!(fx.open().photos[1].thumbnail.is_some());
        assert!(fx.open().photos[0].thumbnail.is_none());
        fx.app.handle(Action::Thumbnail {
            index: 99,
            pixels: Some(pixels()),
            guess: None,
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
                    denoise: 0.0,
                    params: Params::default(),
                    lut: None,
                    frame: None,
                    masks: Vec::new(),
                    skin: 0.0,
                    capture: Capture::default(),
                },
                export::Photo {
                    input: src.join("c.ARW"),
                    output: out.join("사진/c.heic"),
                    denoise: 0.0,
                    params: Params::default(),
                    lut: None,
                    frame: None,
                    masks: Vec::new(),
                    skin: 0.0,
                    capture: Capture::default(),
                },
                export::Photo {
                    input: src.join("sub/b.ARW"),
                    output: out.join("사진/sub/b.heic"),
                    denoise: 0.5,
                    params: Params::default(),
                    lut: None,
                    frame: None,
                    masks: Vec::new(),
                    skin: 0.0,
                    capture: Capture::default(),
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
            failed: vec![("c.ARW".into(), Text::of("읽을 수 없음"))],
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
            .handle(Action::ExportFinished(Err(Text::of("모델이 없습니다"))));
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

    #[test]
    fn choosing_a_language_saves_it_and_later_messages_use_it() {
        use crate::lang::{self, Lang};
        let mut fx = Fixture::new("lang");
        // 프로젝트를 열기 전(시작 화면)에도 바꿀 수 있다.
        let effects = fx.app.handle(Action::SetLanguage(Lang::En));
        assert_eq!(
            effects,
            [Effect::ProbeBackend],
            "받아 둔 AI 안내 문구도 새 언어로 다시 만든다"
        );
        assert_eq!(lang::current(), Lang::En);
        assert_eq!(
            Config::load(&fx.dir.join("config/config.toml")).lang,
            Some(Lang::En)
        );

        fx.app.handle(Action::NewProject(fx.dir.join("trip.amsil")));
        fx.app.handle(Action::Export {
            only_selected: false,
        });
        assert_eq!(
            fx.app.notice.as_deref(),
            Some("Choose an output folder first")
        );
        fx.app.handle(Action::SetLanguage(Lang::Ko));
        fx.app.handle(Action::Export {
            only_selected: false,
        });
        assert_eq!(fx.app.notice.as_deref(), Some("출력 폴더를 먼저 정하세요"));
    }

    #[test]
    fn a_frame_is_applied_to_selected_photos_previewed_and_exported() {
        use crate::frame::{Frame, Layout};
        let mut fx = Fixture::with_photos("frame");
        let frame = Frame {
            layout: Layout::Band,
            dark: false,
        };
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        let effects = fx.app.handle(Action::SetFrame(Some(frame)));
        assert_eq!(fx.open().edit(0).frame, Some(frame));
        assert_eq!(fx.open().edit(1).frame, None, "고르지 않은 사진은 그대로");
        let [Effect::Preview { frame: shown, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(*shown, Some(frame));
        assert_eq!(
            Project::load(&fx.open().path)
                .unwrap()
                .edit(0, "a.ARW")
                .frame,
            Some(frame)
        );

        // 보정 초기화는 색 보정만 지운다. 프레임은 남는다.
        fx.app.handle(Action::ResetLook);
        assert_eq!(fx.open().edit(0).frame, Some(frame));

        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        let [Effect::Export { photos, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(photos[0].frame, Some(frame));
        assert_eq!(photos[1].frame, None);
        fx.app.handle(Action::CancelExport);

        fx.app.handle(Action::SetFrame(None));
        assert_eq!(fx.open().edit(0).frame, None);
    }

    #[test]
    fn recommended_presets_are_applied_to_the_selected_photos() {
        use crate::recommend::{Category, Guess};
        let mut fx = Fixture::with_photos("recommend");
        let guesses = [
            (0, Category::Food, 0.9),
            (1, Category::Landscape, 0.4), // 확신이 낮다
        ];
        for (index, category, confidence) in guesses {
            fx.app.handle(Action::Thumbnail {
                index,
                pixels: None,
                guess: Some(Guess {
                    category,
                    confidence,
                }),
            });
        }
        assert_eq!(
            fx.open().photos[0].guess.map(|guess| guess.category),
            Some(Category::Food)
        );
        // 세 장 모두 흑백으로 해 두고, 추천을 적용한다. 2번 사진은 아직 분류되지 않았다.
        fx.app.handle(Action::SelectAll);
        fx.app.handle(Action::SetPreset(Some("mono".into())));
        fx.app.handle(Action::ApplyRecommended);
        let preset = |index: usize| fx.open().edit(index).preset;
        assert_eq!(preset(0).as_deref(), Some("food"));
        assert_eq!(preset(1), None, "확신이 낮으면 프리셋 없이 둔다");
        assert_eq!(
            preset(2).as_deref(),
            Some("mono"),
            "분류되지 않은 사진은 건드리지 않는다"
        );
        let saved = Project::load(&fx.open().path).unwrap();
        assert_eq!(saved.edit(0, "a.ARW").preset.as_deref(), Some("food"));
    }

    #[test]
    fn a_missing_classification_model_is_remembered_with_its_reason() {
        let mut fx = Fixture::new("no-clip");
        assert_eq!(fx.app.recommend_problem, None);
        fx.app
            .handle(Action::Recommender(Some(Text::of("모델이 없습니다"))));
        assert_eq!(fx.app.recommend_problem, Some(Text::of("모델이 없습니다")));
        fx.app.handle(Action::Recommender(None));
        assert_eq!(fx.app.recommend_problem, None);
    }

    /// 마지막 동작이 요청한 미리보기에서 마스크와 관련된 것만 꺼낸다: (마스크 수, 영역 표시, 프레임 유무).
    fn mask_preview(effects: &[Effect]) -> (usize, Option<usize>, bool) {
        let [
            Effect::Preview {
                masks,
                overlay,
                frame,
                ..
            },
        ] = effects
        else {
            panic!("{effects:?}")
        };
        (masks.len(), *overlay, frame.is_some())
    }

    #[test]
    fn a_mask_is_made_on_the_current_photo_and_grows_point_by_point() {
        use crate::frame::{Frame, Layout};
        use crate::mask::Point;
        let mut fx = Fixture::with_photos("mask");
        // 사진을 고르기 전에는 만들 곳이 없다.
        assert!(fx.app.handle(Action::AddMask).is_empty());
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::Select {
            index: 1,
            extend: true,
        });
        fx.app.handle(Action::SetFrame(Some(Frame {
            layout: Layout::Band,
            dark: false,
        })));

        let effects = fx.app.handle(Action::AddMask);
        assert_eq!(
            fx.open().mask,
            Some(0),
            "새 마스크가 바로 고쳐 쓸 대상이 된다"
        );
        assert_eq!(
            fx.open().edit(1).masks.len(),
            1,
            "지금 보는 사진에만 생긴다"
        );
        assert_eq!(fx.open().edit(0).masks.len(), 0);
        // 마스크를 고치는 동안에는 누른 자리가 어긋나지 않게 프레임 없이 보여 주고, 고른 영역을 표시한다.
        assert_eq!(mask_preview(&effects), (1, Some(0), false));

        fx.app.handle(Action::AddMaskPoint {
            x: 0.25,
            y: 0.5,
            include: true,
        });
        fx.app.handle(Action::AddMaskPoint {
            x: 1.7,
            y: -0.2,
            include: false,
        });
        let points = fx.open().edit(1).masks[0].points.clone();
        assert_eq!(
            points,
            [
                Point {
                    x: 0.25,
                    y: 0.5,
                    include: true
                },
                Point {
                    x: 1.0,
                    y: 0.0,
                    include: false
                },
            ],
            "사진 밖의 좌표는 가장자리로 당긴다"
        );
        fx.app.handle(Action::UndoMaskPoint);
        assert_eq!(fx.open().edit(1).masks[0].points.len(), 1);

        fx.app.handle(Action::SetMaskInvert(true));
        let adjust = Params {
            exposure: 0.7,
            ..Params::default()
        };
        fx.app.handle(Action::SetMaskAdjust {
            params: adjust,
            commit: true,
        });
        let saved = Project::load(&fx.open().path)
            .unwrap()
            .edit(0, "c.ARW")
            .masks;
        assert_eq!(saved.len(), 1);
        assert!(saved[0].invert && saved[0].adjust == adjust && saved[0].points.len() == 1);

        // 영역 표시를 끄면 보정된 결과만 보인다.
        let effects = fx.app.handle(Action::ShowMask(false));
        assert_eq!(mask_preview(&effects), (1, None, false));
        // 고쳐 쓰기를 마치면 프레임이 다시 보인다.
        let effects = fx.app.handle(Action::SelectMask(None));
        assert_eq!(fx.open().mask, None);
        assert_eq!(mask_preview(&effects), (1, None, true));

        // 보정 초기화는 마스크를 지우지 않는다.
        fx.app.handle(Action::ResetLook);
        assert_eq!(fx.open().edit(1).masks.len(), 1);

        // 내보낼 때 마스크도 함께 간다.
        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        let [Effect::Export { photos, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(
            photos
                .iter()
                .map(|photo| photo.masks.len())
                .collect::<Vec<_>>(),
            [0, 1, 0]
        );
        fx.app.handle(Action::CancelExport);

        fx.app.handle(Action::SelectMask(Some(0)));
        fx.app.handle(Action::RemoveMask);
        assert_eq!(fx.open().edit(1).masks.len(), 0);
        assert_eq!(fx.open().mask, None);
    }

    #[test]
    fn choosing_another_photo_stops_editing_the_mask() {
        let mut fx = Fixture::with_photos("mask-leave");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        fx.app.handle(Action::AddMask);
        assert_eq!(fx.open().mask, Some(0));
        fx.app.handle(Action::Select {
            index: 1,
            extend: false,
        });
        assert_eq!(fx.open().mask, None);
        // 고쳐 쓰는 마스크가 없으면 점을 찍어도 아무 일도 없다.
        assert!(
            fx.app
                .handle(Action::AddMaskPoint {
                    x: 0.5,
                    y: 0.5,
                    include: true
                })
                .is_empty()
        );
        // 없는 번호는 고를 수 없다.
        fx.app.handle(Action::SelectMask(Some(7)));
        assert_eq!(fx.open().mask, None);
    }

    #[test]
    fn a_missing_mask_model_is_remembered_with_its_reason() {
        let mut fx = Fixture::new("no-sam");
        fx.app
            .handle(Action::Segmenter(Some(Text::of("모델이 없습니다"))));
        assert_eq!(fx.app.mask_problem, Some(Text::of("모델이 없습니다")));
        fx.app.handle(Action::Segmenter(None));
        assert_eq!(fx.app.mask_problem, None);
    }

    #[test]
    fn skin_retouch_is_set_on_selected_photos_previewed_exported_and_reset() {
        let mut fx = Fixture::with_photos("skin");
        fx.app.handle(Action::Select {
            index: 0,
            extend: false,
        });
        let effects = fx.app.handle(Action::SetSkin {
            value: 0.6,
            commit: true,
        });
        let [Effect::Preview { skin, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!(*skin, 0.6);
        assert_eq!((fx.open().edit(0).skin, fx.open().edit(1).skin), (0.6, 0.0));
        assert_eq!(
            Project::load(&fx.open().path)
                .unwrap()
                .edit(0, "a.ARW")
                .skin,
            0.6
        );

        fx.app.handle(Action::SetExportFolder(fx.dir.join("결과")));
        let effects = fx.app.handle(Action::Export {
            only_selected: false,
        });
        let [Effect::Export { photos, .. }] = &effects[..] else {
            panic!("{effects:?}")
        };
        assert_eq!((photos[0].skin, photos[1].skin), (0.6, 0.0));
        fx.app.handle(Action::CancelExport);

        // 범위를 벗어난 값은 끝으로 당긴다. 보정 초기화는 피부 보정도 지운다.
        fx.app.handle(Action::SetSkin {
            value: 3.0,
            commit: true,
        });
        assert_eq!(fx.open().edit(0).skin, 1.0);
        fx.app.handle(Action::ResetLook);
        assert_eq!(fx.open().edit(0).skin, 0.0);
    }

    #[test]
    fn a_missing_skin_model_is_remembered_with_its_reason() {
        let mut fx = Fixture::new("no-skin");
        fx.app
            .handle(Action::SkinModel(Some(Text::of("모델이 없습니다"))));
        assert_eq!(fx.app.skin_problem, Some(Text::of("모델이 없습니다")));
        fx.app.handle(Action::SkinModel(None));
        assert_eq!(fx.app.skin_problem, None);
    }

    #[test]
    fn quality_is_kept_within_the_supported_range() {
        let mut fx = Fixture::with_photos("quality-range");
        let quality = |fx: &Fixture| fx.open().project.export.quality;
        fx.app.handle(Action::SetQuality(100));
        assert_eq!(quality(&fx), 100, "끝까지 올릴 수 있다");
        fx.app.handle(Action::SetQuality(200));
        assert_eq!(quality(&fx), 100);
        fx.app.handle(Action::SetQuality(3));
        assert_eq!(quality(&fx), 40);
    }

    #[test]
    fn a_notification_from_the_screen_is_shown_as_the_notice() {
        let mut fx = Fixture::new("notify");
        fx.app
            .handle(Action::Notify("로그 파일: /tmp/log.txt".into()));
        assert_eq!(fx.app.notice.as_deref(), Some("로그 파일: /tmp/log.txt"));
    }

    #[test]
    fn choosing_light_or_dark_mode_is_saved() {
        use crate::project::Theme;
        let mut fx = Fixture::new("theme");
        // 프로젝트를 열기 전(시작 화면)에도 바꿀 수 있다.
        assert!(fx.app.handle(Action::SetTheme(Theme::Light)).is_empty());
        assert_eq!(fx.app.config.theme, Some(Theme::Light));
        assert_eq!(
            Config::load(&fx.dir.join("config/config.toml")).theme,
            Some(Theme::Light)
        );
        fx.app.handle(Action::SetTheme(Theme::Dark));
        assert_eq!(
            Config::load(&fx.dir.join("config/config.toml")).theme,
            Some(Theme::Dark)
        );
    }
}
