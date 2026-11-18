//! 실행 파일을 직접 돌려, 메시지의 언어가 시스템 로캘과 설정 파일을 따르는지 본다.

use std::path::Path;
use std::process::Command;

/// 모르는 옵션을 주고 실행했을 때의 오류 출력.
fn complaint(config_home: &Path, locale: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_amsil"))
        .arg("--nope")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", locale)
        .env("XDG_CONFIG_HOME", config_home)
        // 로그도 임시 폴더에 쓰게 한다 (사용자의 실제 로그를 건드리지 않는다).
        .env("XDG_STATE_HOME", config_home.join("state"))
        .output()
        .unwrap();
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn messages_follow_the_system_locale_unless_a_language_was_chosen() {
    let dir = std::env::temp_dir().join(format!("amsil-lang-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("amsil")).unwrap();

    let korean = complaint(&dir, "ko_KR.UTF-8");
    assert!(
        korean.contains("모르는 옵션: --nope") && korean.contains("사용법"),
        "{korean}"
    );
    let english = complaint(&dir, "en_US.UTF-8");
    assert!(
        english.contains("Unknown option: --nope") && english.contains("Usage"),
        "{english}"
    );

    // 화면에서 고른 언어는 설정 파일에 남고, 시스템 로캘보다 먼저다.
    std::fs::write(dir.join("amsil/config.toml"), "lang = \"ko\"\n").unwrap();
    let chosen = complaint(&dir, "en_US.UTF-8");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(chosen.contains("모르는 옵션: --nope"), "{chosen}");
}
