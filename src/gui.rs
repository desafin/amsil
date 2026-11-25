//! Opens the window and wires together `App` (state), `ui` (drawing), and `Workers` (background
//! jobs).

use crate::app::{Action, App, Effect};
use crate::lang::t;
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
            .with_title(t("암실", "Amsil"))
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

/// Upper limit on a preview's long side. Even a 60-megapixel photo is scaled down to this before
/// upload (about 200 MB as RGBA).
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
        workers.run(Effect::ProbeBackend);
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
        // Make the preview as close to the original as one GPU texture allows (for zooming in).
        let texture_side = ui.ctx().input(|input| input.max_texture_side);
        self.workers
            .set_preview_side(texture_side.min(MAX_PREVIEW_SIDE));

        // Apply results from background jobs first, draw that state, then handle what the user did.
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

/// Real file dialogs and screen capture.
struct Dialogs {
    send: Arc<dyn Fn(Action) + Send + Sync>,
}

impl Env for Dialogs {
    fn new_project_path(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(t("새 프로젝트", "New project"))
            .add_filter(t("암실 프로젝트", "Amsil project"), &["amsil"])
            .set_file_name(t("새 프로젝트.amsil", "New project.amsil"))
            .save_file()
    }

    fn open_project_path(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(t("프로젝트 열기", "Open project"))
            .add_filter(t("암실 프로젝트", "Amsil project"), &["amsil"])
            .pick_file()
    }

    fn pick_folder(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new().pick_folder()
    }

    fn pick_lut(&mut self) -> Option<PathBuf> {
        rfd::FileDialog::new()
            .set_title(t("LUT 불러오기", "Load LUT"))
            .add_filter("3D LUT", &["cube"])
            .pick_file()
    }

    // ponytail: opens with xdg-open on Linux. Add the Windows command when Windows is supported.
    fn open_log(&mut self) -> String {
        let path = crate::log::path();
        let shown = path.display();
        match std::process::Command::new("xdg-open").arg(&path).spawn() {
            Ok(mut viewer) => {
                // Wait on it so the finished process does not linger.
                std::thread::spawn(move || viewer.wait());
                t(format!("로그 파일: {shown}"), format!("Log file: {shown}"))
            }
            Err(e) => t(
                format!(
                    "로그 파일을 열 프로그램을 찾지 못했습니다 ({e}). 직접 열어 보세요: {shown}"
                ),
                format!("No program found to open the log file ({e}). Open it yourself: {shown}"),
            ),
        }
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
