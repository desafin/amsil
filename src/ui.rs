//! Drawing the screen. Reads the state (`App`), draws it, and returns what the user did as an
//! `Action`. No logic lives here.

use crate::app::{Action, App, Backend, ExportState, Open, ReportView};
use crate::capture::CaptureView;
use crate::credits::{self, Credit, Feature};
use crate::frame::{Frame, Layout as Shape};
use crate::job::{describe_remaining, describe_resources};
use crate::lang::{self, Lang, Text, t};
use crate::preview::Pixels;
use crate::project::{Before, Split, Theme};
use crate::recommend;
use eframe::egui::{self, Align, Key, Layout, Panel, RichText, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::PathBuf;

/// Things the screen asks of the outside world. Swapped for a fake in tests.
pub trait Env {
    /// Asks where to save a new project file.
    fn new_project_path(&mut self) -> Option<PathBuf>;
    /// Asks for a project file to open.
    fn open_project_path(&mut self) -> Option<PathBuf>;
    /// Asks for a folder.
    fn pick_folder(&mut self) -> Option<PathBuf>;
    /// Asks for a LUT (.cube) file.
    fn pick_lut(&mut self) -> Option<PathBuf>;
    /// Captures the current window. The result arrives later as `Action::SaveScreenshot`.
    fn screenshot(&mut self, ctx: &egui::Context);
    /// Opens the log file. Returns a message for the screen (the file's location, or why it could
    /// not be opened).
    fn open_log(&mut self) -> String;
}

/// State used only by the screen: images uploaded to the GPU and which windows are open.
#[derive(Default)]
pub struct View {
    /// Which project the textures belong to. Dropped when the project changes.
    project: Option<PathBuf>,
    thumbnails: HashMap<usize, TextureHandle>,
    /// (request number, texture)
    preview: Option<(u64, TextureHandle)>,
    /// The unedited image for the before/after comparison. (number, texture)
    original: Option<(u64, TextureHandle)>,
    show_backend_help: bool,
    /// Whether the "AI features and sources" window is open.
    show_ai_info: bool,
    /// The quality slider value while it is being dragged.
    quality: Option<u8>,
    /// The window title set last.
    title: String,
    /// The preview's zoom state, and which photo it belongs to.
    zoom: Zoom,
    zoomed_photo: Option<usize>,
}

/// Gap between the two panes in the before/after comparison (screen units).
const PANE_GAP: f32 = 4.0;
/// How many screen points one source pixel may be enlarged to (4 = 400%).
const MAX_ZOOM: f32 = 4.0;
/// How much one wheel notch zooms.
const WHEEL_ZOOM: f32 = 0.005;

/// Registers a Korean font (NanumGothic). egui's built-in fonts have no Hangul, so on a computer
/// without a Korean system font the text shows up as boxes. Bundling the font makes the screen look
/// the same everywhere.
pub fn install_fonts(ctx: &egui::Context) {
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "NanumGothic".into(),
        egui::FontData::from_static(crate::frame::FONT).into(),
    );
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .insert(0, "NanumGothic".into());
    }
    ctx.set_fonts(fonts);
}

/// Draws one frame and returns what the user did during it.
pub fn draw(ui: &mut egui::Ui, app: &App, view: &mut View, env: &mut dyn Env) -> Vec<Action> {
    apply_theme(ui.ctx(), app.config.theme);
    let mut actions = Vec::new();
    if ui.input(|input| input.key_pressed(Key::F12)) {
        env.screenshot(ui.ctx());
    }
    match &app.open {
        None => {
            // Drop what belongs to the project (textures, zoom state), but keep open windows as
            // they are.
            *view = View {
                show_ai_info: view.show_ai_info,
                ..View::default()
            };
            start_screen(ui, app, view, env, &mut actions);
        }
        Some(open) => {
            if view.project.as_ref() != Some(&open.path) {
                *view = View {
                    project: Some(open.path.clone()),
                    show_ai_info: view.show_ai_info,
                    ..View::default()
                };
            }
            workspace(ui, app, open, view, env, &mut actions);
        }
    }
    if view.show_ai_info {
        ai_info(ui.ctx(), app, view);
    }
    actions
}

/// Program name.
fn app_name() -> &'static str {
    t("암실", "Amsil")
}

/// Matches placement order to on-screen order. Where a layout fills from the right, later items end
/// up on the left, so reverse them.
fn in_reading_order<T, const N: usize>(ui: &egui::Ui, mut items: [T; N]) -> [T; N] {
    if ui.layout().main_dir() == egui::Direction::RightToLeft {
        items.reverse();
    }
    items
}

/// Buttons for choosing the language. Each language's name is written in that language, so people
/// who cannot read the current one can still find theirs.
fn language_buttons(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    for (choice, name) in in_reading_order(ui, [(Lang::Ko, "한국어"), (Lang::En, "English")]) {
        let chosen = lang::current() == choice;
        if ui.selectable_label(chosen, name).clicked() && !chosen {
            actions.push(Action::SetLanguage(choice));
        }
    }
}

/// Applies the chosen brightness mode to the screen. If none was chosen, follows the system
/// setting.
fn apply_theme(ctx: &egui::Context, theme: Option<Theme>) {
    let wanted = match theme {
        Some(Theme::Light) => egui::ThemePreference::Light,
        Some(Theme::Dark) => egui::ThemePreference::Dark,
        None => egui::ThemePreference::System,
    };
    if ctx.options(|options| options.theme_preference) != wanted {
        ctx.set_theme(wanted);
    }
}

/// Buttons for choosing light or dark mode. The one currently applied looks pressed.
fn theme_buttons(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    let choices = [
        (Theme::Light, egui::Theme::Light, t("라이트", "Light")),
        (Theme::Dark, egui::Theme::Dark, t("다크", "Dark")),
    ];
    for (choice, shown, name) in in_reading_order(ui, choices) {
        let chosen = ui.ctx().theme() == shown;
        if ui.selectable_label(chosen, name).clicked() && !chosen {
            actions.push(Action::SetTheme(choice));
        }
    }
}

/// Button that opens the window showing where the AI features' models come from.
fn ai_info_button(ui: &mut egui::Ui, view: &mut View) {
    if ui
        .button(t("AI 정보", "AI info"))
        .on_hover_text(t(
            "AI 기능이 어떤 모델을 쓰고, 그 모델을 누가 만들었는지 봅니다",
            "Shows which models the AI features use and who made them",
        ))
        .clicked()
    {
        view.show_ai_info = true;
    }
}

/// Lists a feature's model on one line. Details are in the "AI info" window.
fn credit_line(ui: &mut egui::Ui, prefix: &str, credit: &Credit) {
    ui.add(egui::Label::new(
        RichText::new(format!("{prefix}: {} · {}", credit.model, credit.by))
            .weak()
            .small(),
    ))
    .on_hover_text(format!(
        "{} {}\n{}\n{}",
        t("라이선스", "Licence"),
        credit.license,
        credit.source,
        t(
            "자세한 것은 위쪽의 [AI 정보]에 있습니다",
            "See [AI info] at the top for details"
        )
    ));
}

/// Whether that feature's model is ready.
fn model_status(app: &App, feature: Feature) -> &'static str {
    let ready = t("준비됨", "Ready");
    let missing = t("모델 없음", "No model");
    let problem = |problem: &Option<Text>| if problem.is_some() { missing } else { ready };
    match feature {
        Feature::Denoise => match &app.backend {
            Backend::Unknown => t("확인 중", "Checking"),
            Backend::Gpu => t("준비됨 (GPU)", "Ready (GPU)"),
            Backend::Cpu(_) => t("준비됨 (CPU, 느림)", "Ready (CPU, slow)"),
            Backend::Unavailable(_) => missing,
        },
        Feature::Mask => problem(&app.mask_problem),
        Feature::Recommend => problem(&app.recommend_problem),
        Feature::Skin => problem(&app.skin_problem),
    }
}

/// Window that gathers the models used by AI features and their sources.
fn ai_info(ctx: &egui::Context, app: &App, view: &mut View) {
    let mut open = true;
    egui::Window::new(t("AI 기능과 출처", "AI features and sources"))
        .open(&mut open)
        .collapsible(false)
        // Open in the middle of the window, using most of its height (it can be dragged or
        // resized).
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .default_width(540.0)
        .default_height(ctx.content_rect().height() * 0.75)
        .vscroll(true)
        .show(ctx, |ui| {
            ui.label(t(
                "AI 기능은 다른 사람들이 만들어 공개한 모델을 그대로 씁니다. 모델은 이 컴퓨터 안에서만 돌고, 사진은 밖으로 나가지 않습니다.",
                "The AI features use models that other people made and published, unchanged. The models run only on this computer; your photos never leave it.",
            ));
            for credit in credits::ALL {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.strong(t(credit.name.0, credit.name.1));
                    ui.weak(model_status(app, credit.feature));
                });
                egui::Grid::new(credit.model)
                    .num_columns(2)
                    .show(ui, |ui| {
                        ui.weak(t("모델", "Model"));
                        ui.label(credit.model);
                        ui.end_row();
                        ui.weak(t("만든 곳", "Made by"));
                        ui.label(credit.by);
                        ui.end_row();
                        ui.weak(t("라이선스", "Licence"));
                        ui.label(credit.license);
                        ui.end_row();
                        ui.weak(t("출처", "Source"));
                        ui.hyperlink(credit.source);
                        ui.end_row();
                        if let Some(paper) = credit.paper {
                            ui.weak(t("논문", "Paper"));
                            ui.hyperlink(paper);
                            ui.end_row();
                        }
                    });
                ui.horizontal_wrapped(|ui| {
                    ui.weak(t("받는 곳", "Downloaded from"));
                    ui.label(t(credit.from.0, credit.from.1));
                });
                ui.weak(t(credit.note.0, credit.note.1));
            }
        });
    view.show_ai_info = open;
}

/// Button that opens the log file to look at when something goes wrong.
fn log_button(ui: &mut egui::Ui, env: &mut dyn Env, actions: &mut Vec<Action>) {
    if ui
        .button(t("로그 열기", "Open log"))
        .on_hover_text(t(
            "오류와 내보내기 기록이 남는 파일을 엽니다",
            "Opens the file where errors and exports are recorded",
        ))
        .clicked()
    {
        actions.push(Action::Notify(env.open_log()));
    }
}

fn start_screen(
    ui: &mut egui::Ui,
    app: &App,
    view: &mut View,
    env: &mut dyn Env,
    actions: &mut Vec<Action>,
) {
    Panel::top("language").show(ui, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            language_buttons(ui, actions);
            ui.separator();
            theme_buttons(ui, actions);
            ui.separator();
            log_button(ui, env, actions);
            ai_info_button(ui, view);
        });
    });
    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.2);
            ui.heading(app_name());
            ui.label(t("RAW 사진을 HEIF로", "RAW photos to HEIF"));
            ui.add_space(24.0);
            if ui.button(t("새 프로젝트", "New project")).clicked()
                && let Some(path) = env.new_project_path()
            {
                actions.push(Action::NewProject(path));
            }
            if ui.button(t("프로젝트 열기", "Open project")).clicked()
                && let Some(path) = env.open_project_path()
            {
                actions.push(Action::OpenProject(path));
            }
            if !app.config.recent.is_empty() {
                ui.add_space(24.0);
                ui.label(t("최근 프로젝트", "Recent projects"));
                for path in &app.config.recent {
                    let name = path.file_stem().map_or_else(
                        || path.display().to_string(),
                        |n| n.to_string_lossy().into(),
                    );
                    if ui
                        .link(name)
                        .on_hover_text(path.display().to_string())
                        .clicked()
                    {
                        actions.push(Action::OpenProject(path.clone()));
                    }
                }
            }
            if let Some(notice) = &app.notice {
                ui.add_space(24.0);
                ui.label(notice);
            }
        });
    });
}

fn workspace(
    ui: &mut egui::Ui,
    app: &App,
    open: &Open,
    view: &mut View,
    env: &mut dyn Env,
    actions: &mut Vec<Action>,
) {
    let exporting = matches!(open.export, ExportState::Running { .. });
    // While exporting, show progress in the window title too (visible even when minimized).
    let title = match &open.export {
        ExportState::Running {
            total,
            report: Some(report),
        } => {
            format!(
                "{:.0}% · {} · {}",
                report.progress / (*total).max(1) as f64 * 100.0,
                open.project.name,
                app_name()
            )
        }
        _ => format!("{} · {}", open.project.name, app_name()),
    };
    // Send only when the title changes. Sending every time keeps the screen redrawing nonstop.
    if view.title != title {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
        view.title = title;
    }

    Panel::top("top").show(ui, |ui| {
        // Wrap to the next line when the window is narrow (forcing one line makes the buttons
        // overlap).
        ui.horizontal_wrapped(|ui| {
            ui.strong(&open.project.name);
            ui.separator();
            if ui.button(t("폴더 가져오기", "Import folder")).clicked()
                && let Some(folder) = env.pick_folder()
            {
                actions.push(Action::ImportFolder(folder));
            }
            if ui
                .button(t("스크린샷", "Screenshot"))
                .on_hover_text(t(
                    "지금 창을 PNG로 저장 (F12)",
                    "Save this window as a PNG (F12)",
                ))
                .clicked()
            {
                env.screenshot(ui.ctx());
            }
            if ui
                .add_enabled(
                    !exporting,
                    egui::Button::new(t("프로젝트 닫기", "Close project")),
                )
                .clicked()
            {
                actions.push(Action::CloseProject);
            }
            ui.separator();
            backend_label(ui, app, view);
            ai_info_button(ui, view);
            ui.separator();
            log_button(ui, env, actions);
            ui.separator();
            theme_buttons(ui, actions);
            ui.separator();
            language_buttons(ui, actions);
        });
    });

    Panel::bottom("status").show(ui, |ui| status_bar(ui, app, open));
    Panel::bottom("export").show(ui, |ui| export_bar(ui, open, view, env, actions));

    // Move the edit panel to the bottom when the window is tall.
    let portrait = ui.available_width() < ui.available_height();
    if portrait {
        Panel::bottom("edit")
            .resizable(true)
            .default_size(ui.available_height() * 0.35)
            .show(ui, |ui| edit_panel(ui, app, open, env, actions));
    } else {
        Panel::right("edit")
            .default_size(240.0)
            .show(ui, |ui| edit_panel(ui, app, open, env, actions));
    }
    Panel::left("photos")
        .default_size(if portrait { 160.0 } else { 220.0 })
        .show(ui, |ui| {
            photo_list(ui, open, view, actions);
        });
    egui::CentralPanel::default().show(ui, |ui| preview(ui, app, open, view, actions));

    export_windows(ui.ctx(), open, actions);
    if view.show_backend_help {
        backend_help(ui.ctx(), app, view);
    }
}

fn backend_label(ui: &mut egui::Ui, app: &App, view: &mut View) {
    match &app.backend {
        Backend::Unknown => {
            ui.weak(t("AI 준비 중", "Preparing AI"));
        }
        Backend::Gpu => {
            ui.label("GPU").on_hover_text(t(
                "AI 노이즈 제거를 GPU로 실행합니다",
                "AI noise reduction runs on the GPU",
            ));
        }
        Backend::Cpu(_) => {
            if ui
                .link(t("CPU (느림)", "CPU (slow)"))
                .on_hover_text(t("눌러서 해결 방법 보기", "Click to see how to fix this"))
                .clicked()
            {
                view.show_backend_help = true;
            }
        }
        Backend::Unavailable(_) => {
            if ui
                .link(t("AI 모델 없음", "No AI model"))
                .on_hover_text(t("눌러서 해결 방법 보기", "Click to see how to fix this"))
                .clicked()
            {
                view.show_backend_help = true;
            }
        }
    }
}

fn backend_help(ctx: &egui::Context, app: &App, view: &mut View) {
    let text = match &app.backend {
        Backend::Cpu(notice) | Backend::Unavailable(notice) => notice.to_string(),
        _ => t(
            "AI 노이즈 제거를 GPU로 실행합니다.",
            "AI noise reduction runs on the GPU.",
        )
        .to_owned(),
    };
    let mut open = true;
    egui::Window::new(t("AI 실행 환경", "AI backend"))
        .open(&mut open)
        .collapsible(false)
        .show(ctx, |ui| {
            ui.set_max_width(520.0);
            ui.label(text);
        });
    view.show_backend_help = open;
}

fn status_bar(ui: &mut egui::Ui, app: &App, open: &Open) {
    ui.horizontal(|ui| {
        match &open.export {
            ExportState::Running {
                report: Some(report),
                ..
            } => {
                ui.label(resources(report));
            }
            _ => match &app.notice {
                Some(notice) => {
                    ui.label(notice);
                }
                None => {
                    let (photos, selected) = (open.photos.len(), open.selected.len());
                    ui.weak(t(
                        format!("사진 {photos}장 · 선택 {selected}장"),
                        format!("Photos: {photos} · Selected: {selected}"),
                    ));
                }
            },
        }
        if let ExportState::Running { total, report } = &open.export {
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (finished, progress, text) = match report {
                    Some(report) => (
                        report.finished,
                        report.progress,
                        describe_remaining(report.remaining, report.idle),
                    ),
                    None => (0, 0.0, t("준비 중", "Preparing").to_string()),
                };
                ui.label(format!("{finished}/{total} · {text}"));
                ui.add(
                    egui::ProgressBar::new((progress / (*total).max(1) as f64) as f32)
                        .desired_width(220.0),
                );
            });
        }
    });
}

fn resources(report: &ReportView) -> String {
    describe_resources(
        report.cpu,
        report.memory,
        report.memory_limit,
        report.gpu,
        report.running,
        report.low_memory,
    )
}

/// What the chosen quality means, in a word. Based on file sizes measured on a 24-megapixel photo.
fn quality_hint(quality: u8) -> &'static str {
    match quality {
        ..=60 => t("권장 범위", "Recommended"),
        61..=75 => t("용량이 2~3배", "2–3× the size"),
        _ => t(
            "16MB를 넘으면 자동으로 낮춤",
            "Lowered automatically above 16 MB",
        ),
    }
}

fn export_bar(
    ui: &mut egui::Ui,
    open: &Open,
    view: &mut View,
    env: &mut dyn Env,
    actions: &mut Vec<Action>,
) {
    let exporting = matches!(open.export, ExportState::Running { .. });
    // Wrap to the next line when the window is narrow.
    ui.horizontal_wrapped(|ui| {
        if ui
            .add_enabled(
                !exporting,
                egui::Button::new(t("출력 폴더…", "Output folder…")),
            )
            .clicked()
            && let Some(folder) = env.pick_folder()
        {
            actions.push(Action::SetExportFolder(folder));
        }
        match &open.project.export.folder {
            Some(folder) => ui.label(folder.display().to_string()),
            None => ui.weak(t("아직 정하지 않음", "Not chosen yet")),
        };
        ui.separator();
        let mut quality = view.quality.unwrap_or(open.project.export.quality);
        // Let quality go all the way up, and say next to it what each value costs.
        let guide = t(
            "55를 권장합니다 (JPEG 90 수준의 화질).\n\
             75면 용량이 55의 2~3배가 되어 카메라 JPEG보다 커집니다.\n\
             그보다 높이면, 16MB를 넘는 사진은 열지 못하는 뷰어가 있어 품질을 자동으로 낮춥니다. \
             낮추는 만큼 다시 인코딩하므로 오래 걸립니다.\n\
             95 이상은 결과가 같습니다.",
            "55 is recommended (about the quality of JPEG 90).\n\
             At 75 files are 2–3 times the size at 55, larger than the camera JPEG.\n\
             Above that, photos over 16 MB cannot be opened by some viewers, so their quality is \
             lowered automatically. Each step down is another encode, so it takes longer.\n\
             95 and above give the same result.",
        );
        // Name the slider (screen readers and tests find it by this name).
        let slider = ui
            .add_enabled(
                !exporting,
                egui::Slider::new(&mut quality, crate::export::QUALITY_RANGE)
                    .text(t("품질", "Quality")),
            )
            .on_hover_text(guide);
        // Keep the short hint from splitting over two lines (if space runs out, it moves whole to
        // the next line).
        ui.add(
            egui::Label::new(RichText::new(quality_hint(quality)).weak())
                .wrap_mode(egui::TextWrapMode::Extend),
        )
        .on_hover_text(guide);
        // While dragging, change it only on screen; save once on release.
        if slider.dragged() {
            view.quality = Some(quality);
        } else if slider.drag_stopped() || slider.changed() {
            view.quality = None;
            actions.push(Action::SetQuality(quality));
        }
        ui.separator();
        if exporting {
            if ui.button(t("취소", "Cancel")).clicked() {
                actions.push(Action::CancelExport);
            }
        } else {
            if ui
                .add_enabled(
                    !open.selected.is_empty(),
                    egui::Button::new(t("선택 내보내기", "Export selected")),
                )
                .clicked()
            {
                actions.push(Action::Export {
                    only_selected: true,
                });
            }
            if ui.button(t("전체 내보내기", "Export all")).clicked() {
                actions.push(Action::Export {
                    only_selected: false,
                });
            }
        }
    });
}

fn edit_panel(
    ui: &mut egui::Ui,
    app: &App,
    open: &Open,
    env: &mut dyn Env,
    actions: &mut Vec<Action>,
) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            look_panel(ui, app, open, env, actions);
            ui.separator();
            mask_panel(ui, app, open, actions);
            ui.separator();
            skin_panel(ui, app, open, actions);
            ui.separator();
            frame_panel(ui, open, actions);
            ui.separator();
            capture_panel(ui, open, actions);
            ui.separator();
            denoise_panel(ui, app, open, actions);
        });
}

/// One slider. Returns `Some(released)` when the value changes: false while dragging, true on
/// release.
fn slider(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut f32,
    range: std::ops::RangeInclusive<f32>,
) -> Option<bool> {
    let response = ui.add(
        egui::Slider::new(value, range)
            .text(label)
            .fixed_decimals(2),
    );
    commit_of(&response)
}

/// `Some(released)` if the slider changed: false while dragging, true on release.
fn commit_of(response: &egui::Response) -> Option<bool> {
    if response.drag_stopped() {
        Some(true)
    } else if response.changed() {
        // Changes made without dragging (typing a number, keyboard) are committed right away.
        Some(!response.dragged())
    } else {
        None
    }
}

/// Capture sharpening: undoes the blur in the RAW. Applies to all selected photos.
fn capture_panel(ui: &mut egui::Ui, open: &Open, actions: &mut Vec<Action>) {
    ui.heading(t("캡처 샤픈", "Capture sharpening"));
    ui.weak(t(
        "렌즈와 센서 앞 필터 때문에 RAW에 생긴 흐림을 되돌립니다 (RawTherapee 방식).",
        "Undoes the blur the lens and the filter in front of the sensor leave in the RAW (RawTherapee's method).",
    ));
    let Some(current) = open.current else {
        ui.weak(t("사진을 고르세요", "Pick a photo"));
        return;
    };
    let before = open.edit(current).capture;
    let mut capture = before;
    let mut commit = true;
    // Radius and threshold found for the current photo (reported by the worker)
    let auto = open
        .capture_auto
        .filter(|(index, _, _)| *index == current)
        .map(|(_, radius, threshold)| (radius, threshold));

    ui.checkbox(
        &mut capture.enabled,
        t("캡처 샤픈 켜기", "Use capture sharpening"),
    );
    ui.add_enabled_ui(capture.enabled, |ui| {
        let response = ui
            .add(
                egui::Slider::new(&mut capture.iterations, crate::capture::ITERATIONS)
                    .text(t("반복 횟수", "Iterations")),
            )
            .on_hover_text(t(
                "반복할수록 흐림을 더 많이 되돌립니다",
                "More iterations undo more of the blur",
            ));
        if let Some(done) = commit_of(&response) {
            commit = done;
        }
        ui.weak(match capture.iterations {
            0..=20 => t("기본 (RawTherapee는 20)", "Default (RawTherapee uses 20)"),
            21..=50 => t("강하게", "Stronger"),
            _ => t(
                "아주 강하게: 경계에 넘침이 생길 수 있습니다",
                "Very strong: edges may overshoot",
            ),
        });
        ui.collapsing(t("세부 설정", "More settings"), |ui| {
            // Radius: when automatic, show the value found from the photo. Turning auto off starts
            // from RawTherapee's default of 0.75.
            let mut auto_radius = capture.radius.is_none();
            if ui
                .checkbox(&mut auto_radius, t("반경 자동", "Auto radius"))
                .changed()
            {
                capture.radius = (!auto_radius).then_some(0.75);
            }
            match capture.radius.as_mut() {
                Some(radius) => {
                    if let Some(done) =
                        slider(ui, t("반경", "Radius"), radius, crate::capture::RADIUS)
                    {
                        commit = done;
                    }
                }
                None => {
                    if let Some((radius, _)) = auto {
                        ui.weak(t(
                            format!("자동: {radius:.2}"),
                            format!("Auto: {radius:.2}"),
                        ));
                    }
                }
            }

            // Contrast threshold: the default is fixed at 0.4. When automatic, it is found from the
            // photo like RawTherapee does.
            let mut auto_threshold = capture.threshold.is_none();
            if ui
                .checkbox(&mut auto_threshold, t("문턱 자동", "Auto threshold"))
                .changed()
            {
                capture.threshold = (!auto_threshold).then_some(crate::capture::DEFAULT_THRESHOLD);
            }
            match capture.threshold.as_mut() {
                Some(threshold) => {
                    let label = t("대비 문턱", "Contrast threshold");
                    if let Some(done) = slider(ui, label, threshold, crate::capture::THRESHOLD) {
                        commit = done;
                    }
                }
                None => {
                    if let Some((_, threshold)) = auto {
                        ui.weak(t(
                            format!("자동: {threshold:.2}"),
                            format!("Auto: {threshold:.2}"),
                        ));
                    }
                }
            }
            ui.weak(t(
                "문턱이 높을수록 뚜렷한 경계만 샤픈합니다",
                "A higher threshold sharpens only clear edges",
            ));

            let label = t("모서리 반경", "Corner radius");
            if let Some(done) = slider(ui, label, &mut capture.corner, crate::capture::CORNER) {
                commit = done;
            }
            ui.checkbox(
                &mut capture.auto_stop,
                t("넘치면 멈추기", "Stop when it overshoots"),
            )
            .on_hover_text(t(
                "경계가 넘치기 시작한 곳은 반복을 멈춥니다",
                "Stops iterating where edges start to overshoot",
            ));
        });
    });
    ui.add_enabled_ui(capture.enabled, |ui| {
        ui.horizontal_wrapped(|ui| {
            ui.label(t("샤픈 영역 보기", "Show sharpened area"));
            for (view, label) in [
                (None, t("안 보기", "Hide")),
                (Some(CaptureView::Overlay), t("빨간 표시", "Red overlay")),
                (
                    Some(CaptureView::Map),
                    t("흑백 지도", "Black-and-white map"),
                ),
            ] {
                if ui
                    .selectable_label(open.capture_view == view, label)
                    .clicked()
                {
                    actions.push(Action::SetCaptureView(view));
                }
            }
        });
    });
    ui.weak(t(
        "선예도는 100%로 확대해서 보세요",
        "Judge sharpness at 100% zoom",
    ));
    if capture != before {
        actions.push(Action::SetCapture {
            value: capture,
            commit,
        });
    }
}

/// Presets, strength, fine adjustments, LUT.
fn look_panel(
    ui: &mut egui::Ui,
    app: &App,
    open: &Open,
    env: &mut dyn Env,
    actions: &mut Vec<Action>,
) {
    ui.heading(t("프리셋", "Presets"));
    let Some(current) = open.current else {
        ui.weak(t(
            "사진을 고르면 프리셋을 적용할 수 있습니다",
            "Pick a photo to apply a preset",
        ));
        return;
    };
    let edit = open.edit(current);
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(edit.preset.is_none(), t("없음", "None"))
            .clicked()
        {
            actions.push(Action::SetPreset(None));
        }
        // Star the presets that fit the current photo's content.
        let recommended = open.photos[current]
            .guess
            .and_then(|guess| recommend::preset_for(&app.presets, &guess));
        for preset in &app.presets {
            let chosen = edit.preset.as_deref() == Some(preset.id.as_str());
            let name = if recommended.is_some_and(|best| best.id == preset.id) {
                format!("{} ★", preset.label())
            } else {
                preset.label().to_owned()
            };
            if ui.selectable_label(chosen, name).clicked() {
                actions.push(Action::SetPreset(Some(preset.id.clone())));
            }
        }
    });
    if open
        .selected
        .iter()
        .any(|&index| open.photos[index].guess.is_some())
    {
        if ui
            .button(t("추천 프리셋 적용", "Apply recommended presets"))
            .on_hover_text(t(
                "선택한 사진마다 내용(음식, 인물, 풍경, 야경)에 맞는 프리셋을 적용합니다. 확실하지 않은 사진은 프리셋 없이 둡니다.",
                "Applies the preset that suits each selected photo (food, portrait, landscape, night). Photos it is not sure about get no preset.",
            ))
            .clicked()
        {
            actions.push(Action::ApplyRecommended);
        }
    } else if let Some(problem) = &app.recommend_problem {
        ui.weak(t("프리셋 추천: 모델 없음", "Preset suggestions: no model"))
            .on_hover_text(problem.to_string());
    }
    credit_line(ui, t("추천", "Suggestions"), &credits::RECOMMEND);
    ui.add_enabled_ui(edit.preset.is_some(), |ui| {
        let mut strength = edit.strength;
        if let Some(commit) = slider(ui, t("강도", "Strength"), &mut strength, 0.0..=1.5) {
            actions.push(Action::SetStrength {
                value: strength,
                commit,
            });
        }
    });
    ui.collapsing(t("자세히", "Details"), |ui| {
        let mut adjust = edit.adjust;
        let mut changed = None;
        let mut row = |ui: &mut egui::Ui, label: &str, value: &mut f32, range| {
            if let Some(commit) = slider(ui, label, value, range) {
                changed = Some(commit);
            }
        };
        row(ui, t("노출", "Exposure"), &mut adjust.exposure, -3.0..=3.0);
        row(ui, t("대비", "Contrast"), &mut adjust.contrast, -1.0..=1.0);
        row(
            ui,
            t("하이라이트", "Highlights"),
            &mut adjust.highlights,
            -1.0..=1.0,
        );
        row(ui, t("그림자", "Shadows"), &mut adjust.shadows, -1.0..=1.0);
        row(ui, t("색온도", "Temperature"), &mut adjust.temp, -1.0..=1.0);
        row(ui, t("틴트", "Tint"), &mut adjust.tint, -1.0..=1.0);
        row(
            ui,
            t("채도", "Saturation"),
            &mut adjust.saturation,
            -1.0..=1.0,
        );
        row(
            ui,
            t("생동감", "Vibrance"),
            &mut adjust.vibrance,
            -1.0..=1.0,
        );
        row(ui, t("선명도", "Clarity"), &mut adjust.clarity, -1.0..=1.0);
        row(ui, t("샤픈", "Sharpen"), &mut adjust.sharpen, -1.0..=1.0);
        row(
            ui,
            t("비네팅", "Vignette"),
            &mut adjust.vignette,
            -1.0..=1.0,
        );
        row(ui, t("흑백", "Monochrome"), &mut adjust.mono, 0.0..=1.0);
        ui.collapsing(t("색상별", "By colour"), |ui| {
            let hsl = &mut adjust.hsl;
            let bands = [
                &mut hsl.red,
                &mut hsl.orange,
                &mut hsl.yellow,
                &mut hsl.green,
                &mut hsl.aqua,
                &mut hsl.blue,
                &mut hsl.purple,
                &mut hsl.magenta,
            ];
            for (name, band) in hue_names().into_iter().zip(bands) {
                let sat = t(format!("{name} 채도"), format!("{name} sat"));
                let lum = t(format!("{name} 밝기"), format!("{name} lum"));
                row(ui, &sat, &mut band.sat, -1.0..=1.0);
                row(ui, &lum, &mut band.lum, -1.0..=1.0);
            }
        });
        if let Some(commit) = changed {
            actions.push(Action::SetAdjust {
                params: adjust,
                commit,
            });
        }
    });
    ui.horizontal_wrapped(|ui| {
        if ui
            .button(t("LUT 불러오기…", "Load LUT…"))
            .on_hover_text(t(
                "3D LUT(.cube) 파일을 보정 뒤에 입힙니다",
                "Applies a 3D LUT (.cube) file after the adjustments",
            ))
            .clicked()
            && let Some(file) = env.pick_lut()
        {
            actions.push(Action::SetLut(Some(file)));
        }
        if let Some(lut) = &edit.lut {
            let name = lut.file.file_name().unwrap_or(lut.file.as_os_str());
            ui.label(name.to_string_lossy())
                .on_hover_text(lut.file.display().to_string());
            if ui.button(t("LUT 없애기", "Remove LUT")).clicked() {
                actions.push(Action::SetLut(None));
            }
        }
    });
    if let Some(lut) = &edit.lut {
        let mut strength = lut.strength;
        if let Some(commit) = slider(ui, t("LUT 강도", "LUT strength"), &mut strength, 0.0..=1.0)
        {
            actions.push(Action::SetLutStrength {
                value: strength,
                commit,
            });
        }
    }
    if ui
        .button(t("보정 초기화", "Reset adjustments"))
        .on_hover_text(t(
            "프리셋, 세부 조정, LUT, 피부 보정을 지웁니다 (노이즈 제거, 프레임, 마스크는 그대로)",
            "Clears the preset, detail adjustments, LUT and skin retouch (noise reduction, frame and masks stay)",
        ))
        .clicked()
    {
        actions.push(Action::ResetLook);
    }
}

/// Display names of the hue bands. Same order as the entries of `edit::Hsl`.
fn hue_names() -> [&'static str; 8] {
    t(
        [
            "빨강", "주황", "노랑", "초록", "청록", "파랑", "보라", "자홍",
        ],
        [
            "Red", "Orange", "Yellow", "Green", "Aqua", "Blue", "Purple", "Magenta",
        ],
    )
}

/// Skin retouching (experimental): finds only the subject's skin and softens blemishes.
fn skin_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.heading(t("피부 보정 (실험)", "Skin retouch (experimental)"));
    credit_line(ui, t("피부 찾기", "Skin finder"), &credits::SKIN);
    let Some(current) = open.current else {
        ui.weak(t(
            "사진을 고르면 인물의 피부를 다듬을 수 있습니다",
            "Pick a photo to retouch skin in portraits",
        ));
        return;
    };
    if let Some(problem) = &app.skin_problem {
        ui.weak(t("피부 보정: 모델 없음", "Skin retouch: no model"))
            .on_hover_text(problem.to_string());
        return;
    }
    let mut amount = open.edit(current).skin;
    if let Some(commit) = slider(
        ui,
        t("피부 보정 세기", "Skin amount"),
        &mut amount,
        0.0..=1.0,
    ) {
        actions.push(Action::SetSkin {
            value: amount,
            commit,
        });
    }
    ui.weak(t(
        "피부를 자동으로 찾아 얼룩과 고르지 않은 톤만 줄입니다. 가까이서 찍은 인물에 잘 듣습니다.",
        "Finds skin automatically and evens out blotches. Works best on people photographed up close.",
    ));
}

/// Masks: pick part of the photo and add adjustments only there.
fn mask_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.heading(t("마스크", "Masks"));
    credit_line(ui, t("모델", "Model"), &credits::MASK);
    let Some(current) = open.current else {
        ui.weak(t(
            "사진을 고르면 일부분만 따로 보정할 수 있습니다",
            "Pick a photo to adjust just a part of it",
        ));
        return;
    };
    if let Some(problem) = &app.mask_problem {
        ui.weak(t("마스크: 모델 없음", "Masks: no model"))
            .on_hover_text(problem.to_string());
        return;
    }
    let masks = open.edit(current).masks;
    ui.horizontal_wrapped(|ui| {
        for index in 0..masks.len() {
            let number = index + 1;
            let name = t(format!("마스크 {number}"), format!("Mask {number}"));
            let active = open.mask == Some(index);
            if ui.selectable_label(active, name).clicked() {
                // Clicking the mask being edited again finishes editing it.
                actions.push(Action::SelectMask((!active).then_some(index)));
            }
        }
        if ui.button(t("새 마스크", "New mask")).clicked() {
            actions.push(Action::AddMask);
        }
    });
    let Some(mask) = open.mask.and_then(|index| masks.get(index)) else {
        return;
    };
    ui.weak(t(
        "사진에서 고를 곳을 누르세요. 빼려면 오른쪽 단추로 누릅니다.",
        "Click the part of the photo to select. Right-click a part to leave it out.",
    ));
    ui.horizontal_wrapped(|ui| {
        let count = mask.points.len();
        ui.label(t(format!("점 {count}개"), format!("Points: {count}")));
        if ui
            .add_enabled(count > 0, egui::Button::new(t("점 되돌리기", "Undo point")))
            .clicked()
        {
            actions.push(Action::UndoMaskPoint);
        }
        if ui.button(t("마스크 지우기", "Delete mask")).clicked() {
            actions.push(Action::RemoveMask);
        }
    });
    ui.horizontal_wrapped(|ui| {
        let mut show = open.show_mask;
        if ui
            .checkbox(&mut show, t("영역 보기", "Show area"))
            .on_hover_text(t(
                "고른 곳을 붉게 표시합니다",
                "Marks the selected part in red",
            ))
            .changed()
        {
            actions.push(Action::ShowMask(show));
        }
        let mut invert = mask.invert;
        if ui
            .checkbox(&mut invert, t("반전", "Invert"))
            .on_hover_text(t(
                "고른 곳의 바깥을 보정합니다",
                "Adjusts everything outside the selected part",
            ))
            .changed()
        {
            actions.push(Action::SetMaskInvert(invert));
        }
    });
    // Adjustments added only to the picked area
    let mut adjust = mask.adjust;
    let mut changed = None;
    let rows = [
        (t("노출", "Exposure"), &mut adjust.exposure, -3.0..=3.0),
        (t("대비", "Contrast"), &mut adjust.contrast, -1.0..=1.0),
        (t("색온도", "Temperature"), &mut adjust.temp, -1.0..=1.0),
        (t("틴트", "Tint"), &mut adjust.tint, -1.0..=1.0),
        (t("채도", "Saturation"), &mut adjust.saturation, -1.0..=1.0),
    ];
    for (label, value, range) in rows {
        if let Some(commit) = slider(ui, label, value, range) {
            changed = Some(commit);
        }
    }
    if let Some(commit) = changed {
        actions.push(Action::SetMaskAdjust {
            params: adjust,
            commit,
        });
    }
}

/// Where in the photo a click on the preview landed. The photo's top left is (0, 0), bottom right
/// (1, 1).
/// `rect` is where the photo is drawn on screen. None if the click was outside the photo.
fn photo_point(rect: egui::Rect, pos: egui::Pos2) -> Option<(f32, f32)> {
    rect.contains(pos).then(|| {
        (
            (pos.x - rect.min.x) / rect.width(),
            (pos.y - rect.min.y) / rect.height(),
        )
    })
}

/// Frame layout and colour.
fn frame_panel(ui: &mut egui::Ui, open: &Open, actions: &mut Vec<Action>) {
    ui.heading(t("프레임", "Frame"));
    let Some(current) = open.current else {
        ui.weak(t(
            "사진을 고르면 촬영 정보를 적은 프레임을 두를 수 있습니다",
            "Pick a photo to add a frame with its shooting info",
        ));
        return;
    };
    let frame = open.edit(current).frame;
    let dark = frame.is_some_and(|frame| frame.dark);
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(frame.is_none(), t("프레임 없음", "No frame"))
            .clicked()
        {
            actions.push(Action::SetFrame(None));
        }
        let layouts = [
            (Shape::Band, t("하단 띠", "Bottom band").to_owned()),
            (Shape::Border, t("테두리", "Border").to_owned()),
            (Shape::Ratio(1, 1), "1:1".to_owned()),
            (Shape::Ratio(4, 5), "4:5".to_owned()),
            (Shape::Ratio(9, 16), "9:16".to_owned()),
        ];
        for (layout, name) in layouts {
            let chosen = frame.is_some_and(|frame| frame.layout == layout);
            if ui.selectable_label(chosen, name).clicked() {
                actions.push(Action::SetFrame(Some(Frame { layout, dark })));
            }
        }
    });
    if let Some(frame) = frame {
        ui.horizontal(|ui| {
            for (choice, name) in [(false, t("흰색", "White")), (true, t("검정", "Black"))] {
                if ui.selectable_label(frame.dark == choice, name).clicked() {
                    actions.push(Action::SetFrame(Some(Frame {
                        dark: choice,
                        ..frame
                    })));
                }
            }
        });
    }
}

fn denoise_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.heading(t("노이즈 제거", "Noise reduction"));
    credit_line(ui, t("모델", "Model"), &credits::DENOISE);
    let usable = matches!(app.backend, Backend::Gpu | Backend::Cpu(_));
    ui.add_enabled_ui(usable && !open.selected.is_empty(), |ui| {
        let mut percent = (open.denoise_level * 100.0).round();
        if ui
            .add(
                egui::Slider::new(&mut percent, 0.0..=100.0)
                    .fixed_decimals(0)
                    .suffix("%")
                    .text(t("레벨", "Level")),
            )
            .changed()
        {
            actions.push(Action::SetDenoiseLevel(percent / 100.0));
        }
        if ui.button(t("적용", "Apply")).clicked() {
            actions.push(Action::ApplyDenoise);
        }
    });
    match (&app.backend, open.selected.len()) {
        (Backend::Unavailable(_), _) => ui.weak(t(
            "AI 모델이 없어 쓸 수 없습니다",
            "Unavailable: the AI model is missing",
        )),
        (Backend::Unknown, _) => ui.weak(t("AI 준비 중", "Preparing AI")),
        (_, 0) => ui.weak(t("사진을 고르세요", "Pick a photo")),
        (_, count) => ui.weak(t(
            format!("선택한 {count}장에 적용됩니다"),
            format!("Applies to the {count} selected"),
        )),
    };
    if let Some(current) = open.current {
        let level = open.edit(current).denoise;
        let percent = level * 100.0;
        ui.weak(t(
            format!("지금 사진: {percent:.0}%"),
            format!("This photo: {percent:.0}%"),
        ));
    }
}

/// Row height of the photo list.
const ROW: f32 = 56.0;

fn photo_list(ui: &mut egui::Ui, open: &Open, view: &mut View, actions: &mut Vec<Action>) {
    if open.photos.is_empty() {
        ui.weak(t(
            "[폴더 가져오기]로 사진 폴더를 추가하세요",
            "Add a photo folder with [Import folder]",
        ));
        return;
    }
    // Up and down arrows go to the previous and next photo.
    let step = ui.input(|input| {
        input.key_pressed(Key::ArrowDown) as isize - input.key_pressed(Key::ArrowUp) as isize
    });
    if step != 0 {
        let next = open.current.map_or(0, |current| current as isize + step);
        if (0..open.photos.len() as isize).contains(&next) {
            actions.push(Action::Select {
                index: next as usize,
                extend: false,
            });
        }
    }
    if ui.input(|input| input.modifiers.command && input.key_pressed(Key::A)) {
        actions.push(Action::SelectAll);
    }
    let extend = ui.input(|input| input.modifiers.command || input.modifiers.shift);
    egui::ScrollArea::vertical().auto_shrink(false).show_rows(
        ui,
        ROW,
        open.photos.len(),
        |ui, rows| {
            for index in rows {
                let photo = &open.photos[index];
                let clicked = ui
                    .horizontal(|ui| {
                        ui.set_height(ROW);
                        let (rect, image) =
                            ui.allocate_exact_size(egui::vec2(ROW, ROW), egui::Sense::click());
                        if let Some(texture) =
                            thumbnail(ui.ctx(), view, index, photo.thumbnail.as_ref())
                        {
                            let size = fit(texture.size_vec2(), rect.size());
                            egui::Image::new(&texture)
                                .paint_at(ui, egui::Rect::from_center_size(rect.center(), size));
                        }
                        let name = photo.relative.rsplit('/').next().unwrap_or(&photo.relative);
                        // Note next to the name what the photo appears to show (only when sure).
                        let name = match photo.guess.filter(|guess| guess.is_confident()) {
                            Some(guess) => format!("{name} · {}", guess.category.label()),
                            None => name.to_owned(),
                        };
                        let label = ui.selectable_label(open.selected.contains(&index), name);
                        image.clicked() || label.clicked()
                    })
                    .inner;
                if clicked {
                    actions.push(Action::Select { index, extend });
                }
            }
        },
    );
}

fn thumbnail(
    ctx: &egui::Context,
    view: &mut View,
    index: usize,
    pixels: Option<&Pixels>,
) -> Option<TextureHandle> {
    if let Some(texture) = view.thumbnails.get(&index) {
        return Some(texture.clone());
    }
    let texture = ctx.load_texture(
        format!("thumb-{index}"),
        color_image(pixels?),
        TextureOptions::LINEAR,
    );
    view.thumbnails.insert(index, texture.clone());
    Some(texture)
}

/// Zoom state of the preview.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// 1 is the fit-to-window size, 2 is twice that.
    pub scale: f32,
    /// How far the image centre is from the centre of the preview area (screen units).
    pub offset: egui::Vec2,
}

impl Default for Zoom {
    fn default() -> Self {
        Self {
            scale: 1.0,
            offset: egui::Vec2::ZERO,
        }
    }
}

impl Zoom {
    /// Zooms by `factor`, keeping the image point under `pointer` (position relative to the area
    /// centre) in place.
    /// `max` is the maximum zoom.
    pub fn zoom_at(&mut self, factor: f32, pointer: egui::Vec2, max: f32) {
        let scale = (self.scale * factor).clamp(1.0, max.max(1.0));
        let applied = scale / self.scale;
        self.offset = pointer - (pointer - self.offset) * applied;
        self.scale = scale;
    }

    /// If the image is larger than the area, keeps its edges from coming inside the area; if
    /// smaller, centres it.
    /// `fit` is the fit-to-window image size, `area` the size of the preview area.
    pub fn clamp(&mut self, fit: egui::Vec2, area: egui::Vec2) {
        let limit = ((fit * self.scale - area) / 2.0).max(egui::Vec2::ZERO);
        self.offset = self.offset.clamp(-limit, limit);
    }
}

/// Before/after comparison buttons over the preview. With comparison on, the split direction can be
/// chosen too.
fn compare_bar(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(open.compare, t("비교", "Compare"))
            .on_hover_text(t(
                "보정 전과 보정 후를 나란히 본다. 한쪽을 확대하거나 옮기면 다른 쪽도 같이 움직인다.",
                "Show the photo before and after editing side by side. Zooming or moving one side moves both.",
            ))
            .clicked()
        {
            actions.push(Action::SetCompare(!open.compare));
        }
        if open.compare {
            ui.separator();
            let choices = [
                (Split::Auto, t("자동", "Auto")),
                (Split::Side, t("좌우", "Side by side")),
                (Split::Stacked, t("위아래", "Top and bottom")),
            ];
            for (choice, name) in choices {
                let chosen = app.config.compare_split == choice;
                if ui.selectable_label(chosen, name).clicked() && !chosen {
                    actions.push(Action::SetCompareSplit(choice));
                }
            }
            ui.separator();
            // What to show in the before pane. Not selectable if the current photo has no JPEG with
            // the same name.
            let has_jpeg = open
                .sidecar
                .as_ref()
                .is_some_and(|(index, jpeg)| Some(*index) == open.current && jpeg.is_some());
            let raw = app.config.compare_before == Before::Raw;
            if ui
                .selectable_label(raw, t("보정 전 RAW", "Unedited RAW"))
                .clicked()
                && !raw
            {
                actions.push(Action::SetCompareBefore(Before::Raw));
            }
            let response = ui
                .add_enabled(
                    has_jpeg,
                    egui::Button::selectable(!raw, t("카메라 JPEG", "Camera JPEG")),
                )
                .on_hover_text(t(
                    "카메라가 함께 저장한 JPEG와 견준다",
                    "Compare with the JPEG the camera saved alongside",
                ))
                .on_disabled_hover_text(t(
                    "같은 폴더에 같은 이름의 JPEG가 없습니다",
                    "There is no JPEG with the same name in the folder",
                ));
            if response.clicked() && raw {
                actions.push(Action::SetCompareBefore(Before::Jpeg));
            }
        }
    });
}

/// Uploads the preview image to the GPU.
fn upload(ctx: &egui::Context, name: &str, pixels: &Pixels) -> TextureHandle {
    // Smooth (mipmaps) when viewed scaled down; show pixels as they are when viewed above 100%.
    let options = TextureOptions {
        magnification: egui::TextureFilter::Nearest,
        minification: egui::TextureFilter::Linear,
        mipmap_mode: Some(egui::TextureFilter::Linear),
        ..TextureOptions::LINEAR
    };
    // Scale down before uploading if it exceeds what the GPU accepts (uploading an oversized
    // texture crashes the program).
    let limit = ctx.input(|input| input.max_texture_side);
    let image = match pixels.fit_within(limit) {
        Some(smaller) => color_image(&smaller),
        None => color_image(pixels),
    };
    ctx.load_texture(name, image, options)
}

/// Splits `area` into two equal panes (before, after) for the before/after comparison. `image` is
/// the photo's size.
/// When automatic, picks whichever of side by side or top and bottom shows the photo larger. Side
/// by side on a tie.
fn compare_panes(split: Split, image: egui::Vec2, area: egui::Rect) -> [egui::Rect; 2] {
    let width = ((area.width() - PANE_GAP) / 2.0).max(0.0);
    let side = [
        egui::Rect::from_min_size(area.min, egui::vec2(width, area.height())),
        egui::Rect::from_min_max(egui::pos2(area.max.x - width, area.min.y), area.max),
    ];
    let height = ((area.height() - PANE_GAP) / 2.0).max(0.0);
    let stacked = [
        egui::Rect::from_min_size(area.min, egui::vec2(area.width(), height)),
        egui::Rect::from_min_max(egui::pos2(area.min.x, area.max.y - height), area.max),
    ];
    let side_by_side = match split {
        Split::Side => true,
        Split::Stacked => false,
        Split::Auto => fit(image, side[0].size()).x >= fit(image, stacked[0].size()).x,
    };
    if side_by_side { side } else { stacked }
}

fn preview(ui: &mut egui::Ui, app: &App, open: &Open, view: &mut View, actions: &mut Vec<Action>) {
    let Some(current) = open.current else {
        ui.centered_and_justified(|ui| {
            ui.weak(t("왼쪽에서 사진을 고르세요", "Pick a photo on the left"))
        });
        return;
    };
    compare_bar(ui, app, open, actions);
    let shown = open
        .preview
        .as_ref()
        .filter(|preview| preview.index == current);
    if let Some(preview) = shown
        && view
            .preview
            .as_ref()
            .is_none_or(|(serial, _)| *serial != preview.serial)
    {
        view.preview = Some((preview.serial, upload(ui.ctx(), "preview", &preview.pixels)));
    }
    // Use the before image only while comparing and only if it belongs to the current photo.
    // Turning comparison off also releases the texture.
    let original = open
        .original
        .as_ref()
        .filter(|original| open.compare && original.index == current);
    if !open.compare {
        view.original = None;
    }
    if let Some(original) = original
        && let Ok(pixels) = &original.pixels
        && view
            .original
            .as_ref()
            .is_none_or(|(serial, _)| *serial != original.serial)
    {
        view.original = Some((original.serial, upload(ui.ctx(), "original", pixels)));
    }
    let (Some(_), Some((_, texture))) = (shown, &view.preview) else {
        ui.centered_and_justified(|ui| ui.spinner());
        return;
    };
    // Moving to another photo goes back to fit-to-window. When the same photo is redrawn (e.g.
    // after applying noise reduction), keep the view where it was.
    if view.zoomed_photo != Some(current) {
        view.zoomed_photo = Some(current);
        view.zoom = Zoom::default();
    }
    let area = ui.available_rect_before_wrap();
    let image = texture.size_vec2();
    // When comparing, two panes: before and after. The last pane is the after one.
    let panes = if open.compare {
        compare_panes(app.config.compare_split, image, area).to_vec()
    } else {
        vec![area]
    };
    // The panes are the same size, so they share one zoom state. Zooming or dragging in any pane
    // moves all of them together.
    let pane_size = panes[0].size();
    let fit_size = fit(image, pane_size);
    // Zoom that equals the original size (100%) when the fit-to-window size counts as 1
    let actual = image.x / fit_size.x;
    let max = (MAX_ZOOM * actual).max(1.0);
    let responses: Vec<egui::Response> = panes
        .iter()
        .map(|pane| ui.allocate_rect(*pane, egui::Sense::click_and_drag()))
        .collect();
    for (pane, response) in panes.iter().zip(&responses) {
        let pointer = response.hover_pos().map(|pos| pos - pane.center());
        if let Some(pointer) = pointer {
            let (wheel, pinch) =
                ui.input(|input| (input.smooth_scroll_delta.y, input.zoom_delta()));
            let factor = (wheel * WHEEL_ZOOM).exp() * pinch;
            if factor != 1.0 {
                view.zoom.zoom_at(factor, pointer, max);
            }
        }
        if response.dragged() {
            view.zoom.offset += response.drag_delta();
        }
        // While editing a mask, clicks mark points, so double-click zoom is paused.
        if response.double_clicked() && open.mask.is_none() {
            // Fit to window ↔ 100%
            if view.zoom.scale == 1.0 {
                view.zoom.zoom_at(actual, pointer.unwrap_or_default(), max);
            } else {
                view.zoom = Zoom::default();
            }
        }
    }
    view.zoom.clamp(fit_size, pane_size);
    let last = panes.len() - 1;
    for (number, (pane, response)) in panes.iter().zip(&responses).enumerate() {
        let after = number == last;
        let rect = egui::Rect::from_center_size(
            pane.center() + view.zoom.offset,
            fit_size * view.zoom.scale,
        );
        let mut canvas = ui.new_child(egui::UiBuilder::new().max_rect(*pane));
        canvas.set_clip_rect(*pane);
        if after {
            egui::Image::new(texture).paint_at(&canvas, rect);
        } else {
            match (original.map(|original| &original.pixels), &view.original) {
                (Some(Ok(_)), Some((_, before))) => {
                    // Keep the aspect ratio and place it in the same spot, so the after image
                    // doesn't distort even if it briefly has a frame or is a small copy.
                    let size = before.size_vec2();
                    let scale = (rect.width() / size.x).min(rect.height() / size.y);
                    egui::Image::new(before).paint_at(
                        &canvas,
                        egui::Rect::from_center_size(rect.center(), size * scale),
                    );
                }
                (Some(Err(error)), _) => {
                    canvas.put(
                        pane.shrink(16.0),
                        egui::Label::new(
                            RichText::new(t(
                                format!("보정 전 그림을 만들지 못했습니다: {error}"),
                                format!("Could not make the unedited photo: {error}"),
                            ))
                            .weak(),
                        )
                        .wrap(),
                    );
                }
                _ => {
                    canvas.put(
                        egui::Rect::from_center_size(pane.center(), egui::Vec2::splat(24.0)),
                        egui::Spinner::new(),
                    );
                }
            }
        }
        if let Some(active) = open.mask {
            // Clicking the photo adds that spot to the mask. A right click marks a spot to
            // subtract. Every pane maps to the same spot.
            let presses = [
                (response.clicked(), true),
                (response.secondary_clicked(), false),
            ];
            for (pressed, include) in presses {
                if pressed
                    && let Some((x, y)) = response
                        .interact_pointer_pos()
                        .and_then(|pos| photo_point(rect, pos))
                {
                    actions.push(Action::AddMaskPoint { x, y, include });
                }
            }
            // Show the points clicked so far as dots on the photo (green to include, red to
            // exclude).
            if let Some(mask) = open.edit(current).masks.get(active) {
                for point in &mask.points {
                    let colour = if point.include {
                        egui::Color32::from_rgb(60, 220, 90)
                    } else {
                        egui::Color32::from_rgb(235, 70, 60)
                    };
                    canvas.painter().circle(
                        rect.min + egui::vec2(point.x, point.y) * rect.size(),
                        5.0,
                        colour,
                        egui::Stroke::new(1.5, egui::Color32::WHITE),
                    );
                }
            }
        }
        if open.compare {
            let jpeg = original.is_some_and(|original| original.jpeg);
            let name = if after {
                t("보정 후", "After")
            } else if jpeg {
                t("카메라 JPEG", "Camera JPEG")
            } else {
                t("보정 전", "Before")
            };
            canvas.put(
                egui::Rect::from_min_size(pane.min + egui::vec2(6.0, 4.0), egui::vec2(110.0, 20.0)),
                egui::Label::new(
                    RichText::new(name)
                        .strong()
                        .background_color(canvas.visuals().extreme_bg_color),
                )
                .selectable(false),
            );
        }
        if after {
            // Current zoom (relative to source pixels)
            let percent = view.zoom.scale / actual * 100.0;
            let corner = egui::Rect::from_min_max(
                pane.right_bottom() - egui::vec2(90.0, 24.0),
                pane.right_bottom(),
            );
            // Show the usage hint only when hovering the zoom readout. Staying over the photo would
            // be distracting.
            canvas
                .put(
                    corner,
                    egui::Label::new(
                        RichText::new(t(
                            format!("배율 {percent:.0}%"),
                            format!("Zoom {percent:.0}%"),
                        ))
                        .weak(),
                    )
                    .selectable(false),
                )
                .on_hover_text(t(
                    "휠: 확대/축소 · 끌기: 이동 · 두 번 누르기: 맞춤/100%",
                    "Wheel: zoom · Drag: move · Double-click: fit/100%",
                ));
        }
    }
}

/// The size that fits inside `area` keeping the aspect ratio. Never larger than the original.
fn fit(image: egui::Vec2, area: egui::Vec2) -> egui::Vec2 {
    let scale = (area.x / image.x).min(area.y / image.y).min(1.0);
    image * scale
}

fn color_image(pixels: &Pixels) -> egui::ColorImage {
    egui::ColorImage::from_rgba_unmultiplied([pixels.width, pixels.height], &pixels.rgba)
}

fn export_windows(ctx: &egui::Context, open: &Open, actions: &mut Vec<Action>) {
    let window = |title: &str| {
        egui::Window::new(title)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
    };
    match &open.export {
        ExportState::Confirm { photos, existing } => {
            window(t("이미 있는 파일", "Files already exist")).show(ctx, |ui| {
                let total = photos.len();
                ui.label(t(
                    format!("내보낼 {total}장 중 {existing}장은 출력 파일이 이미 있습니다."),
                    format!(
                        "{existing} of the {total} photos to export already have output files."
                    ),
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button(t("건너뛰기", "Skip"))
                        .on_hover_text(t("없는 것만 새로 만듭니다", "Only makes the missing ones"))
                        .clicked()
                    {
                        actions.push(Action::ConfirmExport { overwrite: false });
                    }
                    if ui.button(t("덮어쓰기", "Overwrite")).clicked() {
                        actions.push(Action::ConfirmExport { overwrite: true });
                    }
                    if ui.button(t("취소", "Cancel")).clicked() {
                        actions.push(Action::CancelExport);
                    }
                });
            });
        }
        ExportState::Finished { outcome, warnings } => {
            window(t("내보내기 결과", "Export result")).show(ctx, |ui| {
                let (done, skipped, failed) = (outcome.done, outcome.skipped, outcome.failed.len());
                ui.label(t(
                    format!("완료 {done}장 · 건너뜀 {skipped}장 · 실패 {failed}장"),
                    format!("Done {done} · Skipped {skipped} · Failed {failed}"),
                ));
                if let Some(reason) = &outcome.stopped {
                    ui.label(t(
                        format!("끝까지 처리하지 못했습니다: {reason}"),
                        format!("Stopped before finishing: {reason}"),
                    ));
                }
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for (file, reason) in &outcome.failed {
                            ui.label(format!(
                                "{}: {}: {reason}",
                                t("실패", "Failed"),
                                file.display()
                            ));
                        }
                        for (file, warning) in warnings {
                            ui.label(format!(
                                "{}: {}: {warning}",
                                t("알림", "Note"),
                                file.display()
                            ));
                        }
                    });
                if ui.button(t("닫기", "Close")).clicked() {
                    actions.push(Action::DismissExport);
                }
            });
        }
        ExportState::Failed(reason) => {
            window(t("내보내기 실패", "Export failed")).show(ctx, |ui| {
                ui.label(RichText::new(reason));
                if ui.button(t("닫기", "Close")).clicked() {
                    actions.push(Action::DismissExport);
                }
            });
        }
        ExportState::Idle | ExportState::Running { .. } => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::job::Outcome;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;
    use std::cell::RefCell;

    use egui::vec2;

    /// The image point under the pointer (relative to the image centre, in fit-to-window units).
    fn image_point(zoom: &Zoom, pointer: egui::Vec2) -> egui::Vec2 {
        (pointer - zoom.offset) / zoom.scale
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor_in_place() {
        let mut zoom = Zoom::default();
        let pointer = vec2(120.0, -80.0);
        let before = image_point(&zoom, pointer);
        zoom.zoom_at(2.0, pointer, 8.0);
        assert_eq!(zoom.scale, 2.0);
        let after = image_point(&zoom, pointer);
        assert!((before - after).length() < 1e-3, "{before:?} → {after:?}");

        // Once more, after already zooming and panning
        zoom.zoom_at(1.5, vec2(-40.0, 30.0), 8.0);
        let moved = image_point(&zoom, vec2(-40.0, 30.0));
        zoom.zoom_at(0.8, vec2(-40.0, 30.0), 8.0);
        assert!((moved - image_point(&zoom, vec2(-40.0, 30.0))).length() < 1e-3);
    }

    #[test]
    fn zooming_at_the_centre_does_not_move_the_image() {
        let mut zoom = Zoom::default();
        zoom.zoom_at(3.0, vec2(0.0, 0.0), 8.0);
        assert_eq!(
            zoom,
            Zoom {
                scale: 3.0,
                offset: vec2(0.0, 0.0)
            }
        );
    }

    #[test]
    fn zoom_stays_between_fit_and_the_maximum() {
        let mut zoom = Zoom::default();
        zoom.zoom_at(0.5, vec2(50.0, 50.0), 8.0);
        assert_eq!(zoom, Zoom::default(), "never below fit-to-window");
        zoom.zoom_at(100.0, vec2(0.0, 0.0), 8.0);
        assert_eq!(zoom.scale, 8.0);
    }

    #[test]
    fn panning_stops_at_the_image_edges() {
        // Image that exactly fills a 400×300 area, zoomed 2×: 800×600. It can move only 200
        // sideways and 150 up and down.
        let (fit, area) = (vec2(400.0, 300.0), vec2(400.0, 300.0));
        let mut zoom = Zoom {
            scale: 2.0,
            offset: vec2(500.0, -40.0),
        };
        zoom.clamp(fit, area);
        assert_eq!(zoom.offset, vec2(200.0, -40.0));
        zoom.offset = vec2(-999.0, 999.0);
        zoom.clamp(fit, area);
        assert_eq!(zoom.offset, vec2(-200.0, 150.0));
    }

    #[test]
    fn an_image_smaller_than_the_area_stays_centred() {
        // Portrait photo: fits a 400×300 area as 200×300. At 1.5× it is 300×450 → still narrower
        // than the area horizontally.
        let mut zoom = Zoom {
            scale: 1.5,
            offset: vec2(80.0, 500.0),
        };
        zoom.clamp(vec2(200.0, 300.0), vec2(400.0, 300.0));
        assert_eq!(zoom.offset, vec2(0.0, 75.0));
    }

    #[test]
    fn compare_splits_the_area_so_the_photo_is_shown_largest() {
        let area = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), vec2(1000.0, 700.0));
        let landscape = vec2(6000.0, 4000.0);
        let portrait = vec2(4000.0, 6000.0);
        let [before, after] = compare_panes(Split::Auto, landscape, area);
        assert_eq!(before.width(), 1000.0, "landscape: top/bottom: {before:?}");
        assert!(before.max.y < after.min.y, "before is on top");
        let [before, after] = compare_panes(Split::Auto, portrait, area);
        assert_eq!(before.height(), 700.0, "portrait: side by side: {before:?}");
        assert!(before.max.x < after.min.x, "before is on the left");

        // A chosen direction is followed regardless of the photo.
        let [before, after] = compare_panes(Split::Side, landscape, area);
        assert!(before.max.x < after.min.x);
        let [before, after] = compare_panes(Split::Stacked, portrait, area);
        assert!(before.max.y < after.min.y);

        // The two panes are the same size, inside the area, and do not overlap.
        for split in [Split::Side, Split::Stacked] {
            let [before, after] = compare_panes(split, landscape, area);
            assert_eq!(before.size(), after.size());
            assert!(area.contains_rect(before) && area.contains_rect(after));
            assert!(!before.intersects(after));
        }
        // A small photo fits at its original size either way, so side by side.
        let [before, after] = compare_panes(Split::Auto, vec2(300.0, 200.0), area);
        assert!(before.max.x < after.min.x);
    }

    /// Fake environment that returns preset paths instead of showing dialogs.
    #[derive(Default)]
    struct Fake {
        path: Option<PathBuf>,
        screenshots: usize,
        logs_opened: usize,
    }

    impl Env for Fake {
        fn new_project_path(&mut self) -> Option<PathBuf> {
            self.path.clone()
        }
        fn open_project_path(&mut self) -> Option<PathBuf> {
            self.path.clone()
        }
        fn pick_folder(&mut self) -> Option<PathBuf> {
            self.path.clone()
        }
        fn pick_lut(&mut self) -> Option<PathBuf> {
            self.path.clone()
        }
        fn screenshot(&mut self, _: &egui::Context) {
            self.screenshots += 1;
        }
        fn open_log(&mut self) -> String {
            self.logs_opened += 1;
            crate::lang::t(
                "로그 파일: /tmp/amsil/log.txt",
                "Log file: /tmp/amsil/log.txt",
            )
            .into()
        }
    }

    /// Test bench that shows the screen and passes the actions it produces to the app.
    struct Screen {
        dir: PathBuf,
        app: RefCell<App>,
        env: RefCell<Fake>,
        seen: RefCell<Vec<String>>,
    }

    impl Screen {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("amsil-ui-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            for file in ["사진/a.ARW", "사진/b.ARW"] {
                std::fs::create_dir_all(dir.join("사진")).unwrap();
                std::fs::write(dir.join(file), b"").unwrap();
            }
            let app = App::new(dir.join("config.toml"));
            Self {
                dir,
                app: RefCell::new(app),
                env: RefCell::default(),
                seen: RefCell::default(),
            }
        }

        /// A project created with two photos imported.
        fn with_photos(name: &str) -> Self {
            let screen = Self::new(name);
            screen
                .app
                .borrow_mut()
                .handle(Action::NewProject(screen.dir.join("여행.amsil")));
            screen
                .app
                .borrow_mut()
                .handle(Action::ImportFolder(screen.dir.join("사진")));
            screen
                .app
                .borrow_mut()
                .handle(Action::Backend(Backend::Gpu));
            screen
        }

        fn harness(&self) -> Harness<'_> {
            self.harness_sized(egui::vec2(1100.0, 700.0))
        }

        /// Shows the screen at a given window size.
        fn harness_sized(&self, size: egui::Vec2) -> Harness<'_> {
            self.harness_scaled(size, 1.0)
        }

        /// Shows the screen at a given window size and pixels per point (used to save crisp
        /// images).
        fn harness_scaled(&self, size: egui::Vec2, pixels_per_point: f32) -> Harness<'_> {
            let mut view = View::default();
            let mut fonts_installed = false;
            Harness::builder()
                .with_size(size)
                .with_pixels_per_point(pixels_per_point)
                .build_ui(move |ui| {
                    // Fonts take effect from the next frame. Nothing is drawn on the first frame.
                    if !fonts_installed {
                        install_fonts(ui.ctx());
                        fonts_installed = true;
                        ui.ctx().request_repaint();
                        return;
                    }
                    let actions = draw(
                        ui,
                        &self.app.borrow(),
                        &mut view,
                        &mut *self.env.borrow_mut(),
                    );
                    for action in actions {
                        self.seen.borrow_mut().push(name_of(&action));
                        self.app.borrow_mut().handle(action);
                    }
                })
        }
    }

    impl Drop for Screen {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.dir);
        }
    }

    fn name_of(action: &Action) -> String {
        match action {
            Action::NewProject(_) => "NewProject".into(),
            Action::OpenProject(_) => "OpenProject".into(),
            Action::CloseProject => "CloseProject".into(),
            Action::ImportFolder(_) => "ImportFolder".into(),
            Action::Select { index, extend } => format!("Select {index} {extend}"),
            Action::ApplyDenoise => "ApplyDenoise".into(),
            Action::SetExportFolder(_) => "SetExportFolder".into(),
            Action::Export { only_selected } => format!("Export {only_selected}"),
            Action::ConfirmExport { overwrite } => format!("ConfirmExport {overwrite}"),
            Action::CancelExport => "CancelExport".into(),
            Action::DismissExport => "DismissExport".into(),
            _ => "Other".into(),
        }
    }

    #[test]
    fn start_screen_creates_and_reopens_projects() {
        let screen = Screen::new("start");
        screen.env.borrow_mut().path = Some(screen.dir.join("가을 여행.amsil"));
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("프로젝트 열기");
        harness.get_by_label("새 프로젝트").click();
        harness.run();
        assert_eq!(
            screen.app.borrow().open.as_ref().unwrap().project.name,
            "가을 여행"
        );
        harness.get_by_label("가을 여행");

        harness.get_by_label("프로젝트 닫기").click();
        harness.run();
        assert!(screen.app.borrow().open.is_none());
        // Reopen it from the recent projects on the start screen.
        harness.get_by_label("가을 여행").click();
        harness.run();
        assert!(screen.app.borrow().open.is_some());
        assert_eq!(
            *screen.seen.borrow(),
            ["NewProject", "CloseProject", "OpenProject"]
        );
    }

    #[test]
    fn cancelling_a_dialog_does_nothing() {
        let screen = Screen::new("cancel-dialog");
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("새 프로젝트").click();
        harness.run();
        assert!(screen.app.borrow().open.is_none());
        assert!(screen.seen.borrow().is_empty());
    }

    #[test]
    fn importing_a_folder_lists_photos_and_clicking_one_selects_it() {
        let screen = Screen::new("import");
        screen
            .app
            .borrow_mut()
            .handle(Action::NewProject(screen.dir.join("p.amsil")));
        screen.env.borrow_mut().path = Some(screen.dir.join("사진"));
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("폴더 가져오기").click();
        harness.run();
        harness.get_by_label("a.ARW");
        harness.get_by_label("b.ARW").click();
        // While waiting for the preview the spinner keeps turning, so the screen never settles.
        harness.run_steps(3);
        assert_eq!(screen.app.borrow().open.as_ref().unwrap().current, Some(1));
        assert_eq!(*screen.seen.borrow(), ["ImportFolder", "Select 1 false"]);
    }

    /// Reads the zoom (%) shown in the corner of the preview.
    fn shown_zoom(harness: &Harness<'_>) -> f32 {
        use egui_kittest::kittest::NodeT;
        let node = harness.get_by_label_contains("배율 ").accesskit_node();
        let text = node
            .label()
            .or_else(|| node.value())
            .expect("the zoom readout has no text");
        text.trim_start_matches("배율 ")
            .trim_end_matches('%')
            .parse()
            .unwrap()
    }

    #[test]
    fn wheel_zooms_the_preview_between_fit_and_400_percent() {
        let screen = Screen::with_photos("zoom");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        let pixels = Pixels {
            width: 2000,
            height: 1000,
            rgba: vec![128; 2000 * 1000 * 4],
        };
        screen.app.borrow_mut().handle(Action::PreviewReady {
            generation,
            index: 0,
            result: Ok(pixels),
        });
        let mut harness = screen.harness();
        harness.run();
        let fit = shown_zoom(&harness);
        assert!(
            fit > 10.0 && fit < 60.0,
            "a 2000-pixel-wide photo should be scaled down to fit the window: {fit}%"
        );

        // Scroll the wheel up over the preview
        let wheel = |amount: f32| egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, amount),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        };
        harness.hover_at(egui::pos2(550.0, 330.0));
        harness.event(wheel(120.0));
        harness.run();
        let zoomed = shown_zoom(&harness);
        assert!(zoomed > fit * 1.2, "{fit}% → {zoomed}%");

        // No matter how far up, it never exceeds 400%
        for _ in 0..40 {
            harness.event(wheel(400.0));
            harness.run();
        }
        assert_eq!(shown_zoom(&harness), 400.0);

        // No matter how far down, it never gets smaller than the fit-to-window size
        for _ in 0..40 {
            harness.event(wheel(-400.0));
            harness.run();
        }
        assert_eq!(shown_zoom(&harness), fit);
    }

    #[test]
    fn a_preview_larger_than_the_gpu_texture_limit_is_still_shown() {
        let screen = Screen::with_photos("huge-preview");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        // A preview larger than the test environment's texture limit (2048)
        let pixels = Pixels {
            width: 3000,
            height: 100,
            rgba: vec![128; 3000 * 100 * 4],
        };
        screen.app.borrow_mut().handle(Action::PreviewReady {
            generation,
            index: 0,
            result: Ok(pixels),
        });
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label_contains("배율 ");
    }

    #[test]
    fn compare_shows_before_and_after_and_zooms_them_together() {
        let screen = Screen::with_photos("compare");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        let photo = || Pixels {
            width: 2000,
            height: 1000,
            rgba: vec![128; 2000 * 1000 * 4],
        };
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        handle(Action::PreviewReady {
            generation,
            index: 0,
            result: Ok(photo()),
        });
        let mut harness = screen.harness();
        harness.run();
        assert_eq!(shown(&harness, "보정 전"), 0, "one pane before comparing");

        harness.get_by_label("비교").click();
        // While waiting for the before image the spinner turns, so the screen never settles.
        harness.run_steps(3);
        assert!(screen.app.borrow().open.as_ref().unwrap().compare);
        let (generation, original) = {
            let app = screen.app.borrow();
            let open = app.open.as_ref().unwrap();
            (open.preview_generation(), open.original_generation())
        };
        handle(Action::PreviewReady {
            generation,
            index: 0,
            result: Ok(photo()),
        });
        handle(Action::OriginalReady {
            generation: original,
            index: 0,
            result: Ok(photo()),
        });
        harness.run();
        harness.get_by_label("보정 후");
        let fit = shown_zoom(&harness);

        // Scrolling up in the before pane also raises the zoom shown in the after pane.
        let before = harness.get_by_label("보정 전").rect();
        harness.hover_at(before.right_bottom() + vec2(60.0, 60.0));
        harness.event(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: vec2(0.0, 120.0),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: egui::Modifiers::NONE,
        });
        harness.run();
        let zoomed = shown_zoom(&harness);
        assert!(zoomed > fit * 1.2, "{fit}% → {zoomed}%");

        // The direction button is saved to the settings.
        harness.get_by_label("좌우").click();
        harness.run();
        assert_eq!(screen.app.borrow().config.compare_split, Split::Side);
    }

    #[test]
    fn a_preset_is_chosen_with_one_click_and_cleared_with_none() {
        let screen = Screen::with_photos("presets");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        let mut harness = screen.harness();
        harness.run_steps(3);
        for name in ["없음", "음식", "인물", "풍경", "야경", "흑백"] {
            harness.get_by_label(name);
        }
        harness.get_by_label("음식").click();
        harness.run_steps(3);
        let preset = || screen.app.borrow().open.as_ref().unwrap().edit(0).preset;
        assert_eq!(preset().as_deref(), Some("food"));

        harness.get_by_label("없음").click();
        harness.run_steps(3);
        assert_eq!(preset(), None);
    }

    #[test]
    fn a_lut_file_is_chosen_shown_by_name_and_removed() {
        let screen = Screen::with_photos("lut");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        screen.env.borrow_mut().path = Some("/home/user/luts/따뜻한 필름.cube".into());
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("LUT 불러오기…").click();
        harness.run_steps(3);
        let lut = || screen.app.borrow().open.as_ref().unwrap().edit(0).lut;
        assert_eq!(
            lut().unwrap().file,
            std::path::Path::new("/home/user/luts/따뜻한 필름.cube")
        );
        harness.get_by_label("따뜻한 필름.cube");

        harness.get_by_label("LUT 없애기").click();
        harness.run_steps(3);
        assert_eq!(lut(), None);
    }

    #[test]
    fn reset_button_clears_the_look() {
        let screen = Screen::with_photos("reset-look");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        screen
            .app
            .borrow_mut()
            .handle(Action::SetPreset(Some("mono".into())));
        screen.app.borrow_mut().handle(Action::SetStrength {
            value: 0.5,
            commit: true,
        });
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("보정 초기화").click();
        harness.run_steps(3);
        let edit = screen.app.borrow().open.as_ref().unwrap().edit(0);
        assert_eq!((edit.preset, edit.strength), (None, 1.0));
    }

    #[test]
    fn the_look_panel_asks_to_pick_a_photo_first() {
        let screen = Screen::with_photos("no-photo");
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("사진을 고르면 프리셋을 적용할 수 있습니다");
    }

    #[test]
    fn ctrl_a_selects_every_photo() {
        let screen = Screen::with_photos("select-all");
        let mut harness = screen.harness();
        harness.run();
        harness.key_press_modifiers(egui::Modifiers::COMMAND, Key::A);
        harness.run();
        assert_eq!(screen.app.borrow().open.as_ref().unwrap().selected.len(), 2);
        harness.get_by_label("사진 2장 · 선택 2장");
    }

    #[test]
    fn denoise_is_applied_with_the_apply_button() {
        let screen = Screen::with_photos("denoise");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        screen.app.borrow_mut().handle(Action::SetDenoiseLevel(0.7));
        // Make the preview arrive (until it does, the spinner keeps turning).
        let preview_arrives = || {
            let generation = screen
                .app
                .borrow()
                .open
                .as_ref()
                .unwrap()
                .preview_generation();
            let pixels = Pixels {
                width: 2,
                height: 1,
                rgba: vec![200; 8],
            };
            screen.app.borrow_mut().handle(Action::PreviewReady {
                generation,
                index: 0,
                result: Ok(pixels),
            });
        };
        preview_arrives();
        // The right panel is long, so the bottom group is cut off in a 1100×700 window (on a real
        // screen it scrolls).
        let mut harness = screen.harness_sized(egui::vec2(1100.0, 1000.0));
        harness.run();
        harness.get_by_label("선택한 1장에 적용됩니다");
        harness.get_by_label("적용").click();
        harness.run_steps(3);
        preview_arrives();
        harness.run();
        assert_eq!(
            screen.app.borrow().open.as_ref().unwrap().edit(0).denoise,
            0.7
        );
        harness.get_by_label("지금 사진: 70%");
    }

    #[test]
    fn export_asks_about_existing_files_then_shows_progress_and_result() {
        let screen = Screen::with_photos("export");
        let out = screen.dir.join("결과");
        std::fs::create_dir_all(out.join("사진")).unwrap();
        std::fs::write(out.join("사진/a.heic"), b"old").unwrap();
        screen.app.borrow_mut().handle(Action::SetExportFolder(out));
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("전체 내보내기").click();
        harness.run();
        harness.get_by_label("내보낼 2장 중 1장은 출력 파일이 이미 있습니다.");
        harness.get_by_label("건너뛰기").click();
        harness.run();
        assert!(matches!(
            screen.app.borrow().open.as_ref().unwrap().export,
            ExportState::Running { total: 2, .. }
        ));
        harness.get_by_label("0/2 · 준비 중");

        let report = crate::export::Report {
            status: crate::job::Status {
                total: 2,
                finished: 1,
                progress: 1.4,
                running: 1,
            },
            remaining: Some(40.0),
            idle: 0.1,
            cpu: 87.0,
            memory: 2 << 30,
            memory_limit: 8 << 30,
            gpu: 0.0,
            low_memory: false,
        };
        screen
            .app
            .borrow_mut()
            .handle(Action::ExportProgress(report));
        harness.run();
        harness.get_by_label("1/2 · 약 40초 남음");
        harness.get_by_label("CPU 87% · 메모리 2.0/8.0GB · GPU 0% · 처리 중 1장");
        harness.get_by_label("취소").click();
        harness.run();

        let outcome = Outcome {
            done: 1,
            skipped: 1,
            failed: vec![],
            stopped: None,
        };
        screen
            .app
            .borrow_mut()
            .handle(Action::ExportFinished(Ok((outcome, vec![]))));
        harness.run();
        harness.get_by_label("완료 1장 · 건너뜀 1장 · 실패 0장");
        harness.get_by_label("닫기").click();
        harness.run();
        assert_eq!(
            screen.app.borrow().open.as_ref().unwrap().export,
            ExportState::Idle
        );
        assert_eq!(
            *screen.seen.borrow(),
            [
                "Export false",
                "ConfirmExport false",
                "CancelExport",
                "DismissExport"
            ]
        );
    }

    #[test]
    fn screenshot_button_and_f12_ask_for_a_capture() {
        let screen = Screen::with_photos("shot");
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("스크린샷").click();
        harness.run();
        assert_eq!(screen.env.borrow().screenshots, 1);
        harness.key_press(Key::F12);
        harness.run();
        assert_eq!(screen.env.borrow().screenshots, 2);
    }

    #[test]
    fn cpu_backend_is_shown_with_a_way_to_read_the_advice() {
        let screen = Screen::with_photos("cpu");
        screen
            .app
            .borrow_mut()
            .handle(Action::Backend(Backend::Cpu(Text::of(
                "Vulkan 드라이버를 설치하세요",
            ))));
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("CPU (느림)").click();
        harness.run();
        harness.get_by_label("Vulkan 드라이버를 설치하세요");
    }

    /// Saves screen images to `target/ui/` for checking by eye. `cargo test -- --ignored render`
    #[test]
    #[ignore = "needs a GPU and sample photos, and is for saving image files"]
    fn render_screens() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ui");
        std::fs::create_dir_all(&out).unwrap();
        let photos = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10361110");
        let screen = Screen::new("render");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::NewProject(screen.dir.join("가을 여행.amsil")));
        handle(Action::CloseProject);
        let mut harness = screen.harness();
        let shot = |harness: &mut Harness<'_>, name: &str| {
            harness.run_steps(4);
            harness.render().unwrap().save(out.join(name)).unwrap();
        };
        shot(&mut harness, "1-start.png");

        // Import a real photo folder, and make and insert the thumbnails and previews directly.
        handle(Action::OpenProject(screen.dir.join("가을 여행.amsil")));
        handle(Action::ImportFolder(photos.clone()));
        handle(Action::Backend(Backend::Gpu));
        let count = screen.app.borrow().open.as_ref().unwrap().photos.len();
        let mut classifier = crate::recommend::load_classifier().ok();
        for index in 0..count {
            let path = screen.app.borrow().open.as_ref().unwrap().photo_path(index);
            let picture = crate::preview::thumbnail(&path, 448).unwrap();
            let guess = picture
                .as_ref()
                .zip(classifier.as_mut())
                .map(|(picture, classifier)| classifier.classify(picture).unwrap());
            handle(Action::Thumbnail {
                index,
                pixels: picture.map(|picture| picture.fit_within(160).unwrap()),
                guess,
            });
        }
        handle(Action::Select {
            index: 6,
            extend: false,
        });
        handle(Action::Select {
            index: 7,
            extend: true,
        });
        let path = screen.app.borrow().open.as_ref().unwrap().photo_path(7);
        let rendered = crate::export::render(
            &path,
            None,
            crate::export::Look {
                params: &crate::edit::Params::default(),
                lut: None,
                frame: None,
                masks: &[],
                segmenter: None,
                overlay: None,
                skin: 0.0,
                skin_finder: None,
                capture: crate::capture::Capture::off(),
                capture_view: None,
            },
            &|_| {},
        )
        .unwrap();
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        handle(Action::PreviewReady {
            generation,
            index: 7,
            result: Ok(crate::preview::shrink(&rendered.image, 8192)),
        });
        handle(Action::SetDenoiseLevel(0.6));
        handle(Action::SetExportFolder(
            "/home/user/Pictures/가을여행".into(),
        ));
        shot(&mut harness, "2-workspace.png");

        // Zoomed in with the wheel
        harness.hover_at(egui::pos2(560.0, 300.0));
        for _ in 0..6 {
            harness.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, 120.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(2);
        }
        shot(&mut harness, "2b-zoomed.png");
        harness.remove_cursor();

        // With a preset applied ("풍경" (landscape), strength 120%)
        handle(Action::SetPreset(Some("landscape".into())));
        handle(Action::SetStrength {
            value: 1.2,
            commit: true,
        });
        let params = {
            let app = screen.app.borrow();
            app.open.as_ref().unwrap().params(7, &app.presets)
        };
        let looked = crate::export::render(
            &path,
            None,
            crate::export::Look {
                params: &params,
                lut: None,
                frame: None,
                masks: &[],
                segmenter: None,
                overlay: None,
                skin: 0.0,
                skin_finder: None,
                capture: crate::capture::Capture::off(),
                capture_view: None,
            },
            &|_| {},
        )
        .unwrap();
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        handle(Action::PreviewReady {
            generation,
            index: 7,
            result: Ok(crate::preview::shrink(&looked.image, 8192)),
        });
        shot(&mut harness, "2c-preset.png");
        // Before/after comparison: before (no edits) and after (landscape 120%)
        handle(Action::SetCompare(true));
        let (generation, original) = {
            let app = screen.app.borrow();
            let open = app.open.as_ref().unwrap();
            (open.preview_generation(), open.original_generation())
        };
        handle(Action::PreviewReady {
            generation,
            index: 7,
            result: Ok(crate::preview::shrink(&looked.image, 8192)),
        });
        handle(Action::OriginalReady {
            generation: original,
            index: 7,
            result: Ok(crate::preview::shrink(&rendered.image, 8192)),
        });
        shot(&mut harness, "2d-compare.png");
        let before = harness.get_by_label("보정 전").rect();
        harness.hover_at(before.right_bottom() + vec2(80.0, 80.0));
        for _ in 0..6 {
            harness.event(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, 120.0),
                phase: egui::TouchPhase::Move,
                source: egui::MouseWheelSource::Wheel,
                modifiers: egui::Modifiers::NONE,
            });
            harness.run_steps(2);
        }
        shot(&mut harness, "2e-compare-zoomed.png");
        harness.remove_cursor();
        handle(Action::SetCompare(false));
        handle(Action::ResetLook);

        // With a LUT applied (a warm, low-saturation test LUT, strength 80%)
        let cube = screen.dir.join("따뜻한 필름.cube");
        let mut text = String::from("LUT_3D_SIZE 17\n");
        for index in 0..17usize.pow(3) {
            let [r, g, b] = [index % 17, index / 17 % 17, index / 289].map(|v| v as f32 / 16.0);
            let y = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            let fade = |v: f32| y + (v - y) * 0.6;
            text += &format!(
                "{} {} {}\n",
                (fade(r) * 1.06 + 0.02).min(1.0),
                fade(g),
                fade(b) * 0.9
            );
        }
        std::fs::write(&cube, text).unwrap();
        handle(Action::SetLut(Some(cube.clone())));
        handle(Action::SetLutStrength {
            value: 0.8,
            commit: true,
        });
        let lut = crate::lut::Lut::load(&cube).unwrap();
        let params = crate::edit::Params::default();
        let toned = crate::export::render(
            &path,
            None,
            crate::export::Look {
                params: &params,
                lut: Some((&lut, 0.8)),
                frame: None,
                masks: &[],
                segmenter: None,
                overlay: None,
                skin: 0.0,
                skin_finder: None,
                capture: crate::capture::Capture::off(),
                capture_view: None,
            },
            &|_| {},
        )
        .unwrap();
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        handle(Action::PreviewReady {
            generation,
            index: 7,
            result: Ok(crate::preview::shrink(&toned.image, 8192)),
        });
        shot(&mut harness, "2d-lut.png");
        handle(Action::ResetLook);

        // Mask: the middle building picked by clicking (when the model is present)
        if let Ok(segmenter) = crate::mask::load_segmenter() {
            let segmenter = std::sync::Mutex::new(segmenter);
            handle(Action::AddMask);
            handle(Action::AddMaskPoint {
                x: 0.48,
                y: 0.35,
                include: true,
            });
            let masks = screen.app.borrow().open.as_ref().unwrap().edit(7).masks;
            let loaded =
                crate::export::load(&path, None, &crate::capture::Capture::off(), &|_| {}).unwrap();
            let params = crate::edit::Params::default();
            let look = crate::export::Look {
                masks: &masks,
                segmenter: Some(&segmenter),
                overlay: Some(0),
                ..crate::export::Look::new(&params)
            };
            let areas = loaded.areas(look).unwrap();
            let shown = loaded.develop(&loaded.cam, look, &areas);
            let generation = screen
                .app
                .borrow()
                .open
                .as_ref()
                .unwrap()
                .preview_generation();
            handle(Action::PreviewReady {
                generation,
                index: 7,
                result: Ok(crate::preview::shrink(&shown, 8192)),
            });
            shot(&mut harness, "2e-mask.png");
            handle(Action::RemoveMask);
        }

        // Exporting
        handle(Action::Export {
            only_selected: false,
        });
        handle(Action::ExportProgress(crate::export::Report {
            status: crate::job::Status {
                total: count,
                finished: 6,
                progress: 7.8,
                running: 8,
            },
            remaining: Some(372.0),
            idle: 0.3,
            cpu: 87.0,
            memory: 9 << 30,
            memory_limit: 24 << 30,
            gpu: 0.0,
            low_memory: false,
        }));
        shot(&mut harness, "3-exporting.png");

        // Result
        let outcome = Outcome {
            done: count - 1,
            skipped: 0,
            failed: vec![(
                "DSC04560.ARW".into(),
                Text::of("지원하지 않거나 손상된 RAW 파일입니다"),
            )],
            stopped: None,
        };
        handle(Action::ExportFinished(Ok((outcome, vec![]))));
        shot(&mut harness, "4-finished.png");

        // English screens: the workspace (with presets, LUT and fine adjustments expanded) and the
        // start screen
        handle(Action::DismissExport);
        handle(Action::SetLanguage(crate::lang::Lang::En));
        handle(Action::SetPreset(Some("landscape".into())));
        handle(Action::SetLut(Some(cube)));
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        handle(Action::PreviewReady {
            generation,
            index: 7,
            result: Ok(crate::preview::shrink(&toned.image, 8192)),
        });
        harness.run_steps(4);
        harness.get_by_label("Details").click();
        harness.run_steps(4);
        harness.get_by_label("By colour").click();
        shot(&mut harness, "5-english.png");

        // Light mode (back in Korean)
        handle(Action::SetLanguage(crate::lang::Lang::Ko));
        handle(Action::SetTheme(Theme::Light));
        shot(&mut harness, "6-light.png");

        // AI features and sources window
        harness.get_by_label("AI 정보").click();
        shot(&mut harness, "8-ai-info.png");
        harness.key_press(Key::Escape);

        // Narrow, tall window (640×800, close to the smallest size)
        let mut narrow = screen.harness_sized(egui::vec2(640.0, 800.0));
        narrow.run_steps(6);
        narrow
            .render()
            .unwrap()
            .save(out.join("7-narrow.png"))
            .unwrap();
        drop(narrow);

        handle(Action::CloseProject);
        shot(&mut harness, "6b-light-start.png");
        handle(Action::SetTheme(Theme::Dark));
    }

    /// On-screen texts that contain Hangul. Leaves out "한국어" on the language button.
    fn korean_texts(harness: &Harness<'_>) -> Vec<String> {
        use egui_kittest::kittest::NodeT;
        harness
            .root()
            .children_recursive()
            .flat_map(|node| {
                let node = node.accesskit_node();
                [node.label(), node.value()]
            })
            .flatten()
            .filter(|text| text != "한국어" && text.chars().any(|c| ('가'..='힣').contains(&c)))
            .collect()
    }

    #[test]
    fn the_language_is_switched_on_the_start_screen_and_in_the_workspace() {
        use crate::lang::Lang;
        let screen = Screen::new("lang");
        screen.env.borrow_mut().path = Some(screen.dir.join("trip.amsil"));
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("새 프로젝트");
        harness.get_by_label("English").click();
        harness.run_steps(3);
        assert_eq!(screen.app.borrow().config.lang, Some(Lang::En));
        harness.get_by_label("New project").click();
        harness.run_steps(3);
        harness.get_by_label("Import folder");

        harness.get_by_label("한국어").click();
        harness.run_steps(3);
        assert_eq!(screen.app.borrow().config.lang, Some(Lang::Ko));
        harness.get_by_label("폴더 가져오기");
    }

    #[test]
    fn nothing_is_left_in_korean_when_english_is_chosen() {
        use crate::lang::Lang;
        let screen = Screen::new("english");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::NewProject(screen.dir.join("trip.amsil")));
        handle(Action::CloseProject);
        let mut harness = screen.harness();
        harness.run_steps(4);
        assert!(
            korean_texts(&harness).len() > 3,
            "the Korean screen should show Hangul (checks that the test reads text correctly)"
        );
        handle(Action::SetLanguage(Lang::En));
        let check = |harness: &mut Harness<'_>, what: &str| {
            harness.run_steps(4);
            let left = korean_texts(harness);
            assert!(left.is_empty(), "{what}: {left:?}");
        };
        check(&mut harness, "start screen");

        handle(Action::OpenProject(screen.dir.join("trip.amsil")));
        check(&mut harness, "empty workspace");
        handle(Action::ImportFolder(screen.dir.join("사진")));
        handle(Action::Backend(Backend::Cpu(Text::of("no Vulkan"))));
        check(&mut harness, "before picking a photo");

        handle(Action::Select {
            index: 0,
            extend: false,
        });
        handle(Action::SetPreset(Some("food".into())));
        handle(Action::SetLut(Some("/luts/film.cube".into())));
        handle(Action::Thumbnail {
            index: 0,
            pixels: None,
            guess: Some(crate::recommend::Guess {
                category: crate::recommend::Category::Food,
                confidence: 0.9,
            }),
        });
        handle(Action::SetFrame(Some(crate::frame::Frame {
            layout: crate::frame::Layout::Band,
            dark: false,
        })));
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        handle(Action::PreviewReady {
            generation,
            index: 0,
            result: Ok(Pixels {
                width: 4,
                height: 4,
                rgba: vec![128; 64],
            }),
        });
        harness.run_steps(3);
        harness.get_by_label("Details").click();
        harness.run_steps(3);
        harness.get_by_label("By colour").click();
        check(&mut harness, "edit panel fully expanded");
        handle(Action::SetCompare(true));
        check(&mut harness, "comparison on, waiting for the before image");
        let original = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .original_generation();
        handle(Action::OriginalReady {
            generation: original,
            index: 0,
            result: Err(Text::of("broken")),
        });
        check(&mut harness, "when the before image could not be made");
        handle(Action::OriginalReady {
            generation: original,
            index: 0,
            result: Ok(Pixels {
                width: 4,
                height: 4,
                rgba: vec![128; 64],
            }),
        });
        check(&mut harness, "before/after comparison");
        handle(Action::SetCompare(false));
        handle(Action::AddMask);
        handle(Action::AddMaskPoint {
            x: 0.5,
            y: 0.5,
            include: true,
        });
        check(&mut harness, "editing a mask");
        handle(Action::SelectMask(None));
        handle(Action::Segmenter(Some(Text::of("no model"))));
        handle(Action::SkinModel(Some(Text::of("no model"))));
        check(&mut harness, "when the mask and skin models are missing");
        handle(Action::Segmenter(None));
        handle(Action::SkinModel(None));
        harness.get_by_label("CPU (slow)").click();
        check(&mut harness, "CPU notice");
        handle(Action::Backend(Backend::Unavailable(Text::of("no model"))));
        check(&mut harness, "when models are missing");
        handle(Action::Backend(Backend::Unknown));
        check(&mut harness, "AI preparing");
        harness.get_by_label("AI info").click();
        check(&mut harness, "AI info window");
        handle(Action::Recommender(Some(Text::of("no model"))));
        check(&mut harness, "when the classifier model is missing");

        // Export: confirmation window, in progress, result, failure
        let out = screen.dir.join("out");
        std::fs::create_dir_all(out.join("사진")).unwrap();
        std::fs::write(out.join("사진/a.heic"), b"old").unwrap();
        handle(Action::SetExportFolder(out));
        handle(Action::Export {
            only_selected: false,
        });
        check(&mut harness, "existing file confirmation");
        handle(Action::ConfirmExport { overwrite: false });
        check(&mut harness, "export preparing");
        let report = |idle| crate::export::Report {
            status: crate::job::Status {
                total: 2,
                finished: 1,
                progress: 1.4,
                running: 1,
            },
            remaining: Some(372.0),
            idle,
            cpu: 87.0,
            memory: 2 << 30,
            memory_limit: 8 << 30,
            gpu: 0.0,
            low_memory: true,
        };
        handle(Action::ExportProgress(report(0.1)));
        check(&mut harness, "exporting");
        handle(Action::ExportProgress(report(45.0)));
        check(&mut harness, "when progress seems stalled");
        let outcome = Outcome {
            done: 1,
            skipped: 0,
            failed: vec![("b.ARW".into(), Text::of("broken"))],
            stopped: Some(Text::of("disk full")),
        };
        let warnings = vec![("a.ARW".into(), Text::of("quality lowered"))];
        handle(Action::ExportFinished(Ok((outcome, warnings))));
        check(&mut harness, "export result");
        handle(Action::ExportFinished(Err(Text::of("boom"))));
        check(&mut harness, "export failure");
    }

    #[test]
    fn a_frame_is_chosen_recoloured_and_removed() {
        use crate::frame::{Frame, Layout};
        let screen = Screen::with_photos("frame");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        let frame = || screen.app.borrow().open.as_ref().unwrap().edit(0).frame;
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("하단 띠").click();
        harness.run_steps(3);
        assert_eq!(
            frame(),
            Some(Frame {
                layout: Layout::Band,
                dark: false
            })
        );
        harness.get_by_label("검정").click();
        harness.run_steps(3);
        harness.get_by_label("4:5").click();
        harness.run_steps(3);
        assert_eq!(
            frame(),
            Some(Frame {
                layout: Layout::Ratio(4, 5),
                dark: true
            }),
            "changing the layout keeps the colour"
        );
        harness.get_by_label("프레임 없음").click();
        harness.run_steps(3);
        assert_eq!(frame(), None);
    }

    #[test]
    fn the_recommended_preset_is_marked_and_applied_in_one_click() {
        use crate::recommend::{Category, Guess};
        let screen = Screen::with_photos("recommend");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        let mut harness = screen.harness();
        harness.run_steps(3);
        assert!(
            harness.query_by_label("추천 프리셋 적용").is_none(),
            "no button without classified photos"
        );

        handle(Action::Thumbnail {
            index: 0,
            pixels: None,
            guess: Some(Guess {
                category: Category::Food,
                confidence: 0.9,
            }),
        });
        harness.run_steps(3);
        // The list shows the photo's category, and the preset button gets a suggestion mark.
        harness.get_by_label("a.ARW · 음식");
        harness.get_by_label("음식 ★");
        harness.get_by_label("추천 프리셋 적용").click();
        harness.run_steps(3);
        let preset = screen.app.borrow().open.as_ref().unwrap().edit(0).preset;
        assert_eq!(preset.as_deref(), Some("food"));
    }

    #[test]
    fn a_missing_classification_model_is_explained_in_the_preset_panel() {
        let screen = Screen::with_photos("no-clip");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        handle(Action::Recommender(Some(Text::of(
            "사진 분류 모델이 없습니다",
        ))));
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("프리셋 추천: 모델 없음");
    }

    #[test]
    fn the_bundled_font_has_the_symbols_the_screen_uses() {
        use ab_glyph::Font;
        // Characters missing from the font show up as boxes on screen.
        let font = ab_glyph::FontRef::try_from_slice(crate::frame::FONT).unwrap();
        for symbol in ['★', '·', '…', '×', '%', '–', '~'] {
            assert_ne!(font.glyph_id(symbol).0, 0, "{symbol}");
        }
    }

    #[test]
    fn a_click_on_the_preview_becomes_a_place_in_the_photo() {
        let rect = egui::Rect::from_min_size(egui::pos2(100.0, 50.0), egui::vec2(400.0, 200.0));
        assert_eq!(photo_point(rect, egui::pos2(100.0, 50.0)), Some((0.0, 0.0)));
        assert_eq!(
            photo_point(rect, egui::pos2(300.0, 100.0)),
            Some((0.5, 0.25))
        );
        assert_eq!(
            photo_point(rect, egui::pos2(500.0, 250.0)),
            Some((1.0, 1.0))
        );
        assert_eq!(photo_point(rect, egui::pos2(99.0, 100.0)), None, "outside");
        assert_eq!(photo_point(rect, egui::pos2(300.0, 251.0)), None);
    }

    /// Clicks a spot on the screen with a mouse button.
    fn press(harness: &mut Harness<'_>, pos: egui::Pos2, button: egui::PointerButton) {
        harness.hover_at(pos);
        for pressed in [true, false] {
            harness.event(egui::Event::PointerButton {
                pos,
                button,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        harness.run_steps(3);
    }

    #[test]
    fn a_mask_is_made_by_clicking_on_the_preview() {
        let screen = Screen::with_photos("mask");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        let masks = || screen.app.borrow().open.as_ref().unwrap().edit(0).masks;
        let show = |screen: &Screen| {
            let generation = screen
                .app
                .borrow()
                .open
                .as_ref()
                .unwrap()
                .preview_generation();
            screen.app.borrow_mut().handle(Action::PreviewReady {
                generation,
                index: 0,
                result: Ok(Pixels {
                    width: 1500,
                    height: 1000,
                    rgba: vec![128; 1500 * 1000 * 4],
                }),
            });
        };
        show(&screen);
        let mut harness = screen.harness();
        harness.run_steps(3);
        // Unless a mask is being edited, clicking makes no point.
        press(
            &mut harness,
            egui::pos2(560.0, 300.0),
            egui::PointerButton::Primary,
        );
        assert!(masks().is_empty());

        harness.get_by_label("새 마스크").click();
        harness.run_steps(3);
        show(&screen);
        harness.run_steps(3);
        assert_eq!(masks().len(), 1);
        harness.get_by_label("마스크 1");

        press(
            &mut harness,
            egui::pos2(560.0, 300.0),
            egui::PointerButton::Primary,
        );
        show(&screen);
        press(
            &mut harness,
            egui::pos2(600.0, 320.0),
            egui::PointerButton::Secondary,
        );
        show(&screen);
        let points = masks()[0].points.clone();
        assert_eq!(points.len(), 2, "{points:?}");
        assert!(points[0].include && !points[1].include);
        assert!(
            points
                .iter()
                .all(|p| (0.0..=1.0).contains(&p.x) && (0.0..=1.0).contains(&p.y))
        );
        assert!(
            points[1].x > points[0].x && points[1].y > points[0].y,
            "{points:?}"
        );
        harness.run_steps(3);
        harness.get_by_label("점 2개");

        harness.get_by_label("점 되돌리기").click();
        harness.run_steps(3);
        assert_eq!(masks()[0].points.len(), 1);
        harness.get_by_label("반전").click();
        harness.run_steps(3);
        assert!(masks()[0].invert);
        harness.get_by_label("마스크 지우기").click();
        harness.run_steps(3);
        assert!(masks().is_empty());
    }

    #[test]
    fn a_missing_mask_model_is_explained_instead_of_offering_masks() {
        let screen = Screen::with_photos("no-sam");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        handle(Action::Segmenter(Some(Text::of("마스크 모델이 없습니다"))));
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("마스크: 모델 없음");
        assert!(harness.query_by_label("새 마스크").is_none());
    }

    #[test]
    fn the_capture_panel_changes_the_selected_photos() {
        let screen = Screen::with_photos("capture-panel");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        // The right panel is long, so the bottom group is cut off in a 1100×700 window (on a real
        // screen it scrolls).
        let mut harness = screen.harness_sized(egui::vec2(1100.0, 1000.0));
        harness.run_steps(3);
        harness.get_by_label("캡처 샤픈");
        assert!(harness.query_all_by_label("반복 횟수").count() > 0);
        // Radius and threshold are inside "세부 설정" (More settings) and start out automatic.
        assert!(harness.query_by_label("반경 자동").is_none(), "collapsed");
        harness.get_by_label("세부 설정").click();
        harness.run_steps(3);
        harness.get_by_label("반경 자동").click();
        // While waiting for the preview the spinner keeps redrawing, so run a fixed number of steps
        // instead of run().
        harness.run_steps(3);
        let capture = screen.app.borrow().open.as_ref().unwrap().edit(0).capture;
        assert_eq!(
            capture.radius,
            Some(0.75),
            "turning auto off starts from RawTherapee's default radius"
        );
        harness.get_by_label("캡처 샤픈 켜기").click();
        harness.run_steps(3);
        assert!(
            !screen
                .app
                .borrow()
                .open
                .as_ref()
                .unwrap()
                .edit(0)
                .capture
                .enabled
        );
    }

    #[test]
    fn the_automatic_capture_values_are_shown_when_known() {
        let screen = Screen::with_photos("capture-auto-ui");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        handle(Action::CaptureAuto {
            index: 0,
            radius: 0.68,
            threshold: 0.12,
        });
        // The right panel is long, so the bottom group is cut off in a 1100×700 window (on a real
        // screen it scrolls).
        let mut harness = screen.harness_sized(egui::vec2(1100.0, 1000.0));
        harness.run_steps(3);
        harness.get_by_label("세부 설정").click();
        harness.run_steps(3);
        assert_eq!(shown(&harness, "자동: 0.68"), 1);
        // The contrast threshold defaults to a fixed 0.4, so no automatic value is shown. Turning
        // auto on shows it.
        assert_eq!(shown(&harness, "자동: 0.12"), 0);
        harness.get_by_label("문턱 자동").click();
        harness.run_steps(3);
        assert_eq!(shown(&harness, "자동: 0.12"), 1);
        // A value belonging to another photo is not shown.
        handle(Action::Select {
            index: 1,
            extend: false,
        });
        harness.run_steps(3);
        assert_eq!(shown(&harness, "자동: 0.68"), 0);
    }

    #[test]
    fn the_sharpened_area_can_be_shown_and_is_greyed_out_when_capture_is_off() {
        use egui_kittest::kittest::NodeT;
        let screen = Screen::with_photos("capture-view-ui");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        let mut harness = screen.harness_sized(egui::vec2(1100.0, 1000.0));
        harness.run_steps(3);
        harness.get_by_label("빨간 표시").click();
        harness.run_steps(3);
        assert_eq!(
            screen.app.borrow().open.as_ref().unwrap().capture_view,
            Some(CaptureView::Overlay)
        );
        harness.get_by_label("흑백 지도").click();
        harness.run_steps(3);
        assert_eq!(
            screen.app.borrow().open.as_ref().unwrap().capture_view,
            Some(CaptureView::Map)
        );
        // Turning capture sharpening off also turns off the area view and dims its buttons.
        harness.get_by_label("캡처 샤픈 켜기").click();
        harness.run_steps(3);
        assert_eq!(
            screen.app.borrow().open.as_ref().unwrap().capture_view,
            None
        );
        assert!(
            harness
                .get_by_label("빨간 표시")
                .accesskit_node()
                .is_disabled()
        );
    }

    #[test]
    fn the_skin_panel_offers_an_amount_or_explains_the_missing_model() {
        let screen = Screen::with_photos("skin");
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        let mut harness = screen.harness();
        harness.run_steps(3);
        // A slider gets the same name in two places: the label and the control bar.
        assert!(harness.query_all_by_label("피부 보정 세기").count() > 0);
        handle(Action::SkinModel(Some(Text::of(
            "피부 보정 모델이 없습니다",
        ))));
        harness.run_steps(3);
        harness.get_by_label("피부 보정: 모델 없음");
        assert_eq!(harness.query_all_by_label("피부 보정 세기").count(), 0);
    }

    #[test]
    fn the_quality_slider_goes_to_the_top_and_says_what_each_range_costs() {
        let screen = Screen::with_photos("quality-hint");
        let mut harness = screen.harness();
        // The default (55) is in the recommended range.
        harness.run_steps(3);
        harness.get_by_label("권장 범위");
        for (quality, hint) in [
            (60, "권장 범위"),
            (70, "용량이 2~3배"),
            (75, "용량이 2~3배"),
            (76, "16MB를 넘으면 자동으로 낮춤"),
            (100, "16MB를 넘으면 자동으로 낮춤"),
        ] {
            screen.app.borrow_mut().handle(Action::SetQuality(quality));
            harness.run_steps(3);
            harness.get_by_label(hint);
        }
        assert_eq!(
            screen
                .app
                .borrow()
                .open
                .as_ref()
                .unwrap()
                .project
                .export
                .quality,
            100
        );
    }

    #[test]
    fn the_log_can_be_opened_from_the_start_screen_and_the_workspace() {
        let screen = Screen::new("open-log");
        screen.env.borrow_mut().path = Some(screen.dir.join("trip.amsil"));
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("로그 열기").click();
        harness.run_steps(3);
        assert_eq!(screen.env.borrow().logs_opened, 1);
        // The screen also says where the file is.
        harness.get_by_label("로그 파일: /tmp/amsil/log.txt");

        harness.get_by_label("새 프로젝트").click();
        harness.run_steps(3);
        harness.get_by_label("로그 열기").click();
        harness.run_steps(3);
        assert_eq!(screen.env.borrow().logs_opened, 2);
        harness.get_by_label("로그 파일: /tmp/amsil/log.txt");
    }

    #[test]
    fn light_and_dark_mode_are_switched_from_either_screen() {
        use crate::project::Theme;
        let screen = Screen::new("theme");
        screen.env.borrow_mut().path = Some(screen.dir.join("trip.amsil"));
        let theme = || screen.app.borrow().config.theme;
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("라이트").click();
        harness.run_steps(3);
        assert_eq!(theme(), Some(Theme::Light));
        assert_eq!(
            harness.ctx.theme(),
            egui::Theme::Light,
            "the screen should turn light"
        );

        harness.get_by_label("새 프로젝트").click();
        harness.run_steps(3);
        assert_eq!(
            harness.ctx.theme(),
            egui::Theme::Light,
            "stays the same in the workspace"
        );
        harness.get_by_label("다크").click();
        harness.run_steps(3);
        assert_eq!(theme(), Some(Theme::Dark));
        assert_eq!(harness.ctx.theme(), egui::Theme::Dark);
    }

    #[test]
    fn buttons_do_not_overlap_in_a_narrow_window() {
        // At the smallest window width (640), in both languages, buttons in the top and bottom rows
        // must not overlap.
        for (language, labels) in [
            (
                crate::lang::Lang::Ko,
                [
                    "폴더 가져오기",
                    "스크린샷",
                    "프로젝트 닫기",
                    "로그 열기",
                    "라이트",
                    "다크",
                    "한국어",
                    "English",
                    "출력 폴더…",
                    "권장 범위",
                    "선택 내보내기",
                    "전체 내보내기",
                ],
            ),
            (
                crate::lang::Lang::En,
                [
                    "Import folder",
                    "Screenshot",
                    "Close project",
                    "Open log",
                    "Light",
                    "Dark",
                    "한국어",
                    "English",
                    "Output folder…",
                    "Recommended",
                    "Export selected",
                    "Export all",
                ],
            ),
        ] {
            let screen = Screen::with_photos("narrow");
            let handle = |action| {
                screen.app.borrow_mut().handle(action);
            };
            handle(Action::SetLanguage(language));
            handle(Action::SetExportFolder(
                "/home/user/Pictures/a rather long folder name".into(),
            ));
            let mut harness = screen.harness_sized(egui::vec2(640.0, 800.0));
            harness.run_steps(4);
            let rects: Vec<(&str, egui::Rect)> = labels
                .iter()
                .map(|label| (*label, harness.get_by_label(label).rect()))
                .collect();
            for (i, (a, first)) in rects.iter().enumerate() {
                assert!(
                    first.min.x >= 0.0 && first.max.x <= 640.0,
                    "{a}: went outside the window {first:?}"
                );
                for (b, second) in &rects[i + 1..] {
                    assert!(
                        !first.shrink(1.0).intersects(second.shrink(1.0)),
                        "{a} and {b} overlap: {first:?} {second:?}"
                    );
                }
            }
        }
    }

    /// How many places on screen contain this text.
    fn shown(harness: &Harness<'_>, text: &str) -> usize {
        harness.query_all_by_label_contains(text).count()
    }

    #[test]
    fn the_ai_info_window_says_where_every_model_comes_from() {
        let screen = Screen::new("ai-info");
        screen.env.borrow_mut().path = Some(screen.dir.join("trip.amsil"));
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        // Only the mask model is missing
        handle(Action::Backend(Backend::Gpu));
        handle(Action::Segmenter(Some(Text::of("마스크 모델이 없습니다"))));
        let mut harness = screen.harness();
        harness.run_steps(3);
        // It is available from the start screen too.
        harness.get_by_label("AI 정보").click();
        harness.run_steps(3);
        harness.get_by_label("AI 기능과 출처");
        for text in [
            "RawNIND UtNet2",
            "Benoit Brummer (UCLouvain)",
            "GPL-3.0",
            "Segment Anything 2.1",
            "Meta",
            "CLIP ViT-B/32",
            "OpenAI",
            "MediaPipe selfie multiclass",
            "Google",
            "https://github.com/facebookresearch/sam2",
            "https://arxiv.org/abs/2103.00020",
        ] {
            assert!(shown(&harness, text) > 0, "not in the window: {text}");
        }
        // It also shows which models are ready and which are missing.
        assert_eq!(shown(&harness, "모델 없음"), 1);
        assert!(shown(&harness, "준비됨") >= 3);
    }

    #[test]
    fn each_ai_panel_names_the_model_behind_it() {
        let screen = Screen::with_photos("ai-lines");
        screen.app.borrow_mut().handle(Action::Select {
            index: 0,
            extend: false,
        });
        let mut harness = screen.harness();
        harness.run_steps(3);
        for text in [
            "CLIP ViT-B/32 · OpenAI",
            "RawNIND UtNet2 · Benoit Brummer (UCLouvain)",
            "Segment Anything 2.1 (Hiera Small) · Meta",
            "MediaPipe selfie multiclass 256×256 · Google",
        ] {
            assert_eq!(shown(&harness, text), 1, "{text}");
        }
    }

    /// Draws the sharpness features (two sharpened-area views, camera JPEG comparison) with a real
    /// photo into `target/ui/`.
    /// `cargo test -- --ignored render_capture_views`
    #[test]
    #[ignore = "needs sample photos"]
    fn render_capture_views() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = root.join("DCIM/10361110/DSC04551.ARW");
        let out = root.join("target/ui");
        std::fs::create_dir_all(&out).unwrap();
        let read = crate::export::read(&source, None, &|_| {}).unwrap();
        let loaded = read.sharpened(&crate::capture::Capture::default());
        let params = crate::edit::Params::default();
        for (name, view) in [
            ("capture-overlay", Some(CaptureView::Overlay)),
            ("capture-map", Some(CaptureView::Map)),
            ("capture-jpeg", None),
        ] {
            let screen = Screen::new(name);
            let handle = |action| {
                screen.app.borrow_mut().handle(action);
            };
            let folder = screen.dir.join("건물");
            std::fs::create_dir_all(&folder).unwrap();
            for file in ["DSC04551.ARW", "DSC04551.JPG"] {
                std::os::unix::fs::symlink(source.with_file_name(file), folder.join(file)).unwrap();
            }
            handle(Action::NewProject(screen.dir.join("건물.amsil")));
            handle(Action::ImportFolder(folder.clone()));
            handle(Action::Select {
                index: 0,
                extend: false,
            });
            if view.is_some() {
                handle(Action::SetCaptureView(view));
            } else {
                handle(Action::SetCompare(true));
                handle(Action::SetCompareBefore(crate::project::Before::Jpeg));
                let generation = screen
                    .app
                    .borrow()
                    .open
                    .as_ref()
                    .unwrap()
                    .original_generation();
                handle(Action::OriginalReady {
                    generation,
                    index: 0,
                    result: Ok(
                        crate::worker::camera_jpeg(&folder.join("DSC04551.JPG"), 4096).unwrap(),
                    ),
                });
            }
            let look = crate::export::Look {
                capture_view: view,
                ..crate::export::Look::new(&params)
            };
            let image = loaded.develop(&loaded.cam, look, &crate::export::Areas::default());
            let generation = screen
                .app
                .borrow()
                .open
                .as_ref()
                .unwrap()
                .preview_generation();
            handle(Action::PreviewReady {
                generation,
                index: 0,
                result: Ok(crate::preview::shrink(&image, 4096)),
            });
            let mut harness = screen.harness_sized(egui::vec2(1400.0, 1000.0));
            harness.run_steps(6);
            let rendered = harness.render().unwrap();
            rendered.save(out.join(format!("{name}.png"))).unwrap();
        }
    }

    /// Makes the screen images for the READMEs: `assets/screenshots/workspace.jpg` for the Korean
    /// README,
    /// `workspace.en.jpg` for the English one.
    /// `cargo test -- --ignored render_readme_screenshot`
    ///
    /// The images go into a public repository. So only landscape, architecture and food photos with
    /// no people are used, and the paths shown on screen are not real ones.
    #[test]
    #[ignore = "needs sample photos, and rewrites image files in the repository"]
    fn render_readme_screenshot() {
        render_readme_screenshot_in(Lang::Ko, "가을 여행", "workspace.jpg");
        render_readme_screenshot_in(Lang::En, "Autumn trip", "workspace.en.jpg");
    }

    fn render_readme_screenshot_in(lang: Lang, album: &str, file_name: &str) {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let source = root.join("DCIM/10361110");
        let chosen = [
            "DSC04545.ARW", // autumn forest path (shown in the preview)
            "DSC04546.ARW", // hill
            "DSC04549.ARW", // pine twigs
            "DSC04551.ARW", // buildings
            "DSC04553.ARW", // eaves and building
            "DSC04554.ARW", // stone path
            "DSC04558.ARW", // french fries
            "DSC04559.ARW", // hamburger
            "DSC04560.ARW", // french fries
        ];
        let screen = Screen::new(&format!("readme-{file_name}"));
        let handle = |action| {
            screen.app.borrow_mut().handle(action);
        };
        handle(Action::SetLanguage(lang));
        // Make a folder holding only the chosen photos and import it.
        let folder = screen.dir.join(album);
        std::fs::create_dir_all(&folder).unwrap();
        for name in chosen {
            std::os::unix::fs::symlink(source.join(name), folder.join(name)).unwrap();
        }
        handle(Action::NewProject(
            screen.dir.join(format!("{album}.amsil")),
        ));
        handle(Action::ImportFolder(folder));
        handle(Action::Backend(Backend::Gpu));
        handle(Action::SetTheme(Theme::Dark));

        let mut classifier = crate::recommend::load_classifier().ok();
        for index in 0..chosen.len() {
            let path = screen.app.borrow().open.as_ref().unwrap().photo_path(index);
            let picture = crate::preview::thumbnail(&path, 448).unwrap();
            let guess = picture
                .as_ref()
                .zip(classifier.as_mut())
                .map(|(picture, classifier)| classifier.classify(picture).unwrap());
            handle(Action::Thumbnail {
                index,
                pixels: picture.map(|picture| picture.fit_within(160).unwrap()),
                guess,
            });
        }
        // The forest path photo selected with the "풍경" (landscape) preset applied
        handle(Action::Select {
            index: 0,
            extend: false,
        });
        handle(Action::SetPreset(Some("landscape".into())));
        let (path, params) = {
            let app = screen.app.borrow();
            let open = app.open.as_ref().unwrap();
            (open.photo_path(0), open.params(0, &app.presets))
        };
        let rendered = crate::export::render(
            &path,
            None,
            crate::export::Look {
                capture: crate::capture::Capture::default(),
                ..crate::export::Look::new(&params)
            },
            &|_| {},
        )
        .unwrap();
        let generation = screen
            .app
            .borrow()
            .open
            .as_ref()
            .unwrap()
            .preview_generation();
        handle(Action::PreviewReady {
            generation,
            index: 0,
            result: Ok(crate::preview::shrink(&rendered.image, 8192)),
        });
        handle(Action::SetExportFolder(
            format!("/home/user/Pictures/{album}").into(),
        ));

        let out = root.join("assets/screenshots");
        std::fs::create_dir_all(&out).unwrap();
        // Draw the 1100×700 window at 1.5×, so the text stays crisp even when shown smaller.
        let scale = 1.5;
        let mut harness = screen.harness_scaled(egui::vec2(1100.0, 700.0), scale);
        harness.run_steps(6);
        let rendered = harness.render().unwrap();
        // Crop the margin (8 points) the test environment leaves around it, and save as a small
        // JPEG.
        let margin = (8.0 * scale) as u32;
        let (width, height) = (
            rendered.width() - 2 * margin,
            rendered.height() - 2 * margin,
        );
        let cropped =
            image::imageops::crop_imm(&rendered, margin, margin, width, height).to_image();
        let rgb = image::DynamicImage::ImageRgba8(cropped).into_rgb8();
        let file = std::fs::File::create(out.join(file_name)).unwrap();
        image::codecs::jpeg::JpegEncoder::new_with_quality(file, 90)
            .encode_image(&rgb)
            .unwrap();
    }
}
