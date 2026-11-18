mod app;
mod cli;
mod denoise;
mod develop;
mod edit;
mod export;
mod gpu;
mod gui;
mod heif;
mod job;
mod preset;
mod preview;
mod project;
mod raw;
mod ui;
mod worker;

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

/// 작업용 이미지. 채널별 평면(R, G, B), 값은 f32.
pub struct Image {
    pub width: usize,
    pub height: usize,
    pub planes: [Vec<f32>; 3],
}

fn main() -> std::process::ExitCode {
    // 인자 없이 실행하면 창을 띄운다.
    if std::env::args_os().len() <= 1 {
        return match gui::run() {
            Ok(()) => std::process::ExitCode::SUCCESS,
            Err(e) => {
                eprintln!(
                    "창을 열 수 없습니다: {e}\n\n화면 없이 쓰려면 명령행으로 실행하세요.\n{}",
                    cli::USAGE
                );
                std::process::ExitCode::FAILURE
            }
        };
    }
    let args = match cli::parse_args(std::env::args_os().skip(1)) {
        Ok(args) => args,
        Err(message) => {
            eprintln!("{message}\n{}", cli::USAGE);
            return std::process::ExitCode::from(2);
        }
    };
    let started = std::time::Instant::now();
    let result = cli::run(&args);
    eprintln!("걸린 시간 {:.1}초", started.elapsed().as_secs_f32());
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("실패: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

/// 테스트용 샘플 RAW. 개인 사진이라 저장소에 없고, 없으면 해당 테스트는 건너뛴다.
#[cfg(test)]
pub fn sample_arw() -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("DCIM/10161107/DSC04307.ARW");
    if !path.exists() {
        eprintln!("샘플 ARW가 없어 건너뜀");
    }
    path.exists().then_some(path)
}
