//! Runs the executable directly to check that what happened is recorded in the log file.

use std::path::Path;
use std::process::Command;

/// Runs with a temp folder as the config and state folder. Returns the log file's contents.
fn run(dir: &Path, args: &[&str]) -> String {
    let status = Command::new(env!("CARGO_BIN_EXE_amsil"))
        .args(args)
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", "ko_KR.UTF-8")
        .env("XDG_CONFIG_HOME", dir.join("config"))
        .env("XDG_STATE_HOME", dir.join("state"))
        .output()
        .unwrap()
        .status;
    assert!(!status.success(), "every run in this test must fail");
    std::fs::read_to_string(dir.join("state/amsil/log.txt")).unwrap()
}

/// Whether the text has no Korean apart from the paths. Paths (photo names) are chosen by the user,
/// so they may be Korean.
fn english_apart_from(log: &str, paths: &[&Path]) -> bool {
    let mut rest = log.to_owned();
    for path in paths {
        rest = rest.replace(path.to_str().unwrap(), "");
    }
    !rest.chars().any(|c| ('가'..='힣').contains(&c))
}

#[test]
fn failures_are_written_to_the_log_file_in_english() {
    let dir = std::env::temp_dir().join(format!("amsil-logtest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("photos")).unwrap();
    // A file whose content is not RAW
    std::fs::write(dir.join("photos/broken.ARW"), [0x5a; 4096]).unwrap();
    let (photos, out) = (dir.join("photos"), dir.join("out"));

    // Folder export: the start, each failed photo with its reason, and a summary are recorded. The
    // log is English even when the screen is in Korean.
    let log = run(&dir, &[photos.to_str().unwrap(), out.to_str().unwrap()]);
    assert!(
        log.contains("[info] Amsil ") && log.contains("started"),
        "{log}"
    );
    assert!(log.contains("Export started: 1 photos"), "{log}");
    assert!(
        log.contains("[error] Failed: ")
            && log.contains("broken.ARW")
            && log.contains("Unsupported or damaged RAW file"),
        "{log}"
    );
    assert!(log.contains("done 0, skipped 0, failed 1"), "{log}");
    assert!(english_apart_from(&log, &[&dir]), "{log}");

    // A single-photo export failure is recorded too, appended after the earlier entries.
    let missing = dir.join("없는 사진.ARW");
    let log = run(
        &dir,
        &[
            missing.to_str().unwrap(),
            out.join("x.heic").to_str().unwrap(),
        ],
    );
    // A mistyped option is recorded too.
    let log_after_bad_option = run(&dir, &["--nope", "in", "out"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        log.contains("Export started: 1 photos"),
        "earlier entries were erased: {log}"
    );
    assert!(log.matches("[info] Amsil ").count() == 2, "{log}");
    assert!(log.lines().last().unwrap().contains("[error]"), "{log}");
    assert!(english_apart_from(&log, &[&missing, &dir]), "{log}");
    assert!(
        log_after_bad_option
            .lines()
            .last()
            .unwrap()
            .ends_with("[error] Unknown option: --nope"),
        "{log_after_bad_option}"
    );
}
