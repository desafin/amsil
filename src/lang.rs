//! 화면과 메시지의 언어. 한국어와 영어 두 가지다.
//!
//! 문구는 쓰는 자리에 두 언어를 나란히 적는다: `t("사진", "Photos")`.
//!
//! 오류와 알림처럼 화면에도 나오고 로그에도 남는 글은 `text!("…", "…")`로 두 언어를 함께 지닌
//! [`Text`]로 만든다. 화면에는 지금 언어로 보이고, 로그에는 언제나 영어로 남는다.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Ko,
    En,
}

// 오류 문구는 백그라운드 스레드에서도 만들어지므로, 언어는 프로그램 전체가 함께 보는 값 하나로 둔다.
#[cfg(not(test))]
static ENGLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// 테스트는 한 프로세스에서 여러 개가 동시에 돈다. 한 테스트가 언어를 바꿔도 다른 테스트가
// 흔들리지 않도록 스레드마다 따로 둔다.
#[cfg(test)]
thread_local!(static ENGLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

// `with`가 잠시 정한 언어. 그 스레드에서만 보인다.
thread_local!(static OVERRIDE: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) });

/// 지금 언어. 따로 정하지 않으면 한국어다.
pub fn current() -> Lang {
    if let Some(lang) = OVERRIDE.get() {
        return lang;
    }
    #[cfg(not(test))]
    let english = ENGLISH.load(std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    let english = ENGLISH.get();
    if english { Lang::En } else { Lang::Ko }
}

pub fn set(lang: Lang) {
    let english = lang == Lang::En;
    #[cfg(not(test))]
    ENGLISH.store(english, std::sync::atomic::Ordering::Relaxed);
    #[cfg(test)]
    ENGLISH.set(english);
}

/// 지금 언어에 맞는 쪽을 고른다.
pub fn t<T>(ko: T, en: T) -> T {
    match current() {
        Lang::Ko => ko,
        Lang::En => en,
    }
}

/// `f`를 부르는 동안만 이 스레드의 언어를 `lang`으로 둔다. 화면의 언어는 그대로다.
pub fn with<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    // `f`가 패닉해도 원래 언어로 돌려놓는다.
    struct Restore(Option<Lang>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.set(self.0);
        }
    }
    let _restore = Restore(OVERRIDE.replace(Some(lang)));
    f()
}

/// 두 언어로 된 [`Text`]를 만든다: `text!(format!("{n}장"), format!("{n} photos"))`.
///
/// 한국어 쪽은 한국어로, 영어 쪽은 영어로 그린다. 그래서 글 안에 넣은 다른 오류(`{e}`)도
/// 언어마다 제 말로 들어간다.
macro_rules! text {
    ($ko:expr, $en:expr $(,)?) => {
        $crate::lang::Text::new(
            $crate::lang::with($crate::lang::Lang::Ko, || {
                ::std::string::ToString::to_string(&$ko)
            }),
            $crate::lang::with($crate::lang::Lang::En, || {
                ::std::string::ToString::to_string(&$en)
            }),
        )
    };
}
pub(crate) use text;

/// 두 언어로 된 글. 오류와 알림처럼 화면에도 나오고 로그에도 남는 글이다.
/// 화면에는 지금 언어로 보이고(`Display`), 로그에는 영어로 남는다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    ko: String,
    en: String,
}

impl Text {
    pub fn new(ko: String, en: String) -> Self {
        Self { ko, en }
    }

    /// 다른 오류를 두 언어로 옮겨 담는다. 그 안에 `Text`가 들어 있으면 언어마다 제 말로 나온다.
    pub fn of(value: &(impl std::fmt::Display + ?Sized)) -> Self {
        text!(value, value)
    }

    pub fn english(&self) -> &str {
        &self.en
    }
}

impl std::fmt::Display for Text {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(t(&self.ko, &self.en))
    }
}

impl std::error::Error for Text {}

/// 시스템의 언어. POSIX 규칙대로 `LC_ALL`, `LC_MESSAGES`, `LANG` 순서로 본다.
// ponytail: 환경 변수만 본다. 이 변수들이 없는 Windows에서는 영어가 된다. Windows를 지원할 때 시스템 API로 바꾼다.
pub fn system() -> Lang {
    system_from(|key| std::env::var(key).ok())
}

fn system_from(var: impl Fn(&str) -> Option<String>) -> Lang {
    ["LC_ALL", "LC_MESSAGES", "LANG"]
        .into_iter()
        .filter_map(var)
        .find(|locale| !locale.is_empty())
        .map_or(Lang::En, |locale| from_locale(&locale))
}

/// 로캘 이름(`ko_KR.UTF-8` 등)의 언어. 한국어가 아니면 모두 영어로 본다.
fn from_locale(locale: &str) -> Lang {
    if locale.split(['_', '.', '@']).next() == Some("ko") {
        Lang::Ko
    } else {
        Lang::En
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_korean_locales_get_korean() {
        for locale in ["ko_KR.UTF-8", "ko", "ko_KR", "ko.UTF-8", "ko_KR@hangul"] {
            assert_eq!(from_locale(locale), Lang::Ko, "{locale}");
        }
        for locale in ["en_US.UTF-8", "C", "POSIX", "", "ja_JP.UTF-8", "kok_IN"] {
            assert_eq!(from_locale(locale), Lang::En, "{locale}");
        }
    }

    #[test]
    fn the_system_language_follows_lc_all_then_lc_messages_then_lang() {
        fn system(vars: &'static [(&str, &str)]) -> Lang {
            system_from(|key| {
                vars.iter()
                    .find(|(name, _)| *name == key)
                    .map(|(_, value)| value.to_string())
            })
        }
        assert_eq!(system(&[("LANG", "ko_KR.UTF-8")]), Lang::Ko);
        assert_eq!(
            system(&[("LANG", "ko_KR.UTF-8"), ("LC_ALL", "en_US.UTF-8")]),
            Lang::En
        );
        assert_eq!(
            system(&[("LANG", "en_US.UTF-8"), ("LC_MESSAGES", "ko_KR.UTF-8")]),
            Lang::Ko
        );
        // 빈 값은 정하지 않은 것으로 본다.
        assert_eq!(system(&[("LANG", "ko_KR.UTF-8"), ("LC_ALL", "")]), Lang::Ko);
        assert_eq!(system(&[]), Lang::En);
    }

    #[test]
    fn t_picks_the_text_for_the_current_language() {
        assert_eq!(current(), Lang::Ko, "따로 정하지 않으면 한국어");
        assert_eq!(t("사진", "Photos"), "사진");
        set(Lang::En);
        assert_eq!(current(), Lang::En);
        assert_eq!(t("사진", "Photos"), "Photos");
        assert_eq!(t(format!("{}장", 3), format!("{} photos", 3)), "3 photos");
    }

    #[test]
    fn a_text_shows_the_current_language_and_keeps_the_english() {
        let missing = text!("모델이 없습니다", "The model is missing");
        // 글 안에 넣은 오류도 언어마다 제 말로 들어간다.
        let wrapped = text!(format!("a.ARW: {missing}"), format!("a.ARW: {missing}"));
        let boxed: Box<dyn std::error::Error + Send + Sync> = wrapped.clone().into();
        let carried = Text::of(&boxed);
        assert_eq!(current(), Lang::Ko);
        assert_eq!(wrapped.to_string(), "a.ARW: 모델이 없습니다");
        assert_eq!(carried.to_string(), "a.ARW: 모델이 없습니다");
        assert_eq!(carried.english(), "a.ARW: The model is missing");
        set(Lang::En);
        assert_eq!(boxed.to_string(), "a.ARW: The model is missing");
        assert_eq!(
            with(Lang::Ko, || boxed.to_string()),
            "a.ARW: 모델이 없습니다"
        );
        assert_eq!(current(), Lang::En, "with는 끝나면 언어를 돌려놓는다");
    }
}
