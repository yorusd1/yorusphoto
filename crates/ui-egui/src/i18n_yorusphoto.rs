//! yorusphoto: translations of the fork's own interface labels (the assistant sidebar and its
//! menus), from `crates/ui-egui/i18n-yorusphoto/<code>.tsv`. Upstream PhotoCraft's text is
//! translated by craft-i18n; this small catalog is consulted first and covers only what the fork
//! adds. Each line is `English<TAB>translation`; `#` starts a comment.

use std::collections::HashMap;
use std::sync::LazyLock;

/// (language code as `i18n::Lang::code` spells it, catalog text).
const CATALOGS: &[(&str, &str)] = &[
    ("cs", include_str!("../i18n-yorusphoto/cs.tsv")),
    ("de", include_str!("../i18n-yorusphoto/de.tsv")),
    ("el", include_str!("../i18n-yorusphoto/el.tsv")),
    ("es", include_str!("../i18n-yorusphoto/es.tsv")),
    ("fr", include_str!("../i18n-yorusphoto/fr.tsv")),
    ("hu", include_str!("../i18n-yorusphoto/hu.tsv")),
    ("id", include_str!("../i18n-yorusphoto/id.tsv")),
    ("it", include_str!("../i18n-yorusphoto/it.tsv")),
    ("ja", include_str!("../i18n-yorusphoto/ja.tsv")),
    ("ko", include_str!("../i18n-yorusphoto/ko.tsv")),
    ("nl", include_str!("../i18n-yorusphoto/nl.tsv")),
    ("pl", include_str!("../i18n-yorusphoto/pl.tsv")),
    ("pt-br", include_str!("../i18n-yorusphoto/pt-br.tsv")),
    ("ru", include_str!("../i18n-yorusphoto/ru.tsv")),
    ("tr", include_str!("../i18n-yorusphoto/tr.tsv")),
    ("uk", include_str!("../i18n-yorusphoto/uk.tsv")),
    ("vi", include_str!("../i18n-yorusphoto/vi.tsv")),
    ("zh-hans", include_str!("../i18n-yorusphoto/zh-hans.tsv")),
    ("zh-hant", include_str!("../i18n-yorusphoto/zh-hant.tsv")),
];

type Table = HashMap<&'static str, &'static str>;

static TABLES: LazyLock<HashMap<&'static str, Table>> = LazyLock::new(|| CATALOGS.iter().map(|(code, text)| (*code, parse(text))).collect());

/// One catalog's rows; blank lines, comments and malformed lines are skipped, never fatal.
fn parse(text: &'static str) -> Table {
    text.lines()
        .filter(|line| !line.starts_with('#'))
        .filter_map(|line| line.split_once('\t'))
        .filter(|(english, translated)| !english.is_empty() && !translated.trim().is_empty() && !translated.contains('\t'))
        .collect()
}

/// The fork's translation of `english` in the language `code`, if it has one.
pub fn lookup(code: &str, english: &str) -> Option<&'static str> {
    TABLES.get(code)?.get(english).copied()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalog_parses_and_keeps_the_english_shape() {
        for (code, text) in CATALOGS {
            let table = parse(text);
            assert!(!table.is_empty(), "{code}: empty catalog");
            for (english, translated) in table {
                assert_eq!(english.ends_with('…'), translated.ends_with('…'), "{code}: {english}");
                assert_eq!(english.matches('{').count(), translated.matches('{').count(), "{code}: {english}");
            }
        }
    }

    #[test]
    fn lookups_fall_through_for_unknown_languages_and_labels() {
        assert_eq!(lookup("es", "Generate Image with Codex…"), Some("Generar imagen con Codex…"));
        assert_eq!(lookup("xx", "Generate Image with Codex…"), None);
        assert_eq!(lookup("es", "Not a fork label"), None);
    }
}
