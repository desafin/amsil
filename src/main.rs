mod app;
mod capture;
mod cli;
mod credits;
mod denoise;
mod develop;
mod edit;
mod export;
mod frame;
mod gpu;
mod gui;
mod heif;
mod job;
mod lang;
mod log;
mod lut;
mod mask;
mod preset;
mod preview;
mod project;
mod raw;
#[cfg(test)]
mod raw16;
mod rcd;
mod recommend;
mod skin;
mod ui;
mod worker;

use lang::t;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// Working image. One plane per channel (R, G, B), values are f32.
#[derive(Clone)]
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub planes: [Vec<f32>; 3],
}

fn main() -> std::process::ExitCode {
    // Use the language picked in the UI if there is one, otherwise the system language.
    let config = project::Config::load(&project::Config::path());
    lang::set(config.lang.unwrap_or_else(lang::system));
    log::init();
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    log::info(format!(
        "Amsil {} started: {}",
        env!("CARGO_PKG_VERSION"),
        if arguments.is_empty() {
            "window".to_owned()
        } else {
            arguments.join(" ")
        }
    ));

    // Run without arguments to open the window.
    if std::env::args_os().len() <= 1 {
        return match gui::run() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                log::error(format!("Could not open a window: {e}"));
                eprintln!(
                    "{}\n{}",
                    t(
                        format!(
                            "창을 열 수 없습니다: {e}\n\n화면 없이 쓰려면 명령행으로 실행하세요."
                        ),
                        format!(
                            "Could not open a window: {e}\n\nTo use it without a display, run it from the command line."
                        ),
                    ),
                    cli::usage()
                );
                std::process::ExitCode::FAILURE
            }
        };
    }
    let args = match cli::parse_args(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            log::error(&message);
            eprintln!("{message}\n{}", cli::usage());
            return std::process::ExitCode::from(2);
        }
    };
    let started = std::time::Instant::now();
    let result = cli::run(&args);
    let seconds = started.elapsed().as_secs_f32();
    eprintln!(
        "{}",
        t(
            format!("걸린 시간 {seconds:.1}초"),
            format!("Took {seconds:.1} s")
        )
    );
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            log::error(&e);
            eprintln!("{}: {e}", t("실패", "Failed"));
            std::process::ExitCode::FAILURE
        }
    }
}

/// Sample RAW for tests. It is a personal photo, so it is not in the repository; tests that
/// need it are skipped when it is missing.
#[cfg(test)]
pub fn sample_arw() -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10161107/DSC04307.ARW");
    if !path.exists() {
        eprintln!("Sample ARW missing, skipping");
    }
    path.exists().then_some(path)
}
