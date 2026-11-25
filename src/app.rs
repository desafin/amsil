//! Program state, independent of the UI.
//!
//! Everything the user does and every result of a background job comes in as an `Action`.
//! `App::handle` changes the state and returns the work to do outside the UI (making thumbnails,
//! exporting, and so on) as `Effect`s. UI code only reads the state and draws it.

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
    /// The open project. None means the start screen.
    pub open: Option<Open>,
    /// Notice to show in the status bar (errors included).
    pub notice: Option<String>,
    /// Where the AI models run.
    pub backend: Backend,
    /// Presets to choose from: the built-in ones followed by the user's own.
    pub presets: Vec<Preset>,
    /// Why preset recommendation (photo classification) is unavailable. None if available.
    pub recommend_problem: Option<Text>,
    /// Why masks are unavailable. None if available.
    pub mask_problem: Option<Text>,
    /// Why skin retouching is unavailable. None if available.
    pub skin_problem: Option<Text>,
}

/// Where the AI model (denoising) runs.
#[derive(Debug, Clone, PartialEq)]
pub enum Backend {
    /// Still checking.
    Unknown,
    Gpu,
    /// The GPU is unavailable, so it runs on the CPU. A note that it is slow, and how to fix it.
    Cpu(Text),
    /// The model cannot be used (for example, the file is missing). The reason and how to fix it.
    Unavailable(Text),
}

/// Working state of the open project.
pub struct Open {
    /// Location of the project file.
    pub path: PathBuf,
    pub project: Project,
    /// Photos from all imported folders.
    pub photos: Vec<Photo>,
    pub selected: BTreeSet<usize>,
    /// The photo shown in the preview.
    pub current: Option<usize>,
    /// Value of the denoise slider. It reaches the photo only when [Apply] is pressed.
    pub denoise_level: f32,
    pub preview: Option<Preview>,
    /// Index of the mask being edited on the current photo. Clicking the preview adds a point to
    /// this mask.
    pub mask: Option<usize>,
    /// Whether to tint the area selected by the edited mask red in the preview.
    pub show_mask: bool,
    /// Whether before/after comparison is on. When on, the preview is split into a before pane and
    /// an after pane.
    pub compare: bool,
    /// The before image shown while comparing (only one is kept).
    pub original: Option<Original>,
    /// Whether the last requested before image is the camera JPEG.
    original_jpeg: bool,
    /// (photo index, that photo's camera JPEG). Looked up for the current photo when the before
    /// image is requested.
    pub sidecar: Option<(usize, Option<PathBuf>)>,
    /// Capture sharpening (photo index, radius, contrast threshold) derived from the photo. Only
    /// the most recently read photo's is kept.
    pub capture_auto: Option<(usize, f32, f32)>,
    /// Sharpening area view (not saved).
    pub capture_view: Option<CaptureView>,
    pub export: ExportState,
    /// Preview request number. Used to drop stale results that arrive late.
    generation: u64,
    preview_serial: u64,
    /// Request number of the before image. Used to drop stale results that arrive late.
    original_generation: u64,
}

pub struct Photo {
    /// Position in `project.folders`.
    pub folder: usize,
    /// Path relative to the folder.
    pub relative: String,
    pub thumbnail: Option<Pixels>,
    /// A guess at what the photo shows. None if not yet classified.
    pub guess: Option<Guess>,
}

pub struct Preview {
    pub index: usize,
    /// A number that changes whenever the image changes. The UI uses it to know whether to
    /// re-upload the texture.
    pub serial: u64,
    pub pixels: Pixels,
}

/// The camera JPEG with the same name in the same folder (`.jpg`, `.jpeg`, any case). None if there
/// is none.
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

/// The photo with no adjustments (the before pane of the comparison). May be the camera JPEG.
pub struct Original {
    pub index: usize,
    /// Whether it was read from the camera JPEG.
    pub jpeg: bool,
    /// A number that changes whenever the image changes. The UI uses it to know whether to
    /// re-upload the texture.
    pub serial: u64,
    /// The reason, if it could not be made.
    pub pixels: std::result::Result<Pixels, Text>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ExportState {
    Idle,
    /// Asking what to do with output files that already exist.
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

/// What the UI needs from a `Report` (kept separate so it can be compared).
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

/// Something the user did, or a result reported by a background job.
pub enum Action {
    NewProject(PathBuf),
    OpenProject(PathBuf),
    CloseProject,
    ImportFolder(PathBuf),
    /// If `extend` is true, adds to the current selection.
    Select {
        index: usize,
        extend: bool,
    },
    SelectAll,
    SetDenoiseLevel(f32),
    /// Applies the slider's level to the selected photos.
    ApplyDenoise,
    /// Sets the preset of the selected photos (None removes it).
    SetPreset(Option<String>),
    /// Applies to each selected photo the preset that fits its content. Removes the preset if none
    /// fits.
    ApplyRecommended,
    /// Preset strength. `commit` is false while the slider is dragged and comes once as true on
    /// release (that is when it is saved).
    SetStrength {
        value: f32,
        commit: bool,
    },
    /// Manual adjustments.
    SetAdjust {
        params: Params,
        commit: bool,
    },
    /// Sets the LUT file applied to the selected photos (None removes it).
    SetLut(Option<PathBuf>),
    /// LUT strength.
    SetLutStrength {
        value: f32,
        commit: bool,
    },
    /// Sets the frame around the selected photos (None removes it).
    SetFrame(Option<Frame>),
    /// Creates a new mask on the current photo and starts editing it.
    AddMask,
    /// Picks the mask to edit (None ends editing).
    SelectMask(Option<usize>),
    /// Deletes the mask being edited.
    RemoveMask,
    /// Adds a clicked point to the mask being edited. Coordinates run from (0, 0) at the photo's
    /// top left to (1, 1) at its bottom right.
    AddMaskPoint {
        x: f32,
        y: f32,
        include: bool,
    },
    /// Removes the last clicked point.
    UndoMaskPoint,
    SetMaskInvert(bool),
    /// Adjustments added only within the area of the mask being edited.
    SetMaskAdjust {
        params: Params,
        commit: bool,
    },
    /// Whether to show the selected area in the preview.
    ShowMask(bool),
    /// Turns before/after comparison on or off.
    SetCompare(bool),
    /// Skin retouching strength of the selected photos (0 to 1).
    SetSkin {
        value: f32,
        commit: bool,
    },
    /// Capture sharpening values of the selected photos. Out-of-range values are clamped into
    /// range.
    SetCapture {
        value: Capture,
        commit: bool,
    },
    /// Changes the sharpening area view (None turns it off).
    SetCaptureView(Option<CaptureView>),
    /// What to show in the before pane of the comparison (saved to the config file).
    SetCompareBefore(Before),
    /// Capture sharpening radius and contrast threshold the worker derived while reading the photo
    /// (used when no value is set).
    CaptureAuto {
        index: usize,
        radius: f32,
        threshold: f32,
    },
    /// Clears the preset, manual adjustments, LUT, and skin retouching (denoise, frame, and masks
    /// stay).
    ResetLook,
    SetExportFolder(PathBuf),
    SetQuality(u8),
    Export {
        only_selected: bool,
    },
    /// Whether to overwrite (true) or skip (false) files that already exist.
    ConfirmExport {
        overwrite: bool,
    },
    CancelExport,
    /// Closes the export result display.
    DismissExport,
    /// Saves a screenshot.
    SaveScreenshot(Pixels),
    /// Changes the language of the UI and messages.
    SetLanguage(Lang),
    /// The UI has something to report (shown in the status bar).
    Notify(String),
    /// Light or dark UI.
    SetTheme(Theme),
    /// Sets the direction the preview is split for before/after comparison (saved to the config).
    SetCompareSplit(Split),

    Thumbnail {
        index: usize,
        pixels: Option<Pixels>,
        /// A guess at the kind of photo (when the classification model is available).
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
    /// Whether preset recommendation is available. If not, the reason.
    Recommender(Option<Text>),
    /// Whether masks are available. If not, the reason.
    Segmenter(Option<Text>),
    /// Whether skin retouching is available. If not, the reason.
    SkinModel(Option<Text>),
}

/// Work to do outside the UI.
#[derive(Debug, PartialEq)]
// Only a few effects are made per action, so differing variant sizes do not matter.
#[allow(clippy::large_enum_variant)]
pub enum Effect {
    /// Makes thumbnails for the (photo index, file path) pairs.
    Thumbnails(Vec<(usize, PathBuf)>),
    Preview {
        generation: u64,
        index: usize,
        path: PathBuf,
        denoise: f32,
        /// Adjustment values (preset × strength + manual adjustments).
        params: Params,
        /// LUT file and strength, applied last.
        lut: Option<(PathBuf, f32)>,
        /// Frame around the photo.
        frame: Option<Frame>,
        /// Local adjustment masks.
        masks: Vec<Mask>,
        /// Tints the area selected by the mask with this index red.
        overlay: Option<usize>,
        /// Skin retouching strength.
        skin: f32,
        /// Capture sharpening (RAW stage).
        capture: Capture,
        /// Whether to show where capture sharpening applies.
        capture_view: Option<CaptureView>,
    },
    /// Makes the photo with no adjustments (the before pane of the comparison). If `jpeg` is set,
    /// reads that camera JPEG instead. The result arrives as `Action::OriginalReady`.
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
    /// Checks (again) whether the AI can run on the GPU. The result arrives as `Action::Backend`.
    ProbeBackend,
}

impl App {
    /// Reads the config at `config_path` and starts at the start screen.
    pub fn new(config_path: PathBuf) -> Self {
        // User presets live in the presets folder next to the config file.
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
                // Errors shown on screen are also logged (a notice disappears when the next one
                // replaces it).
                log::error(&error);
                self.notice = Some(error.to_string());
                Vec::new()
            }
        }
    }

    fn try_handle(&mut self, action: Action) -> crate::Result<Vec<Effect>> {
        // Actions that do not need a project
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
                // The AI notes received earlier are in the old language. Fetch them again in the
                // new one.
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
                // Masks are per photo. Moving to another photo ends editing.
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
                    // Photos not yet classified are left alone.
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
                // The frame is dropped while comparing, so request the after image again.
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
                // Drop it if another photo was picked in the meantime.
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
                // Drop it if another photo was picked in the meantime.
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

    /// Opens a project and enters the work screen. Rescans the folders to build the photo list.
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
            // ponytail: scans the folders right here. A large folder on a slow disk can briefly
            // freeze the UI.
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

    /// Saves a screenshot as PNG, in a screenshots folder next to the project if one is open (named
    /// `스크린샷` in Korean), otherwise in the config folder.
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
    /// Full path of the photo file.
    pub fn photo_path(&self, index: usize) -> PathBuf {
        let photo = &self.photos[index];
        self.project.folders[photo.folder]
            .path
            .join(&photo.relative)
    }

    /// Number of the most recently requested preview.
    #[cfg(test)]
    pub fn preview_generation(&self) -> u64 {
        self.generation
    }

    /// Number of the most recently requested before image.
    #[cfg(test)]
    pub fn original_generation(&self) -> u64 {
        self.original_generation
    }

    /// While comparing, requests the before image of the current photo. Does nothing if it is
    /// already held (unless it could not be made). If `before` is the camera JPEG but the photo has
    /// none, requests the unadjusted RAW.
    fn request_original(&mut self, before: Before) -> Option<Effect> {
        let current = self.current.filter(|_| self.compare)?;
        // A file may appear in the photo folder later, so look again on every request (one folder
        // scan).
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

    /// Edits saved for the photo.
    pub fn edit(&self, index: usize) -> Edit {
        let photo = &self.photos[index];
        self.project.edit(photo.folder, &photo.relative)
    }

    fn save(&self) -> crate::Result<()> {
        self.project.save(&self.path)
    }

    /// Appends the folder's photos to the list and returns requests to make their thumbnails.
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

    /// Final adjustment values to apply to the photo.
    pub fn params(&self, index: usize, presets: &[Preset]) -> Params {
        let edit = self.edit(index);
        preset::resolve(presets, edit.preset.as_deref(), edit.strength, &edit.adjust)
    }

    /// LUT file and strength to apply to the photo.
    fn lut(&self, index: usize) -> Option<(PathBuf, f32)> {
        self.edit(index).lut.map(|lut| (lut.file, lut.strength))
    }

    fn request_preview(&mut self, index: usize, presets: &[Preset]) -> Effect {
        self.generation += 1;
        let edit = self.edit(index);
        // While editing a mask or comparing before/after, show it without the frame (so clicks line
        // up and both panes are the same size).
        let editing = self.mask.filter(|_| self.current == Some(index));
        Effect::Preview {
            generation: self.generation,
            index,
            path: self.photo_path(index),
            denoise: edit.denoise,
            params: self.params(index, presets),
            lut: self.lut(index),
            // Drop the frame while viewing an area (mask, sharpening) so the overlay lines up with
            // the photo.
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

    /// Changes the current photo's mask list and requests the preview again. With `commit`, also
    /// saves to the file.
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

    /// Changes the mask being edited. Does nothing if no mask is being edited.
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

    /// Changes the edits of the selected photos and requests the preview again. With `commit`, also
    /// saves to the file.
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
            // A hand-edited project file may contain out-of-range values.
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

    /// Temporary folder: config file, project file, and a photo folder with fake RAW files.
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

        /// State with a project created and the photo folder imported.
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
            "saved to a file as soon as it is created"
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
            "the imported folder is saved"
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
            "saved even without an open project"
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
            "a plain click selects only that photo"
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
        // A result for an earlier-requested photo is dropped even if it arrives late.
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

    /// (request number, photo index) of the before-image requests among the effects.
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
        // Put a camera JPEG next to photo a. b has none.
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
        // A photo without a JPEG shows the unadjusted RAW.
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
        // Reverting the choice requests a's before image as RAW again.
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
            "not requested before comparison is on"
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

        // Changing adjustments or toggling comparison does not make it again.
        let effects = fx.app.handle(Action::SetStrength {
            value: 0.5,
            commit: true,
        });
        assert!(original_requests(&effects).is_empty());
        fx.app.handle(Action::SetCompare(false));
        let effects = fx.app.handle(Action::SetCompare(true));
        assert!(original_requests(&effects).is_empty());

        // Moving to another photo requests that photo's.
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
            "the frame is dropped while comparing so both panes are the same size"
        );
        let effects = fx.app.handle(Action::SetCompare(false));
        assert_eq!(shown_frame(&effects), Some(Some(frame)));
        assert_eq!(
            fx.open().edit(0).frame,
            Some(frame),
            "the saved frame is unchanged"
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
        assert!(fx.open().original.is_none(), "stale photo result dropped");

        fx.app.handle(Action::OriginalReady {
            generation: second,
            index: 1,
            result: Err(Text::of("broken")),
        });
        let original = fx.open().original.as_ref().unwrap();
        assert_eq!(original.index, 1);
        assert_eq!(original.pixels, Err(Text::of("broken")));
        // A photo that could not be made is requested again when picked again.
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
            "moving the slider alone does not change the photo"
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
        assert_eq!(*params, food, "preview uses the preset-applied values");
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

        // While dragging: shown on screen at once, file unchanged
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

        // Saved on release
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
        // Out-of-range values are clamped into range.
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
            "presets that fail to load are reported"
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
        }); // Out of range is ignored
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
        assert!(effects.is_empty(), "does not start before asking");
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
            "does not start again while running"
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
        // Can be changed before a project is opened (start screen) too.
        let effects = fx.app.handle(Action::SetLanguage(Lang::En));
        assert_eq!(
            effects,
            [Effect::ProbeBackend],
            "stored AI notes are remade in the new language too"
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
        assert_eq!(fx.open().edit(1).frame, None, "other photos unchanged");
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

        // Resetting adjustments clears only color adjustments. The frame stays.
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
            (1, Category::Landscape, 0.4), // Low confidence
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
        // Set all three to mono, then apply recommendations. Photo 2 is not yet classified.
        fx.app.handle(Action::SelectAll);
        fx.app.handle(Action::SetPreset(Some("mono".into())));
        fx.app.handle(Action::ApplyRecommended);
        let preset = |index: usize| fx.open().edit(index).preset;
        assert_eq!(preset(0).as_deref(), Some("food"));
        assert_eq!(preset(1), None, "low confidence leaves no preset");
        assert_eq!(
            preset(2).as_deref(),
            Some("mono"),
            "unclassified photos are left alone"
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

    /// Extracts the mask-related parts of the preview requested by the last action: (mask count,
    /// area shown, frame present).
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
        // Before a photo is picked, there is nowhere to make one.
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
            "the new mask becomes the one being edited"
        );
        assert_eq!(
            fx.open().edit(1).masks.len(),
            1,
            "created only on the current photo"
        );
        assert_eq!(fx.open().edit(0).masks.len(), 0);
        // While editing a mask, show it without the frame so clicks line up, and show the selected
        // area.
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
            "coordinates outside the photo are clamped to the edge"
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

        // Turning off the area display shows only the adjusted result.
        let effects = fx.app.handle(Action::ShowMask(false));
        assert_eq!(mask_preview(&effects), (1, None, false));
        // Ending editing shows the frame again.
        let effects = fx.app.handle(Action::SelectMask(None));
        assert_eq!(fx.open().mask, None);
        assert_eq!(mask_preview(&effects), (1, None, true));

        // Resetting adjustments does not delete masks.
        fx.app.handle(Action::ResetLook);
        assert_eq!(fx.open().edit(1).masks.len(), 1);

        // Masks go along on export.
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
        // With no mask being edited, clicking a point does nothing.
        assert!(
            fx.app
                .handle(Action::AddMaskPoint {
                    x: 0.5,
                    y: 0.5,
                    include: true
                })
                .is_empty()
        );
        // A nonexistent index cannot be picked.
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

        // Out-of-range values are clamped to the ends. Resetting adjustments also clears skin
        // retouching.
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
        assert_eq!(quality(&fx), 100, "can go all the way up");
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
        // Can be changed before a project is opened (start screen) too.
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
