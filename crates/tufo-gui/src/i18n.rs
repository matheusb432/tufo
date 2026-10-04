//! One startup language for the window, worker threads, and tray.

use std::{fmt::Display, sync::OnceLock};

use gettext::Catalog;
pub(crate) use tufo_client::desktop::Language;

static CATALOG: OnceLock<Catalog> = OnceLock::new();
const PORTUGUESE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/pt_BR.mo"));

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
