//! 화면 그리기. 상태(`App`)를 읽어 그리고, 사용자가 한 일을 `Action`으로 돌려준다. 로직은 여기에 두지 않는다.

use crate::app::{Action, App, Backend, ExportState, Open, ReportView};
use crate::job::describe_remaining;
use crate::preview::Pixels;
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
    /// 지금 창을 갈무리한다. 결과는 나중에 `Action::SaveScreenshot`으로 도착한다.
    fn screenshot(&mut self, ctx: &egui::Context);
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

const GB: f64 = (1u64 << 30) as f64;

/// 원본 화소 하나를 화면 몇 점까지 키울 수 있는지 (4 = 400%).
const MAX_ZOOM: f32 = 4.0;
/// 휠 한 칸에 얼마나 확대할지 정하는 값.
const WHEEL_ZOOM: f32 = 0.005;

/// 한글 글꼴(나눔고딕)을 등록한다. egui 내장 글꼴에는 한글이 없어서, 시스템에 한글 글꼴이 없는
/// 컴퓨터에서는 글자가 네모로 나온다. 글꼴을 프로그램에 넣어 어디서나 같은 화면이 나오게 한다.
pub fn install_fonts(ctx: &egui::Context) {
    const NANUM_GOTHIC: &[u8] = include_bytes!("../assets/fonts/NanumGothic-Regular.ttf");
    let mut fonts = egui::FontDefinitions::default();
    fonts.font_data.insert(
        "NanumGothic".into(),
        egui::FontData::from_static(NANUM_GOTHIC).into(),
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

fn start_screen(ui: &mut egui::Ui, app: &App, env: &mut dyn Env, actions: &mut Vec<Action>) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.2);
            ui.heading("암실");
            ui.label("RAW 사진을 HEIF로");
            ui.add_space(24.0);
            if ui.button("새 프로젝트").clicked()
                && let Some(path) = env.new_project_path()
            {
                actions.push(Action::NewProject(path));
            }
            if ui.button("프로젝트 열기").clicked()
                && let Some(path) = env.open_project_path()
            {
                actions.push(Action::OpenProject(path));
            }
            if !app.config.recent.is_empty() {
                ui.add_space(24.0);
                ui.label("최근 프로젝트");
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
                "{:.0}% · {} · 암실",
                report.progress / (*total).max(1) as f64 * 100.0,
                open.project.name
            )
        }
        _ => format!("{} · 암실", open.project.name),
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
            if ui.button("폴더 가져오기").clicked()
                && let Some(folder) = env.pick_folder()
            {
                actions.push(Action::ImportFolder(folder));
            }
            if ui
                .button("스크린샷")
                .on_hover_text("지금 창을 PNG로 저장 (F12)")
                .clicked()
            {
                env.screenshot(ui.ctx());
            }
            if ui
                .add_enabled(!exporting, egui::Button::new("프로젝트 닫기"))
                .clicked()
            {
                actions.push(Action::CloseProject);
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                backend_label(ui, app, view)
            });
        });
    });

    Panel::bottom("status").show(ui, |ui| status_bar(ui, app, open));
    Panel::bottom("export").show(ui, |ui| export_bar(ui, open, view, env, actions));

    // 창이 세로로 길면 편집 패널을 아래로 내린다.
    let portrait = ui.available_width() < ui.available_height();
    if portrait {
        Panel::bottom("edit").show(ui, |ui| edit_panel(ui, app, open, actions));
    } else {
        Panel::right("edit")
            .default_size(240.0)
            .show(ui, |ui| edit_panel(ui, app, open, actions));
    }
    Panel::left("photos")
        .default_size(if portrait { 160.0 } else { 220.0 })
        .show(ui, |ui| {
            photo_list(ui, open, view, actions);
        });
    egui::CentralPanel::default().show(ui, |ui| preview(ui, open, view));

    export_windows(ui.ctx(), open, actions);
    if view.show_backend_help {
        backend_help(ui.ctx(), app, view);
    }
}

fn backend_label(ui: &mut egui::Ui, app: &App, view: &mut View) {
    match &app.backend {
        Backend::Unknown => {
            ui.weak("AI 준비 중");
        }
        Backend::Gpu => {
            ui.label("GPU")
                .on_hover_text("AI 노이즈 제거를 GPU로 실행합니다");
        }
        Backend::Cpu(_) => {
            if ui
                .link("CPU (느림)")
                .on_hover_text("눌러서 해결 방법 보기")
                .clicked()
            {
                view.show_backend_help = true;
            }
        }
        Backend::Unavailable(_) => {
            if ui
                .link("AI 모델 없음")
                .on_hover_text("눌러서 해결 방법 보기")
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
        _ => "AI 노이즈 제거를 GPU로 실행합니다.",
    };
    let mut open = true;
    egui::Window::new("AI 실행 환경")
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
                    ui.weak(format!(
                        "사진 {}장 · 선택 {}장",
                        open.photos.len(),
                        open.selected.len()
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
                    None => (0, 0.0, "준비 중".to_string()),
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
    let mut text = format!(
        "CPU {:.0}% · 메모리 {:.1}/{:.1}GB · GPU {:.0}% · 처리 중 {}장",
        report.cpu,
        report.memory as f64 / GB,
        report.memory_limit as f64 / GB,
        report.gpu,
        report.running
    );
    if report.low_memory {
        text.push_str(" · 메모리가 부족해 동시 처리 수를 줄였습니다");
    }
    text
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
            .add_enabled(!exporting, egui::Button::new("출력 폴더…"))
            .clicked()
            && let Some(folder) = env.pick_folder()
        {
            actions.push(Action::SetExportFolder(folder));
        }
        match &open.project.export.folder {
            Some(folder) => ui.label(folder.display().to_string()),
            None => ui.weak("아직 정하지 않음"),
        };
        ui.separator();
        let mut quality = view.quality.unwrap_or(open.project.export.quality);
        // 슬라이더에 이름을 붙여 둔다 (화면 낭독기와 테스트가 이 이름으로 찾는다).
        let slider = ui.add_enabled(
            !exporting,
            egui::Slider::new(&mut quality, 40..=75).text("품질"),
        );
        // 끄는 동안에는 화면에서만 바꾸고, 놓을 때 한 번 저장한다.
        if slider.dragged() {
            view.quality = Some(quality);
        } else if slider.drag_stopped() || slider.changed() {
            view.quality = None;
            actions.push(Action::SetQuality(quality));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if exporting {
                if ui.button("취소").clicked() {
                    actions.push(Action::CancelExport);
                }
            } else {
                if ui.button("전체 내보내기").clicked() {
                    actions.push(Action::Export {
                        only_selected: false,
                    });
                }
                if ui
                    .add_enabled(
                        !open.selected.is_empty(),
                        egui::Button::new("선택 내보내기"),
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

fn edit_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            look_panel(ui, app, open, actions);
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

/// 프리셋, 강도, 세부 조정.
fn look_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.heading("프리셋");
    let Some(current) = open.current else {
        ui.weak("사진을 고르면 프리셋을 적용할 수 있습니다");
        return;
    };
    let edit = open.edit(current);
    ui.horizontal_wrapped(|ui| {
        if ui.selectable_label(edit.preset.is_none(), "없음").clicked() {
            actions.push(Action::SetPreset(None));
        }
        for preset in &app.presets {
            let chosen = edit.preset.as_deref() == Some(preset.id.as_str());
            if ui.selectable_label(chosen, &preset.name).clicked() {
                actions.push(Action::SetPreset(Some(preset.id.clone())));
            }
        }
    });
    ui.add_enabled_ui(edit.preset.is_some(), |ui| {
        let mut strength = edit.strength;
        if let Some(commit) = slider(ui, "강도", &mut strength, 0.0..=1.5) {
            actions.push(Action::SetStrength {
                value: strength,
                commit,
            });
        }
    });
    ui.collapsing("자세히", |ui| {
        let mut adjust = edit.adjust;
        let mut changed = None;
        let mut row = |ui: &mut egui::Ui, label: &str, value: &mut f32, range| {
            if let Some(commit) = slider(ui, label, value, range) {
                changed = Some(commit);
            }
        };
        row(ui, "노출", &mut adjust.exposure, -3.0..=3.0);
        row(ui, "대비", &mut adjust.contrast, -1.0..=1.0);
        row(ui, "하이라이트", &mut adjust.highlights, -1.0..=1.0);
        row(ui, "그림자", &mut adjust.shadows, -1.0..=1.0);
        row(ui, "색온도", &mut adjust.temp, -1.0..=1.0);
        row(ui, "틴트", &mut adjust.tint, -1.0..=1.0);
        row(ui, "채도", &mut adjust.saturation, -1.0..=1.0);
        row(ui, "생동감", &mut adjust.vibrance, -1.0..=1.0);
        row(ui, "선명도", &mut adjust.clarity, -1.0..=1.0);
        row(ui, "샤픈", &mut adjust.sharpen, -1.0..=1.0);
        row(ui, "비네팅", &mut adjust.vignette, -1.0..=1.0);
        row(ui, "흑백", &mut adjust.mono, 0.0..=1.0);
        ui.collapsing("색상별", |ui| {
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
            for (name, band) in HUE_NAMES.iter().zip(bands) {
                row(ui, &format!("{name} 채도"), &mut band.sat, -1.0..=1.0);
                row(ui, &format!("{name} 밝기"), &mut band.lum, -1.0..=1.0);
            }
        });
        if let Some(commit) = changed {
            actions.push(Action::SetAdjust {
                params: adjust,
                commit,
            });
        }
    });
    if ui
        .button("보정 초기화")
        .on_hover_text("프리셋과 세부 조정을 지웁니다 (노이즈 제거는 그대로)")
        .clicked()
    {
        actions.push(Action::ResetLook);
    }
}

/// 색상대의 화면 이름. `edit::Hsl`의 항목과 같은 순서.
const HUE_NAMES: [&str; 8] = [
    "빨강", "주황", "노랑", "초록", "청록", "파랑", "보라", "자홍",
];

fn denoise_panel(ui: &mut egui::Ui, app: &App, open: &Open, actions: &mut Vec<Action>) {
    ui.heading("노이즈 제거");
    let usable = matches!(app.backend, Backend::Gpu | Backend::Cpu(_));
    ui.add_enabled_ui(usable && !open.selected.is_empty(), |ui| {
        let mut percent = (open.denoise_level * 100.0).round();
        if ui
            .add(
                egui::Slider::new(&mut percent, 0.0..=100.0)
                    .suffix("%")
                    .text("레벨"),
            )
            .changed()
        {
            actions.push(Action::SetDenoiseLevel(percent / 100.0));
        }
        if ui.button("적용").clicked() {
            actions.push(Action::ApplyDenoise);
        }
    });
    match (&app.backend, open.selected.len()) {
        (Backend::Unavailable(_), _) => ui.weak("AI 모델이 없어 쓸 수 없습니다"),
        (Backend::Unknown, _) => ui.weak("AI 준비 중"),
        (_, 0) => ui.weak("사진을 고르세요"),
        (_, count) => ui.weak(format!("선택한 {count}장에 적용됩니다")),
    };
    if let Some(current) = open.current {
        let level = open.edit(current).denoise;
        ui.weak(format!("지금 사진: {:.0}%", level * 100.0));
    }
}

/// 사진 목록의 한 줄 높이.
const ROW: f32 = 56.0;

fn photo_list(ui: &mut egui::Ui, open: &Open, view: &mut View, actions: &mut Vec<Action>) {
    if open.photos.is_empty() {
        ui.weak("[폴더 가져오기]로 사진 폴더를 추가하세요");
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

fn preview(ui: &mut egui::Ui, open: &Open, view: &mut View) {
    let Some(current) = open.current else {
        ui.centered_and_justified(|ui| ui.weak("왼쪽에서 사진을 고르세요"));
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
            if response.double_clicked() {
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
                    egui::Label::new(RichText::new(format!("배율 {percent:.0}%")).weak())
                        .selectable(false),
                )
                .on_hover_text("휠: 확대/축소 · 끌기: 이동 · 두 번 누르기: 맞춤/100%");
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
            window("이미 있는 파일").show(ctx, |ui| {
                ui.label(format!(
                    "내보낼 {}장 중 {existing}장은 출력 파일이 이미 있습니다.",
                    photos.len()
                ));
                ui.horizontal(|ui| {
                    if ui
                        .button("건너뛰기")
                        .on_hover_text("없는 것만 새로 만듭니다")
                        .clicked()
                    {
                        actions.push(Action::ConfirmExport { overwrite: false });
                    }
                    if ui.button("덮어쓰기").clicked() {
                        actions.push(Action::ConfirmExport { overwrite: true });
                    }
                    if ui.button("취소").clicked() {
                        actions.push(Action::CancelExport);
                    }
                });
            });
        }
        ExportState::Finished { outcome, warnings } => {
            window("내보내기 결과").show(ctx, |ui| {
                ui.label(format!(
                    "완료 {}장 · 건너뜀 {}장 · 실패 {}장",
                    outcome.done,
                    outcome.skipped,
                    outcome.failed.len()
                ));
                if let Some(reason) = &outcome.stopped {
                    ui.label(format!("끝까지 처리하지 못했습니다: {reason}"));
                }
                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for (file, reason) in &outcome.failed {
                            ui.label(format!("실패: {}: {reason}", file.display()));
                        }
                        for (file, warning) in warnings {
                            ui.label(format!("알림: {}: {warning}", file.display()));
                        }
                    });
                if ui.button("닫기").clicked() {
                    actions.push(Action::DismissExport);
                }
            });
        }
        ExportState::Failed(reason) => {
            window("내보내기 실패").show(ctx, |ui| {
                ui.label(RichText::new(reason));
                if ui.button("닫기").clicked() {
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
        fn screenshot(&mut self, _: &egui::Context) {
            self.screenshots += 1;
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
        for index in 0..count {
            let path = screen.app.borrow().open.as_ref().unwrap().photo_path(index);
            handle(Action::Thumbnail {
                index,
                pixels: crate::preview::thumbnail(&path, 160).unwrap(),
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
        let rendered =
            crate::export::render(&path, None, &crate::edit::Params::default(), &|_| {}).unwrap();
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
        let looked = crate::export::render(&path, None, &params, &|_| {}).unwrap();
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
    }
}
