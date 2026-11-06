//! UI language: English, or Italian when the operating system is set to
//! Italian. `LUMEN_LANG=en|it` overrides the detection.
//!
//! Texts are written in English next to their translation, at the place
//! they are used: `tr("Open folder…", "Apri cartella…")`, or
//! `trf!("{n} photos", "{n} foto")` for formatted ones.

use std::sync::OnceLock;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    En,
    It,
}

static LANG: OnceLock<Lang> = OnceLock::new();

/// Picks the language once, at start-up.
pub fn init() -> Lang {
    *LANG.get_or_init(|| {
        let requested = std::env::var("LUMEN_LANG").ok().or_else(sys_locale::get_locale);
        requested.as_deref().map_or(Lang::En, from_locale)
    })
}

/// "it", "it-IT", "it_CH.UTF-8"… are Italian; everything else English.
pub fn from_locale(locale: &str) -> Lang {
    let lang = locale.split(['-', '_', '.']).next().unwrap_or("");
    if lang.eq_ignore_ascii_case("it") {
        Lang::It
    } else {
        Lang::En
    }
}

pub fn italian() -> bool {
    LANG.get() == Some(&Lang::It)
}

/// The text in the UI language.
pub fn tr(en: &'static str, it: &'static str) -> &'static str {
    if italian() {
        it
    } else {
        en
    }
}

/// `format!` in the UI language: `trf!("{n} photos", "{n} foto")`.
macro_rules! trf {
    ($en:literal, $it:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::italian() { format!($it $(, $arg)*) } else { format!($en $(, $arg)*) }
    };
}
pub(crate) use trf;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locales() {
        assert_eq!(from_locale("it-IT"), Lang::It);
        assert_eq!(from_locale("it_CH.UTF-8"), Lang::It);
        assert_eq!(from_locale("IT"), Lang::It);
        assert_eq!(from_locale("en-US"), Lang::En);
        assert_eq!(from_locale("de-DE"), Lang::En);
        assert_eq!(from_locale(""), Lang::En);
    }
}
