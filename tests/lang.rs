//! Runs the executable directly to check that the message language follows the system locale and
//! the settings file.

use std::path::Path;
use std::process::Command;

/// Error output when run with an unknown option.
fn complaint(config_home: &Path, locale: &str) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_amsil"))
        .arg("--nope")
        .env_remove("LC_ALL")
        .env_remove("LC_MESSAGES")
        .env("LANG", locale)
        .env("XDG_CONFIG_HOME", config_home)
        // Write the log to the temp folder too (leave the user's real log alone).
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

    // The language chosen on screen is kept in the settings file and overrides the system locale.
    std::fs::write(dir.join("amsil/config.toml"), "lang = \"ko\"\n").unwrap();
    let chosen = complaint(&dir, "en_US.UTF-8");
    std::fs::remove_dir_all(&dir).unwrap();
    assert!(chosen.contains("모르는 옵션: --nope"), "{chosen}");
}
