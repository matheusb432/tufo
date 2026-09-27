//! One startup language for the window, worker threads, and tray.

use std::{fmt::Display, sync::OnceLock};

use gettext::Catalog;

static CATALOG: OnceLock<Catalog> = OnceLock::new();
const PORTUGUESE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pt_BR.mo"));

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Deserialize)]
pub(crate) enum Language {
    #[default]
    #[serde(rename = "system")]
    System,
    #[serde(rename = "en")]
    English,
    #[serde(rename = "pt-BR", alias = "pt_BR")]
    PortugueseBrazil,
}

impl Language {
    pub(crate) fn config_name(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::English => "en",
            Self::PortugueseBrazil => "pt-BR",
        }
    }

    fn resolve(self, locales: impl IntoIterator<Item = String>) -> Self {
        if self != Self::System {
            return self;
        }
        for locale in locales {
            let language = locale.split(['-', '_', '.', '@']).next().unwrap_or("");
            if language.eq_ignore_ascii_case("pt") {
                return Self::PortugueseBrazil;
            }
            if language.eq_ignore_ascii_case("en") || matches!(language, "C" | "POSIX") {
                return Self::English;
            }
        }
        Self::English
    }
}

pub(crate) fn initialize(language: Language) -> Result<(), gettext::Error> {
    let language = language.resolve(sys_locale::get_locales());
    let catalog = if language == Language::PortugueseBrazil {
        Catalog::parse(PORTUGUESE)?
    } else {
        Catalog::empty()
    };
    let _ = CATALOG.set(catalog);
    Ok(())
}

pub(crate) fn select_window_language(
    language: Language,
) -> Result<(), slint::SelectBundledTranslationError> {
    let language = language.resolve(sys_locale::get_locales());
    slint::select_bundled_translation(if language == Language::PortugueseBrazil {
        "pt_BR"
    } else {
        ""
    })
}

pub(crate) fn tr(message: &'static str) -> &'static str {
    CATALOG
        .get()
        .map_or(message, |catalog| catalog.gettext(message))
}

pub(crate) fn tr_args(message: &'static str, args: &[&dyn Display]) -> String {
    format_message(tr(message), None, args)
}

pub(crate) fn tr_count(one: &'static str, many: &'static str, count: usize) -> String {
    let message = CATALOG.get().map_or_else(
        || if count == 1 { one } else { many },
        |catalog| catalog.ngettext(one, many, u64::try_from(count).unwrap_or(u64::MAX)),
    );
    format_message(message, Some(count), &[])
}

fn format_message(message: &str, count: Option<usize>, args: &[&dyn Display]) -> String {
    let mut result = String::new();
    let mut rest = message;
    while let Some((before, after)) = rest.split_once('{') {
        result.push_str(before);
        let Some((key, tail)) = after.split_once('}') else {
            result.push('{');
            result.push_str(after);
            return result;
        };
        if key == "n"
            && let Some(count) = count
        {
            result.push_str(&count.to_string());
        } else if let Ok(index) = key.parse::<usize>()
            && let Some(value) = args.get(index)
        {
            result.push_str(&value.to_string());
        } else {
            result.push('{');
            result.push_str(key);
            result.push('}');
        }
        rest = tail;
    }
    result.push_str(rest);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn language_override_and_system_fallback() {
        for locale in ["pt-BR", "pt_BR.UTF-8", "pt", "pt-PT"] {
            assert_eq!(
                Language::System.resolve([locale.to_owned()]),
                Language::PortugueseBrazil
            );
        }
        assert_eq!(
            Language::English.resolve(["pt-BR".to_owned()]),
            Language::English
        );
        assert_eq!(
            Language::System.resolve(["de-DE".to_owned()]),
            Language::English
        );
        assert_eq!(
            Language::System.resolve(["en-US".to_owned(), "pt-BR".to_owned()]),
            Language::English
        );
    }

    #[test]
    fn catalog_uses_brazilian_portuguese_plural_rules() {
        let catalog = Catalog::parse(PORTUGUESE).unwrap();
        assert_eq!(catalog.ngettext("{n} file", "{n} files", 0), "{n} arquivo");
        assert_eq!(catalog.ngettext("{n} file", "{n} files", 1), "{n} arquivo");
        assert_eq!(catalog.ngettext("{n} file", "{n} files", 2), "{n} arquivos");
    }

    #[test]
    fn parameters_can_be_reordered_and_are_never_reinterpreted() {
        assert_eq!(
            format_message("{1}: {0}", None, &[&"{1}", &"Nome"]),
            "Nome: {1}"
        );
    }
}
