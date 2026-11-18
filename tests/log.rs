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

#[test]
fn failures_are_written_to_the_log_file() {
    let dir = std::env::temp_dir().join(format!("amsil-logtest-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("photos")).unwrap();
    // RAW가 아닌 내용이 든 파일
    std::fs::write(dir.join("photos/broken.ARW"), [0x5a; 4096]).unwrap();
    let (photos, out) = (dir.join("photos"), dir.join("out"));

    // 폴더 변환: 시작, 실패한 사진과 이유, 요약이 남는다.
    let log = run(&dir, &[photos.to_str().unwrap(), out.to_str().unwrap()]);
    assert!(
        log.contains("[정보] 암실 ") && log.contains("시작"),
        "{log}"
    );
    assert!(log.contains("내보내기 시작: 1장"), "{log}");
    assert!(
        log.contains("[오류]") && log.contains("broken.ARW") && log.contains("RAW 파일"),
        "{log}"
    );
    assert!(log.contains("완료 0장, 건너뜀 0장, 실패 1장"), "{log}");

    // 한 장 변환의 실패도 남고, 앞의 기록 뒤에 이어 쓴다.
    let missing = dir.join("없는 사진.ARW");
    let log = run(
        &dir,
        &[
            missing.to_str().unwrap(),
            out.join("x.heic").to_str().unwrap(),
        ],
    );
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(
        log.contains("내보내기 시작: 1장"),
        "앞의 기록이 지워졌다: {log}"
    );
    assert!(log.matches("[정보] 암실 ").count() == 2, "{log}");
    assert!(log.lines().last().unwrap().contains("[오류]"), "{log}");
}
