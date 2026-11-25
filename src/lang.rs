//! The language of the screen and messages. There are two: Korean and English.
//!
//! Wording is written with both languages side by side where it is used: `t("사진", "Photos")`.
//!
//! Text that both appears on screen and stays in the log, like errors and notices, is made with
//! `text!("…", "…")` as a [`Text`] that carries both languages. On screen it shows in the current
//! language; in the log it is always English.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Lang {
    Ko,
    En,
}

// Error text is also built on background threads, so the language is a single value shared by the
// whole program.
#[cfg(not(test))]
static ENGLISH: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
// Tests run several at once in one process. Keep it per thread so that one test changing the
// language does not disturb the others.
#[cfg(test)]
thread_local!(static ENGLISH: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });

// The language set temporarily by `with`. Visible only on that thread.
thread_local!(static OVERRIDE: std::cell::Cell<Option<Lang>> = const { std::cell::Cell::new(None) });

/// The current language. Korean unless set otherwise.
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

/// Picks the side that matches the current language.
pub fn t<T>(ko: T, en: T) -> T {
    match current() {
        Lang::Ko => ko,
        Lang::En => en,
    }
}

/// Sets this thread's language to `lang` only while `f` is called. The screen language stays as is.
pub fn with<R>(lang: Lang, f: impl FnOnce() -> R) -> R {
    // Restore the original language even if `f` panics.
    struct Restore(Option<Lang>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.set(self.0);
        }
    }
    let _restore = Restore(OVERRIDE.replace(Some(lang)));
    f()
}

/// Makes a bilingual [`Text`]: `text!(format!("{n}장"), format!("{n} photos"))`.
///
/// The Korean side is rendered in Korean and the English side in English, so another error put into
/// the text (`{e}`) also comes out in each language's own words.
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

/// Bilingual text. Text that both appears on screen and stays in the log, like errors and notices.
/// On screen it shows in the current language (`Display`); in the log it stays in English.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    ko: String,
    en: String,
}

impl Text {
    pub fn new(ko: String, en: String) -> Self {
        Self { ko, en }
    }

    /// Carries another error over in both languages. A `Text` inside comes out in each language.
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

/// The system language. Checks `LC_ALL`, `LC_MESSAGES`, `LANG` in that order, per POSIX rules.
// ponytail: only looks at environment variables. On Windows, which lacks them, this becomes
// English. Switch to the system API when Windows is supported.
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

/// The language of a locale name (`ko_KR.UTF-8` etc.). Anything not Korean counts as English.
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
        // An empty value counts as unset.
        assert_eq!(system(&[("LANG", "ko_KR.UTF-8"), ("LC_ALL", "")]), Lang::Ko);
        assert_eq!(system(&[]), Lang::En);
    }

    #[test]
    fn t_picks_the_text_for_the_current_language() {
        assert_eq!(current(), Lang::Ko, "Korean unless set otherwise");
        assert_eq!(t("사진", "Photos"), "사진");
        set(Lang::En);
        assert_eq!(current(), Lang::En);
        assert_eq!(t("사진", "Photos"), "Photos");
        assert_eq!(t(format!("{}장", 3), format!("{} photos", 3)), "3 photos");
    }

    #[test]
    fn a_text_shows_the_current_language_and_keeps_the_english() {
        let missing = text!("모델이 없습니다", "The model is missing");
        // An error put into the text also comes out in each language's own words.
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
        assert_eq!(
            current(),
            Lang::En,
            "with restores the language when it ends"
        );
    }
}
