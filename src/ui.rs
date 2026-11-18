//! 화면 그리기. 상태(`App`)를 읽어 그리고, 사용자가 한 일을 `Action`으로 돌려준다. 로직은 여기에 두지 않는다.

use crate::app::{Action, App, Backend, ExportState, Open, ReportView};
use crate::frame::{Frame, Layout as Shape};
use crate::job::{describe_remaining, describe_resources};
use crate::lang::{self, Lang, t};
use crate::preview::Pixels;
use crate::recommend;
use eframe::egui::{self, Align, Key, Layout, Panel, RichText, TextureHandle, TextureOptions};
use std::collections::HashMap;
use std::path::PathBuf;

/// 화면이 바깥 세계에 부탁하는 일. 테스트에서는 가짜로 바꿔 끼운다.
pub trait Env {
    /// 새 프로젝트 파일을 저장할 위치를 묻는다.
    fn new_project_path(&mut self) -> Option<PathBuf>;
    /// 열 프로젝트 파일을 묻는다.
    fn open_project_path(&mut self) -> Option<PathBuf>;
    /// 폴더를 묻는다.
    fn pick_folder(&mut self) -> Option<PathBuf>;
    /// LUT(.cube) 파일을 묻는다.
    fn pick_lut(&mut self) -> Option<PathBuf>;
    /// 지금 창을 갈무리한다. 결과는 나중에 `Action::SaveScreenshot`으로 도착한다.
    fn screenshot(&mut self, ctx: &egui::Context);
    /// 로그 파일을 연다. 화면에 알릴 말(파일의 위치, 또는 열지 못한 까닭)을 돌려준다.
    fn open_log(&mut self) -> String;
}

/// 화면 쪽에서만 쓰는 상태: GPU에 올린 그림과 창의 열림 여부.
#[derive(Default)]
pub struct View {
    /// 텍스처가 어느 프로젝트의 것인지. 프로젝트가 바뀌면 버린다.
    project: Option<PathBuf>,
    thumbnails: HashMap<usize, TextureHandle>,
    /// (요청 번호, 텍스처)
    preview: Option<(u64, TextureHandle)>,
    show_backend_help: bool,
    /// 끌고 있는 동안의 품질 슬라이더 값.
    quality: Option<u8>,
    /// 마지막으로 설정한 창 제목.
    title: String,
    /// 미리보기의 확대 상태와, 그것이 어느 사진의 것인지.
    zoom: Zoom,
    zoomed_photo: Option<usize>,
}

/// 원본 화소 하나를 화면 몇 점까지 키울 수 있는지 (4 = 400%).
const MAX_ZOOM: f32 = 4.0;
/// 휠 한 칸에 얼마나 확대할지 정하는 값.
const WHEEL_ZOOM: f32 = 0.005;

/// 한글 글꼴(나눔고딕)을 등록한다. egui 내장 글꼴에는 한글이 없어서, 시스템에 한글 글꼴이 없는
/// 컴퓨터에서는 글자가 네모로 나온다. 글꼴을 프로그램에 넣어 어디서나 같은 화면이 나오게 한다.
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

/// 한 프레임을 그리고, 이번 프레임에 사용자가 한 일을 돌려준다.
pub fn draw(ui: &mut egui::Ui, app: &App, view: &mut View, env: &mut dyn Env) -> Vec<Action> {
    let mut actions = Vec::new();
    if ui.input(|input| input.key_pressed(Key::F12)) {
        env.screenshot(ui.ctx());
    }
    match &app.open {
        None => {
            *view = View::default();
            start_screen(ui, app, env, &mut actions);
        }
        Some(open) => {
            if view.project.as_ref() != Some(&open.path) {
                *view = View {
                    project: Some(open.path.clone()),
                    ..View::default()
                };
            }
            workspace(ui, app, open, view, env, &mut actions);
        }
    }
    actions
}

/// 프로그램 이름.
fn app_name() -> &'static str {
    t("암실", "Amsil")
}

/// 언어를 고르는 단추. 지금 언어를 못 읽는 사람도 찾을 수 있게, 각 언어의 이름은 그 언어로 적는다.
/// 오른쪽부터 채우는 자리에 놓으므로 뒤의 것이 왼쪽에 온다 (화면에는 "한국어 English" 순서).
fn language_buttons(ui: &mut egui::Ui, actions: &mut Vec<Action>) {
    for (choice, name) in [(Lang::En, "English"), (Lang::Ko, "한국어")] {
        let chosen = lang::current() == choice;
        if ui.selectable_label(chosen, name).clicked() && !chosen {
            actions.push(Action::SetLanguage(choice));
        }
    }
}

/// 문제가 생겼을 때 볼 로그 파일을 여는 단추.
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

fn start_screen(ui: &mut egui::Ui, app: &App, env: &mut dyn Env, actions: &mut Vec<Action>) {
    Panel::top("language").show(ui, |ui| {
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            language_buttons(ui, actions);
            ui.separator();
            log_button(ui, env, actions);
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
    // 내보내는 동안에는 창 제목에도 진행률을 보여 준다 (최소화해도 보인다).
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
    // 제목이 바뀔 때만 보낸다. 매번 보내면 화면이 쉬지 않고 다시 그려진다.
    if view.title != title {
        ui.ctx()
            .send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
        view.title = title;
    }

    Panel::top("top").show(ui, |ui| {
        ui.horizontal(|ui| {
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
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                language_buttons(ui, actions);
                ui.separator();
                log_button(ui, env, actions);
                ui.separator();
                backend_label(ui, app, view)
            });
        });
    });

    Panel::bottom("status").show(ui, |ui| status_bar(ui, app, open));
    Panel::bottom("export").show(ui, |ui| export_bar(ui, open, view, env, actions));

    // 창이 세로로 길면 편집 패널을 아래로 내린다.
    let portrait = ui.available_width() < ui.available_height();
    if portrait {
        Panel::bottom("edit").show(ui, |ui| edit_panel(ui, app, open, env, actions));
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
    egui::CentralPanel::default().show(ui, |ui| preview(ui, open, view, actions));

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
        Backend::Cpu(notice) | Backend::Unavailable(notice) => notice.as_str(),
        _ => t(
            "AI 노이즈 제거를 GPU로 실행합니다.",
            "AI noise reduction runs on the GPU.",
        ),
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

/// 고른 품질이 무엇을 뜻하는지 한마디로. 기준은 2,400만 화소 사진으로 잰 용량이다.
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
    ui.horizontal(|ui| {
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
        // 품질은 끝까지 올릴 수 있게 두고, 값마다 무엇을 치르는지 옆에 알려 준다.
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
        // 슬라이더에 이름을 붙여 둔다 (화면 낭독기와 테스트가 이 이름으로 찾는다).
        let slider = ui
            .add_enabled(
                !exporting,
                egui::Slider::new(&mut quality, crate::export::QUALITY_RANGE)
                    .text(t("품질", "Quality")),
            )
            .on_hover_text(guide);
        ui.weak(quality_hint(quality)).on_hover_text(guide);
        // 끄는 동안에는 화면에서만 바꾸고, 놓을 때 한 번 저장한다.
        if slider.dragged() {
            view.quality = Some(quality);
        } else if slider.drag_stopped() || slider.changed() {
            view.quality = None;
            actions.push(Action::SetQuality(quality));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if exporting {
                if ui.button(t("취소", "Cancel")).clicked() {
                    actions.push(Action::CancelExport);
                }
            } else {
                if ui.button(t("전체 내보내기", "Export all")).clicked() {
                    actions.push(Action::Export {
                        only_selected: false,
                    });
                }
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
            }
        });
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
            denoise_panel(ui, app, open, actions);
        });
}

/// 슬라이더 하나. 값이 바뀌면 `Some(놓았는가)`를 돌려준다. 끄는 동안에는 거짓, 놓을 때 참이다.
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
    if response.drag_stopped() {
        Some(true)
    } else if response.changed() {
        // 끌지 않고 바뀐 경우(숫자를 직접 입력, 키보드)는 바로 확정한다.
        Some(!response.dragged())
    } else {
        None
    }
}

/// 프리셋, 강도, 세부 조정, LUT.
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
        // 지금 사진의 내용에 맞는 프리셋에는 별표를 붙인다.
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
            .on_hover_text(problem);
    }
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

/// 색상대의 화면 이름. `edit::Hsl`의 항목과 같은 순서.
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

/// 피부 보정 (실험 기능): 인물의 피부만 찾아 얼룩을 누그러뜨린다.
fn skin_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.heading(t("피부 보정 (실험)", "Skin retouch (experimental)"));
    let Some(current) = open.current else {
        ui.weak(t(
            "사진을 고르면 인물의 피부를 다듬을 수 있습니다",
            "Pick a photo to retouch skin in portraits",
        ));
        return;
    };
    if let Some(problem) = &app.skin_problem {
        ui.weak(t("피부 보정: 모델 없음", "Skin retouch: no model"))
            .on_hover_text(problem);
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

/// 마스크: 사진의 일부분을 골라 그곳에만 보정을 더한다.
fn mask_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.heading(t("마스크", "Masks"));
    let Some(current) = open.current else {
        ui.weak(t(
            "사진을 고르면 일부분만 따로 보정할 수 있습니다",
            "Pick a photo to adjust just a part of it",
        ));
        return;
    };
    if let Some(problem) = &app.mask_problem {
        ui.weak(t("마스크: 모델 없음", "Masks: no model"))
            .on_hover_text(problem);
        return;
    }
    let masks = open.edit(current).masks;
    ui.horizontal_wrapped(|ui| {
        for index in 0..masks.len() {
            let number = index + 1;
            let name = t(format!("마스크 {number}"), format!("Mask {number}"));
            let active = open.mask == Some(index);
            if ui.selectable_label(active, name).clicked() {
                // 고쳐 쓰는 마스크를 다시 누르면 고쳐 쓰기를 마친다.
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
    // 고른 곳에만 더할 보정
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

/// 미리보기에서 누른 곳이 사진의 어디인지. 사진의 왼쪽 위가 (0, 0), 오른쪽 아래가 (1, 1)이다.
/// `rect`는 화면에서 사진이 그려진 자리다. 사진 밖을 눌렀으면 None.
fn photo_point(rect: egui::Rect, pos: egui::Pos2) -> Option<(f32, f32)> {
    rect.contains(pos).then(|| {
        (
            (pos.x - rect.min.x) / rect.width(),
            (pos.y - rect.min.y) / rect.height(),
        )
    })
}

/// 프레임의 모양과 색.
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

/// 사진 목록의 한 줄 높이.
const ROW: f32 = 56.0;

fn photo_list(ui: &mut egui::Ui, open: &Open, view: &mut View, actions: &mut Vec<Action>) {
    if open.photos.is_empty() {
        ui.weak(t(
            "[폴더 가져오기]로 사진 폴더를 추가하세요",
            "Add a photo folder with [Import folder]",
        ));
        return;
    }
    // 위아래 화살표로 앞뒤 사진으로 간다.
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
                        // 무엇을 찍은 사진으로 보이는지 이름 옆에 적는다 (확실할 때만).
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

/// 미리보기의 확대 상태.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// 1이면 창에 맞춘 크기, 2면 그 두 배.
    pub scale: f32,
    /// 이미지 중심이 미리보기 영역의 중심에서 벗어난 거리 (화면 단위).
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
    /// `factor`배로 확대하되, `pointer`(영역 중심 기준 위치) 아래의 이미지 지점이 제자리에 있게 한다.
    /// `max`는 최대 배율.
    pub fn zoom_at(&mut self, factor: f32, pointer: egui::Vec2, max: f32) {
        let scale = (self.scale * factor).clamp(1.0, max.max(1.0));
        let applied = scale / self.scale;
        self.offset = pointer - (pointer - self.offset) * applied;
        self.scale = scale;
    }

    /// 이미지가 영역보다 크면 가장자리가 영역 안으로 들어오지 않게, 작으면 가운데에 오게 한다.
    /// `fit`은 창에 맞춘 이미지 크기, `area`는 미리보기 영역의 크기.
    pub fn clamp(&mut self, fit: egui::Vec2, area: egui::Vec2) {
        let limit = ((fit * self.scale - area) / 2.0).max(egui::Vec2::ZERO);
        self.offset = self.offset.clamp(-limit, limit);
    }
}

fn preview(ui: &mut egui::Ui, open: &Open, view: &mut View, actions: &mut Vec<Action>) {
    let Some(current) = open.current else {
        ui.centered_and_justified(|ui| {
            ui.weak(t("왼쪽에서 사진을 고르세요", "Pick a photo on the left"))
        });
        return;
    };
    let shown = open
        .preview
        .as_ref()
        .filter(|preview| preview.index == current);
    if let Some(preview) = shown
        && view
            .preview
            .as_ref()
            .is_none_or(|(generation, _)| *generation != preview.serial)
    {
        // 줄여서 볼 때는 부드럽게(밉맵), 100%를 넘겨 볼 때는 화소가 그대로 보이게 한다.
        let options = TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            mipmap_mode: Some(egui::TextureFilter::Linear),
            ..TextureOptions::LINEAR
        };
        // GPU가 받을 수 있는 크기를 넘으면 줄여서 올린다 (넘는 텍스처를 올리면 프로그램이 멈춘다).
        let limit = ui.ctx().input(|input| input.max_texture_side);
        let image = match preview.pixels.fit_within(limit) {
            Some(smaller) => color_image(&smaller),
            None => color_image(&preview.pixels),
        };
        let texture = ui.ctx().load_texture("preview", image, options);
        view.preview = Some((preview.serial, texture));
    }
    match (shown, &view.preview) {
        (Some(_), Some((_, texture))) => {
            // 다른 사진으로 넘어가면 창에 맞춘 상태로 돌아간다. 같은 사진을 다시 그린 경우(노이즈 제거 적용 등)에는
            // 보던 자리를 유지한다.
            if view.zoomed_photo != Some(current) {
                view.zoomed_photo = Some(current);
                view.zoom = Zoom::default();
            }
            let area = ui.available_rect_before_wrap();
            let response = ui.allocate_rect(area, egui::Sense::click_and_drag());
            let image = texture.size_vec2();
            let fit_size = fit(image, area.size());
            // 창에 맞춘 크기를 1로 볼 때 원본 크기(100%)에 해당하는 배율
            let actual = image.x / fit_size.x;
            let max = (MAX_ZOOM * actual).max(1.0);
            let pointer = response.hover_pos().map(|pos| pos - area.center());
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
            // 마스크를 고쳐 쓰는 동안에는 누르는 것이 자리를 찍는 일이라, 두 번 눌러 확대하는 것은 쉰다.
            if response.double_clicked() && open.mask.is_none() {
                // 창에 맞춤 ↔ 100%
                if view.zoom.scale == 1.0 {
                    view.zoom.zoom_at(actual, pointer.unwrap_or_default(), max);
                } else {
                    view.zoom = Zoom::default();
                }
            }
            view.zoom.clamp(fit_size, area.size());
            let rect = egui::Rect::from_center_size(
                area.center() + view.zoom.offset,
                fit_size * view.zoom.scale,
            );
            let mut canvas = ui.new_child(egui::UiBuilder::new().max_rect(area));
            canvas.set_clip_rect(area);
            egui::Image::new(texture).paint_at(&canvas, rect);
            if let Some(active) = open.mask {
                // 사진을 누른 자리가 마스크에 더해진다. 오른쪽 단추로 누르면 빼는 자리다.
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
                // 지금까지 누른 자리를 사진 위에 점으로 보여 준다 (고르는 자리는 초록, 빼는 자리는 빨강).
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
            // 지금 배율 (원본 화소 대비)
            let percent = view.zoom.scale / actual * 100.0;
            let corner = egui::Rect::from_min_max(
                area.right_bottom() - egui::vec2(90.0, 24.0),
                area.right_bottom(),
            );
            // 사용법은 배율 표시에 마우스를 올렸을 때만 보여 준다. 사진 위에 계속 뜨면 거슬린다.
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
        _ => {
            ui.centered_and_justified(|ui| ui.spinner());
        }
    }
}

/// 비율을 유지하며 `area` 안에 들어가는 크기. 원본보다 키우지는 않는다.
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

    /// 포인터 아래에 있는 이미지 지점 (이미지 중심 기준, 창에 맞춘 크기 단위).
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

        // 이미 확대하고 옮긴 상태에서 한 번 더
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
        assert_eq!(zoom, Zoom::default(), "창에 맞춘 크기보다 작아지지 않는다");
        zoom.zoom_at(100.0, vec2(0.0, 0.0), 8.0);
        assert_eq!(zoom.scale, 8.0);
    }

    #[test]
    fn panning_stops_at_the_image_edges() {
        // 400×300 영역에 꽉 맞는 이미지를 2배로: 800×600. 좌우 200, 상하 150까지만 움직일 수 있다.
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
        // 세로 사진: 영역 400×300 안에 200×300으로 맞춰짐. 1.5배면 300×450 → 좌우는 아직 영역보다 좁다.
        let mut zoom = Zoom {
            scale: 1.5,
            offset: vec2(80.0, 500.0),
        };
        zoom.clamp(vec2(200.0, 300.0), vec2(400.0, 300.0));
        assert_eq!(zoom.offset, vec2(0.0, 75.0));
    }

    /// 대화 상자 대신 정해 둔 경로를 돌려주는 가짜 환경.
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

    /// 화면을 띄우고, 그 화면에서 나온 동작을 앱에 전달하는 시험대.
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

        /// 프로젝트를 만들고 사진 두 장을 가져온 상태.
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
            let mut view = View::default();
            let mut fonts_installed = false;
            Harness::builder()
                .with_size(egui::vec2(1100.0, 700.0))
                .build_ui(move |ui| {
                    // 글꼴은 다음 프레임부터 적용된다. 첫 프레임에는 아무것도 그리지 않는다.
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
            _ => "기타".into(),
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
        // 시작 화면의 최근 프로젝트에서 다시 연다.
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
        // 미리보기를 기다리는 동안에는 회전 표시가 계속 돌아서 화면이 멈추지 않는다.
        harness.run_steps(3);
        assert_eq!(screen.app.borrow().open.as_ref().unwrap().current, Some(1));
        assert_eq!(*screen.seen.borrow(), ["ImportFolder", "Select 1 false"]);
    }

    /// 미리보기 구석에 표시된 배율(%)을 읽는다.
    fn shown_zoom(harness: &Harness<'_>) -> f32 {
        use egui_kittest::kittest::NodeT;
        let node = harness.get_by_label_contains("배율 ").accesskit_node();
        let text = node
            .label()
            .or_else(|| node.value())
            .expect("배율 표시에 글자가 없다");
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
            "2000화소 폭의 사진이 창에 맞게 줄어 있어야 한다: {fit}%"
        );

        // 미리보기 위에서 휠을 올린다
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

        // 아무리 올려도 400%를 넘지 않는다
        for _ in 0..40 {
            harness.event(wheel(400.0));
            harness.run();
        }
        assert_eq!(shown_zoom(&harness), 400.0);

        // 아무리 내려도 창에 맞춘 크기보다 작아지지 않는다
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
        // 시험 환경의 텍스처 한도(2048)보다 큰 미리보기
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
        // 미리보기가 도착한 상태로 만든다 (도착 전에는 회전 표시가 계속 돈다).
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
        let mut harness = screen.harness();
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
        screen.app.borrow_mut().handle(Action::Backend(Backend::Cpu(
            "Vulkan 드라이버를 설치하세요".into(),
        )));
        let mut harness = screen.harness();
        harness.run();
        harness.get_by_label("CPU (느림)").click();
        harness.run();
        harness.get_by_label("Vulkan 드라이버를 설치하세요");
    }

    /// 눈으로 확인하기 위한 화면 그림을 `target/ui/`에 남긴다. `cargo test -- --ignored render`
    #[test]
    #[ignore = "GPU와 샘플 사진이 필요하고, 그림 파일을 남기는 용도"]
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

        // 실제 사진 폴더를 가져오고, 썸네일과 미리보기를 직접 만들어 넣는다.
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

        // 휠로 확대한 모습
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

        // 프리셋을 적용한 모습 ("풍경", 강도 120%)
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
        handle(Action::ResetLook);

        // LUT을 입힌 모습 (따뜻하고 채도가 낮은 시험용 LUT, 강도 80%)
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

        // 마스크: 가운데 건물을 눌러 고른 모습 (모델이 있을 때)
        if let Ok(segmenter) = crate::mask::load_segmenter() {
            let segmenter = std::sync::Mutex::new(segmenter);
            handle(Action::AddMask);
            handle(Action::AddMaskPoint {
                x: 0.48,
                y: 0.35,
                include: true,
            });
            let masks = screen.app.borrow().open.as_ref().unwrap().edit(7).masks;
            let loaded = crate::export::load(&path, None, &|_| {}).unwrap();
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

        // 내보내는 중
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

        // 결과
        let outcome = Outcome {
            done: count - 1,
            skipped: 0,
            failed: vec![(
                "DSC04560.ARW".into(),
                "지원하지 않거나 손상된 RAW 파일입니다".into(),
            )],
            stopped: None,
        };
        handle(Action::ExportFinished(Ok((outcome, vec![]))));
        shot(&mut harness, "4-finished.png");

        // 영어 화면: 작업 화면(프리셋·LUT·세부 조정을 펼친 모습)과 시작 화면
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
        handle(Action::CloseProject);
        shot(&mut harness, "5b-english-start.png");
    }

    /// 화면에 나온 글자 가운데 한글이 든 것. 언어 단추의 "한국어"는 뺀다.
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
            "한국어 화면에서는 한글이 보여야 한다 (검사가 글자를 제대로 읽는지 확인)"
        );
        handle(Action::SetLanguage(Lang::En));
        let check = |harness: &mut Harness<'_>, what: &str| {
            harness.run_steps(4);
            let left = korean_texts(harness);
            assert!(left.is_empty(), "{what}: {left:?}");
        };
        check(&mut harness, "시작 화면");

        handle(Action::OpenProject(screen.dir.join("trip.amsil")));
        check(&mut harness, "빈 작업 화면");
        handle(Action::ImportFolder(screen.dir.join("사진")));
        handle(Action::Backend(Backend::Cpu("no Vulkan".into())));
        check(&mut harness, "사진을 고르기 전");

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
        check(&mut harness, "편집 패널을 다 펼친 모습");
        handle(Action::AddMask);
        handle(Action::AddMaskPoint {
            x: 0.5,
            y: 0.5,
            include: true,
        });
        check(&mut harness, "마스크를 고치는 중");
        handle(Action::SelectMask(None));
        handle(Action::Segmenter(Some("no model".into())));
        handle(Action::SkinModel(Some("no model".into())));
        check(&mut harness, "마스크와 피부 모델이 없을 때");
        handle(Action::Segmenter(None));
        handle(Action::SkinModel(None));
        harness.get_by_label("CPU (slow)").click();
        check(&mut harness, "CPU 안내");
        handle(Action::Backend(Backend::Unavailable("no model".into())));
        check(&mut harness, "모델이 없을 때");
        handle(Action::Backend(Backend::Unknown));
        check(&mut harness, "AI 준비 중");
        handle(Action::Recommender(Some("no model".into())));
        check(&mut harness, "분류 모델이 없을 때");

        // 내보내기: 확인 창, 진행 중, 결과, 실패
        let out = screen.dir.join("out");
        std::fs::create_dir_all(out.join("사진")).unwrap();
        std::fs::write(out.join("사진/a.heic"), b"old").unwrap();
        handle(Action::SetExportFolder(out));
        handle(Action::Export {
            only_selected: false,
        });
        check(&mut harness, "이미 있는 파일 확인");
        handle(Action::ConfirmExport { overwrite: false });
        check(&mut harness, "내보내기 준비 중");
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
        check(&mut harness, "내보내는 중");
        handle(Action::ExportProgress(report(45.0)));
        check(&mut harness, "진행이 멈춘 듯할 때");
        let outcome = Outcome {
            done: 1,
            skipped: 0,
            failed: vec![("b.ARW".into(), "broken".into())],
            stopped: Some("disk full".into()),
        };
        let warnings = vec![("a.ARW".into(), "quality lowered".into())];
        handle(Action::ExportFinished(Ok((outcome, warnings))));
        check(&mut harness, "내보내기 결과");
        handle(Action::ExportFinished(Err("boom".into())));
        check(&mut harness, "내보내기 실패");
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
            "모양을 바꿔도 색은 그대로"
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
            "분류된 사진이 없으면 단추도 없다"
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
        // 목록에는 사진의 종류가, 프리셋 단추에는 추천 표시가 붙는다.
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
        handle(Action::Recommender(Some(
            "사진 분류 모델이 없습니다".into(),
        )));
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("프리셋 추천: 모델 없음");
    }

    #[test]
    fn the_bundled_font_has_the_symbols_the_screen_uses() {
        use ab_glyph::Font;
        // 글꼴에 없는 글자는 화면에 네모로 나온다.
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
        assert_eq!(photo_point(rect, egui::pos2(99.0, 100.0)), None, "사진 밖");
        assert_eq!(photo_point(rect, egui::pos2(300.0, 251.0)), None);
    }

    /// 화면의 한 곳을 마우스 단추로 누른다.
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
        // 마스크를 고쳐 쓰는 중이 아니면 눌러도 점이 생기지 않는다.
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
        handle(Action::Segmenter(Some("마스크 모델이 없습니다".into())));
        let mut harness = screen.harness();
        harness.run_steps(3);
        harness.get_by_label("마스크: 모델 없음");
        assert!(harness.query_by_label("새 마스크").is_none());
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
        // 슬라이더는 이름표와 조절 막대 두 군데에 같은 이름이 붙는다.
        assert!(harness.query_all_by_label("피부 보정 세기").count() > 0);
        handle(Action::SkinModel(Some("피부 보정 모델이 없습니다".into())));
        harness.run_steps(3);
        harness.get_by_label("피부 보정: 모델 없음");
        assert_eq!(harness.query_all_by_label("피부 보정 세기").count(), 0);
    }

    #[test]
    fn the_quality_slider_goes_to_the_top_and_says_what_each_range_costs() {
        let screen = Screen::with_photos("quality-hint");
        let mut harness = screen.harness();
        // 기본값(55)은 권장 범위다.
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
        // 파일이 어디에 있는지 화면에도 알려 준다.
        harness.get_by_label("로그 파일: /tmp/amsil/log.txt");

        harness.get_by_label("새 프로젝트").click();
        harness.run_steps(3);
        harness.get_by_label("로그 열기").click();
        harness.run_steps(3);
        assert_eq!(screen.env.borrow().logs_opened, 2);
        harness.get_by_label("로그 파일: /tmp/amsil/log.txt");
    }
}
