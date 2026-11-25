//! Log file: records what happened so the cause can be found when something goes wrong.
//!
//! The whole program writes to one file (`~/.local/state/amsil/log.txt`). Nothing is written before
//! `init` is called, so tests do not touch the user's log file.
//!
//! The log is in English regardless of the screen language, so it is easy to share or search.
//! For text that also appears on screen (errors etc.), pass a [`Text`](crate::lang::Text) and its
//! English side is recorded.

use crate::lang::{self, Lang, t};
use std::ffi::OsString;
use std::fmt::Display;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// A log larger than this is moved to `log.old.txt` at the next launch and a new one starts.
const MAX_BYTES: u64 = 1 << 20;

/// How serious something is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    fn label(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }
}

/// One log file.
pub struct Logger {
    file: File,
}

impl Logger {
    /// Opens the log file for appending. Creates the folder if missing.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        Self::open_within(path, MAX_BYTES)
    }

    /// A log over `limit` bytes is moved to `log.old.txt` (keeping only the previous one) and a new
    /// one starts.
    // ponytail: the size is checked only at launch. Writing over 1MB in one session is rare.
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

    /// Writes one entry: `time [level] message`. Lines after the first are indented.
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
        // Carry on with the work even if the log cannot be written.
        let _ = self.file.write_all(entry.as_bytes());
    }
}

/// The log the whole program shares. Absent before `init`.
static LOGGER: Mutex<Option<Logger>> = Mutex::new(None);

/// Location of the log file.
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

/// Opens the log file and sets it up to record the reason if the program crashes. Called once at
/// launch.
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
        // It may have stopped while writing the log, so do not wait if it is locked.
        if let Ok(mut logger) = LOGGER.try_lock()
            && let Some(logger) = logger.as_mut()
        {
            logger.write(
                Level::Error,
                &format!("The program stopped unexpectedly: {info}"),
            );
        }
        default(info);
    }));
}

/// Renders in English and writes. A `Text`, or an error holding one, gives its English side.
fn write(level: Level, text: &dyn Display) {
    if let Ok(mut logger) = LOGGER.lock()
        && let Some(logger) = logger.as_mut()
    {
        logger.write(level, &english(text));
    }
}

fn english(text: &dyn Display) -> String {
    lang::with(Lang::En, || text.to_string())
}

pub fn info(text: impl Display) {
    write(Level::Info, &text);
}

pub fn warn(text: impl Display) {
    write(Level::Warn, &text);
}

pub fn error(text: impl Display) {
    write(Level::Error, &text);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("amsil-log-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    /// Whether it starts with a time of the form `2026-11-18 15:04:05 `.
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
            // Creates the folder if missing.
            let mut logger = Logger::open(&path).unwrap();
            logger.write(Level::Info, "시작");
            logger.write(Level::Error, "a.ARW: 읽을 수 없습니다");
        }
        // Reopening appends.
        Logger::open(&path)
            .unwrap()
            .write(Level::Warn, "품질을 낮췄습니다");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3, "{text}");
        assert!(lines.iter().all(|line| starts_with_time(line)), "{text}");
        assert!(lines[0].ends_with(" [info] 시작"), "{}", lines[0]);
        assert!(lines[1].ends_with(" [error] a.ARW: 읽을 수 없습니다"));
        assert!(lines[2].ends_with(" [warn] 품질을 낮췄습니다"));
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
        // Continuation lines are indented, so counting only lines that start with a time gives the
        // number of entries.
        assert_eq!(lines[1], "    받는 방법: scripts/fetch-models.sh");
    }

    #[test]
    fn a_log_that_has_grown_too_big_is_set_aside_once() {
        let dir = temp("rotate");
        let path = dir.join("log.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&path, "옛 기록\n".repeat(100)).unwrap();
        std::fs::write(dir.join("log.old.txt"), "더 옛 기록\n").unwrap();
        // Below the limit, append as is.
        Logger::open_within(&path, 1 << 20)
            .unwrap()
            .write(Level::Info, "이어서");
        assert!(std::fs::read_to_string(&path).unwrap().contains("옛 기록"));
        // Over the limit, keep only the previous one and start anew.
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
    fn what_the_screen_shows_in_korean_is_logged_in_english() {
        lang::set(Lang::Ko);
        let reason = lang::text!("손상된 파일입니다", "The file is damaged");
        let error: Box<dyn std::error::Error + Send + Sync> =
            lang::text!(format!("a.ARW: {reason}"), format!("a.ARW: {reason}")).into();
        assert_eq!(
            error.to_string(),
            "a.ARW: 손상된 파일입니다",
            "Korean on screen"
        );
        assert_eq!(english(&error), "a.ARW: The file is damaged");
    }

    #[test]
    fn nothing_is_written_until_the_log_is_set_up() {
        // Tests do not call `init`. Code that logs must still pass through without trouble.
        info("in a test");
        error("in a test");
    }
}
