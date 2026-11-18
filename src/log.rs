//! 로그 파일: 문제가 생겼을 때 원인을 볼 수 있도록 일어난 일을 남긴다.
//!
//! 프로그램 전체가 파일 하나(`~/.local/state/amsil/log.txt`)에 쓴다. `init`을 부르기 전에는
//! 아무것도 쓰지 않으므로, 테스트가 사용자의 로그 파일을 건드리지 않는다.

use crate::lang::t;
use std::ffi::OsString;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// 이보다 커진 로그는 다음에 켤 때 `log.old.txt`로 옮기고 새로 시작한다.
const MAX_BYTES: u64 = 1 << 20;

/// 얼마나 심각한 일인지.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Info => t("정보", "info"),
            Level::Warn => t("경고", "warn"),
            Level::Error => t("오류", "error"),
        }
    }
}

/// 로그 파일 하나.
pub struct Logger {
    file: File,
}

impl Logger {
    /// 로그 파일을 열어 이어 쓴다. 폴더가 없으면 만든다.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Self::open_within(path, MAX_BYTES)
    }

    /// `limit`바이트를 넘은 로그는 `log.old.txt`로 옮기고(이전 것 하나만 남긴다) 새로 시작한다.
    // ponytail: 켤 때만 크기를 본다. 한 번 켜서 1MB 넘게 쓰는 일은 드물다.
    fn open_within(path: &Path, limit: u64) -> std::io::Result<Self> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if std::fs::metadata(path).is_ok_and(|file| file.len() > limit) {
            let old = path.with_file_name("log.old.txt");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(path, old)?;
        }
        let file = File::options().create(true).append(true).open(path)?;
        Ok(Self { file })
    }

    /// 한 항목을 쓴다: `시각 [수준] 내용`. 여러 줄이면 둘째 줄부터 들여 쓴다.
    pub fn write(&mut self, level: Level, text: &str) {
        let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let mut lines = text.lines();
        let mut entry = format!(
            "{now} [{}] {}\n",
            level.label(),
            lines.next().unwrap_or_default()
        );
        for line in lines {
            entry.push_str("    ");
            entry.push_str(line);
            entry.push('\n');
        }
        // 로그를 쓰지 못해도 하던 일은 계속한다.
        let _ = self.file.write_all(entry.as_bytes());
    }
}

/// 프로그램 전체가 함께 쓰는 로그. `init` 전에는 없다.
static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

/// 로그 파일의 위치.
pub fn path() -> PathBuf {
    path_from(|key| std::env::var_os(key))
}

fn path_from(var: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let state = var("XDG_STATE_HOME")
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
        .or_else(|| var("HOME").map(|home| PathBuf::from(home).join(".local/state")))
        .unwrap_or_default();
    state.join("amsil/log.txt")
}

/// 로그 파일을 열고, 프로그램이 튕길 때 그 이유도 남기게 해 둔다. 프로그램을 켤 때 한 번 부른다.
pub fn init() {
    let path = path();
    match Logger::open(&path) {
        Ok(logger) => *LOGGER.lock().unwrap() = Some(logger),
        Err(e) => {
            let path = path.display();
            eprintln!(
                "{}",
                t(
                    format!("로그 파일을 열 수 없습니다 ({path}): {e}"),
                    format!("Cannot open the log file ({path}): {e}"),
                )
            );
        }
    }
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // 로그를 쓰던 중에 멈춘 것일 수도 있으니, 잠겨 있으면 기다리지 않는다.
        if let Ok(mut logger) = LOGGER.try_lock()
            && let Some(logger) = logger.as_mut()
        {
            logger.write(
                Level::Error,
                &t(
                    format!("프로그램이 예기치 않게 멈췄습니다: {info}"),
                    format!("The program stopped unexpectedly: {info}"),
                ),
            );
        }
        default(info);
    }));
}

fn write(level: Level, text: &str) {
    if let Ok(mut logger) = LOGGER.lock()
        && let Some(logger) = logger.as_mut()
    {
        logger.write(level, text);
    }
}

pub fn info(text: impl AsRef<str>) {
    write(Level::Info, text.as_ref());
}

pub fn warn(text: impl AsRef<str>) {
    write(Level::Warn, text.as_ref());
}

pub fn error(text: impl AsRef<str>) {
    write(Level::Error, text.as_ref());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("amsil-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// `2026-11-18 15:04:05 ` 꼴의 시각으로 시작하는가.
    fn starts_with_time(line: &str) -> bool {
        let bytes = line.as_bytes();
        bytes.len() > 20
            && bytes[..19].iter().enumerate().all(|(i, b)| match i {
                4 | 7 => *b == b'-',
                10 => *b == b' ',
                13 | 16 => *b == b':',
                _ => b.is_ascii_digit(),
            })
            && bytes[19] == b' '
    }

    #[test]
    fn each_entry_is_a_line_with_the_time_and_how_serious_it_is() {
        let dir = temp("lines");
        let path = dir.join("deep/log.txt");
        {
            // 폴더가 없으면 만든다.
            let mut logger = Logger::open(&path).unwrap();
            logger.write(Level::Info, "시작");
            logger.write(Level::Error, "a.ARW: 읽을 수 없습니다");
        }
        // 다시 열면 이어서 쓴다.
        Logger::open(&path)
            .unwrap()
            .write(Level::Warn, "품질을 낮췄습니다");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines.iter().all(|line| starts_with_time(line)), "{text}");
        assert!(lines[0].ends_with(" [정보] 시작"), "{}", lines[0]);
        assert!(lines[1].ends_with(" [오류] a.ARW: 읽을 수 없습니다"));
        assert!(lines[2].ends_with(" [경고] 품질을 낮췄습니다"));
    }

    #[test]
    fn a_message_of_several_lines_stays_one_entry() {
        let dir = temp("multi");
        let path = dir.join("log.txt");
        Logger::open(&path).unwrap().write(
            Level::Warn,
            "모델이 없습니다\n받는 방법: scripts/fetch-models.sh\n",
        );
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert!(starts_with_time(lines[0]) && lines[0].ends_with("모델이 없습니다"));
        // 이어지는 줄은 들여 써서, 시각으로 시작하는 줄만 세면 항목 수가 된다.
        assert_eq!(lines[1], "    받는 방법: scripts/fetch-models.sh");
    }

    #[test]
    fn a_log_that_has_grown_too_big_is_set_aside_once() {
        let dir = temp("rotate");
        let path = dir.join("log.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "옛 기록\n".repeat(100)).unwrap();
        std::fs::write(dir.join("log.old.txt"), "더 옛 기록\n").unwrap();
        // 한도보다 작으면 그대로 이어 쓴다.
        Logger::open_within(&path, 1 << 20)
            .unwrap()
            .write(Level::Info, "이어서");
        assert!(std::fs::read_to_string(&path).unwrap().contains("옛 기록"));
        // 한도를 넘었으면 이전 것 하나만 남기고 새로 시작한다.
        Logger::open_within(&path, 100)
            .unwrap()
            .write(Level::Info, "새로");
        let (now, old) = (
            std::fs::read_to_string(&path).unwrap(),
            std::fs::read_to_string(dir.join("log.old.txt")).unwrap(),
        );
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(now.contains("새로") && !now.contains("옛 기록"), "{now}");
        assert!(old.contains("옛 기록") && old.contains("이어서") && !old.contains("더 옛 기록"));
    }

    #[test]
    fn the_log_lives_in_the_state_folder() {
        let vars = |pairs: &'static [(&str, &str)]| {
            move |key: &str| {
                pairs
                    .iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| std::ffi::OsString::from(value))
            }
        };
        assert_eq!(
            path_from(vars(&[("HOME", "/home/me")])),
            PathBuf::from("/home/me/.local/state/amsil/log.txt")
        );
        assert_eq!(
            path_from(vars(&[
                ("HOME", "/home/me"),
                ("XDG_STATE_HOME", "/data/state")
            ])),
            PathBuf::from("/data/state/amsil/log.txt")
        );
    }

    #[test]
    fn nothing_is_written_until_the_log_is_set_up() {
        // 테스트에서는 `init`을 부르지 않는다. 그래도 로그를 남기는 코드는 탈 없이 지나가야 한다.
        info("테스트 중");
        error("테스트 중");
    }
}
