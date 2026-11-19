//! 실행 파일을 직접 돌려, 일어난 일이 로그 파일에 남는지 본다.

use std::path::Path;
use std::process::Command;

/// 임시 폴더를 설정·상태 폴더로 삼아 실행한다. 로그 파일의 내용을 돌려준다.
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
    assert!(!status.success(), "이 테스트의 실행은 모두 실패해야 한다");
    std::fs::read_to_string(dir.join("state/amsil/log.txt")).unwrap()
}

/// 경로를 뺀 나머지에 한글이 없는가. 경로(사진 이름)는 사용자가 정한 것이라 한글일 수 있다.
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
    // RAW가 아닌 내용이 든 파일
    std::fs::write(dir.join("photos/broken.ARW"), [0x5a; 4096]).unwrap();
    let (photos, out) = (dir.join("photos"), dir.join("out"));

    // 폴더 변환: 시작, 실패한 사진과 이유, 요약이 남는다. 화면이 한국어여도 로그는 영어다.
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

    // 한 장 변환의 실패도 남고, 앞의 기록 뒤에 이어 쓴다.
    let missing = dir.join("없는 사진.ARW");
    let log = run(
        &dir,
        &[
            missing.to_str().unwrap(),
            out.join("x.heic").to_str().unwrap(),
        ],
    );
    // 잘못 쓴 옵션도 남는다.
    let log_after_bad_option = run(&dir, &["--nope", "in", "out"]);
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        log.contains("Export started: 1 photos"),
        "앞의 기록이 지워졌다: {log}"
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
