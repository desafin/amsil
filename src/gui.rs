//! 창을 띄우고 `App`(상태), `ui`(그리기), `Workers`(백그라운드 작업)를 잇는다.

use crate::app::{Action, App};
use crate::preview::Pixels;
use crate::project::Config;
use crate::ui::{self, Env, View};
use crate::worker::Workers;
use eframe::egui;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;

pub fn run() -> crate::Result<()> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("암실")
            .with_inner_size([1280.0, 800.0])
            .with_min_inner_size([640.0, 480.0]),
        ..Default::default()
    };
    eframe::run_native(
        "amsil",
        options,
        Box::new(|cc| Ok(Box::new(Gui::new(&cc.egui_ctx)))),
    )
    .map_err(|e| e.to_string().into())
}

/// 미리보기 한 장의 긴 변 상한. 6,000만 화소 사진도 여기까지 줄여서 올린다 (RGBA로 약 200MB).
const MAX_PREVIEW_SIDE: usize = 8192;

struct Gui {
    app: App,
    view: View,
    workers: Workers,
    events: Receiver<Action>,
    env: Dialogs,
}

impl Gui {
    fn new(ctx: &egui::Context) -> Self {
        ui::install_fonts(ctx);
        let wake = ctx.clone();
        let (workers, events) = Workers::new(move || wake.request_repaint());
        workers.probe_backend();
        let env = Dialogs {
            send: Arc::new(workers.sender()),
        };
        Self {
            app: App::new(Config::path()),
            view: View::default(),
            workers,
            events,
            env,
        }
    }
}

impl eframe::App for Gui {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        // 미리보기는 GPU가 한 장으로 받을 수 있는 크기까지 원본에 가깝게 만든다 (확대해서 보기 위해).
        let texture_side = ui.ctx().input(|input| input.max_texture_side);
        self.workers
            .set_preview_side(texture_side.min(MAX_PREVIEW_SIDE));

        // 백그라운드 작업이 보낸 결과를 먼저 반영하고, 그 상태를 그린 뒤, 사용자가 한 일을 처리한다.
        let mut actions: Vec<Action> = self.events.try_iter().collect();
        actions.extend(ui::draw(ui, &self.app, &mut self.view, &mut self.env));
        if !actions.is_empty() {
            ui.ctx().request_repaint();
        }
        for action in actions {
            for effect in self.app.handle(action) {
                self.workers.run(effect);
            }
        }
    }
}

/// 실제 파일 대화 상자와 화면 갈무리.
struct Dialogs {
    send: Arc<dyn Fn(Action) + Send + Sync>,
}

impl Env for Dialogs {
    fn new_project_path(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("새 프로젝트")
            .add_filter("암실 프로젝트", &["amsil"])
            .set_file_name("새 프로젝트.amsil")
            .save_file()
    }

    fn open_project_path(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title("프로젝트 열기")
            .add_filter("암실 프로젝트", &["amsil"])
            .pick_file()
    }

    fn pick_folder(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new().pick_folder()
    }

    fn screenshot(&mut self, ctx: &egui::Context) {
        let send = self.send.clone();
        ctx.request_screenshot(move |image| {
            let rgba = image
                .pixels
                .iter()
                .flat_map(|pixel| pixel.to_array())
                .collect();
            send(Action::SaveScreenshot(Pixels {
                width: image.size[0],
                height: image.size[1],
                rgba,
            }));
        });
    }
}
