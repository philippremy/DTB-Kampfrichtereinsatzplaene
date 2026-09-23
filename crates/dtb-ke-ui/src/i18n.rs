//! The application's own localization system.
//!
//! One axis, authored as TOML compiled into the binary (`locales/`): a
//! **catalog** — nested tables of UI strings per screen/module, flattened to
//! dotted keys at load time (`[settings.general] theme-row-title = "…"`
//! becomes the key `"settings.general.theme-row-title"`).
//!
//! [`Locale::install`] resolves which catalog to use — an explicit
//! `Settings.locale` override, else the OS's own preference list via
//! `sys_locale`, else the hardcoded default [`DEFAULT_TAG`] — and installs it
//! as a gpui global, read through [`ActiveLocale`]. `de-DE` is always loaded
//! as the base and any other resolved locale is overlaid on top of it, so a
//! partial translation never shows a blank string — a missing key just keeps
//! showing German.
//!
//! Debug builds re-read the TOML from the source tree (call [`Locale::reload`]);
//! release builds use only the embedded copies. Mirrors `theme.rs`'s shape.

use std::borrow::Cow;
use std::collections::HashMap;

use gpui_kit::{App, Global, SharedString};

/// The catalog every other resolved locale is overlaid onto, and the final
/// fallback when nothing else matches. Must always have an entry in
/// [`CATALOGS`].
const DEFAULT_TAG: &str = "de-DE";

/// One shipped catalog. `filename` is used to find the on-disk copy in debug
/// builds (`locales/<filename>`); `embedded` is the release/fallback copy.
struct CatalogSource {
    tag: &'static str,
    filename: &'static str,
    embedded: &'static str,
}

/// Every catalog shipped with the app. Adding a language is: drop
/// `locales/<tag>.toml` next to `de-DE.toml`, then add one entry here.
const CATALOGS: &[CatalogSource] = &[
    CatalogSource {
        tag: "de-DE",
        filename: "de-DE.toml",
        embedded: include_str!("../locales/de-DE.toml"),
    },
    CatalogSource {
        tag: "en-US",
        filename: "en-US.toml",
        embedded: include_str!("../locales/en-US.toml"),
    },
];

#[derive(Debug, thiserror::Error)]
pub enum I18nError {
    #[error("locale TOML is invalid: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("locale TOML's top level is not a table")]
    NotATable,
    #[error("locale TOML is missing a [meta] table with `tag` and `name`")]
    InvalidMeta,
    #[error("locale TOML key {0:?} has a non-string value — every leaf must be a string")]
    NonStringLeaf(String),
}

/// Display info for one shipped catalog — the settings window's language
/// picker is built from this, so adding a catalog needs no UI code change.
#[derive(Debug, Clone)]
pub struct LocaleInfo {
    pub tag: &'static str,
    /// The native display name, from the catalog's own `[meta].name`.
    pub name: String,
}

/// A ready-to-use `Locale` for unit tests elsewhere (`menu.rs`, `keymap.rs`)
/// that need to exercise translated text without a gpui `App` — plain Rust
/// tests are this project's convention (see `theme.rs`, `keymap.rs`'s own
/// existing tests), so pure logic here takes `&Locale`, not `&App`.
#[cfg(test)]
pub(crate) fn test_locale() -> Locale {
    Locale::resolve_from(None, Source::Embedded).expect("embedded de-DE catalog must be valid")
}

/// Every catalog shipped with the app, for the settings window's picker.
pub fn available_locales() -> Vec<LocaleInfo> {
    CATALOGS
        .iter()
        .filter_map(|source| {
            let toml = Source::Preferred.catalog_toml(source);
            match load_catalog(&toml) {
                Ok((meta, _)) => Some(LocaleInfo {
                    tag: source.tag,
                    name: meta.name,
                }),
                Err(err) => {
                    log::warn!("i18n: catalog {:?} is invalid: {err}", source.tag);
                    None
                }
            }
        })
        .collect()
}

/// The active locale: the resolved catalog's strings, ready for lookup via
/// [`ActiveLocale`].
#[derive(Debug, Clone)]
pub struct Locale {
    /// The tag actually in use (may differ from what was requested — see
    /// [`resolve_tag`]).
    pub tag: String,
    /// The native display name, from the resolved catalog's `[meta].name`.
    pub name: String,
    strings: HashMap<String, String>,
}

impl Global for Locale {}

/// Read the active [`Locale`] from any app context via [`ActiveLocale::t`]
/// and friends.
pub trait ActiveLocale {
    /// Look up `key`. A missing key logs a warning and returns the raw key
    /// itself — visibly wrong in dev, never a panic or a blank string.
    fn t(&self, key: &str) -> SharedString;

    /// [`Self::t`], then substitute every `{name}` placeholder from `args`.
    fn t_fmt(&self, key: &str, args: &[(&str, &str)]) -> String {
        let mut s = self.t(key).to_string();
        for (name, value) in args {
            s = s.replace(&format!("{{{name}}}"), value);
        }
        s
    }

    /// Picks `<base_key>_one` (`n == 1`) or `<base_key>_other`, then
    /// [`Self::t_fmt`]s it with `n` merged into `args` under the name `"n"`.
    /// Two-form only (matches how both German and English inflect) — not a
    /// full CLDR plural-rule engine.
    fn t_plural(&self, base_key: &str, n: i64, args: &[(&str, &str)]) -> String {
        let key = if n == 1 {
            format!("{base_key}_one")
        } else {
            format!("{base_key}_other")
        };
        let n_string = n.to_string();
        let mut full_args = args.to_vec();
        full_args.push(("n", n_string.as_str()));
        self.t_fmt(&key, &full_args)
    }
}

impl ActiveLocale for Locale {
    fn t(&self, key: &str) -> SharedString {
        match self.strings.get(key) {
            Some(s) => SharedString::from(s.clone()),
            None => {
                log::warn!("i18n: missing key {key:?}");
                SharedString::from(key.to_owned())
            }
        }
    }
}

impl ActiveLocale for App {
    fn t(&self, key: &str) -> SharedString {
        self.global::<Locale>().t(key)
    }
}

impl Locale {
    /// Resolve and install the locale as the global. Call once at startup,
    /// after `Settings::init` (reads `Settings.locale`) and before anything
    /// that renders translated text (menus, keymap).
    pub fn install(cx: &mut App) {
        Self::apply(cx);
    }

    /// `Settings.locale` changed — re-resolve and re-install.
    pub fn reload(cx: &mut App) {
        Self::apply(cx);
    }

    fn apply(cx: &mut App) {
        let explicit = crate::settings::Settings::global(cx).locale;
        let locale =
            Self::resolve_from(explicit.as_deref(), Source::Preferred).unwrap_or_else(|err| {
                // Disk overrides (debug) can be broken by an edit; the
                // embedded catalogs are covered by tests and must not fail.
                log::warn!("i18n: {err}; falling back to the embedded catalogs");
                Self::resolve_from(explicit.as_deref(), Source::Embedded)
                    .expect("embedded locale catalogs must be valid")
            });
        log::debug!("locale applied — {} ({})", locale.tag, locale.name);
        cx.set_global(locale);
        cx.refresh_windows();
    }

    fn resolve_from(explicit: Option<&str>, source: Source) -> Result<Self, I18nError> {
        let tag = resolve_tag(explicit);

        let base_source = CATALOGS
            .iter()
            .find(|c| c.tag == DEFAULT_TAG)
            .expect("DEFAULT_TAG must have a CATALOGS entry");
        let (base_meta, mut strings) = load_catalog(&source.catalog_toml(base_source))?;

        if tag == DEFAULT_TAG {
            Ok(Self {
                tag: base_meta.tag,
                name: base_meta.name,
                strings,
            })
        } else {
            let overlay_source = CATALOGS
                .iter()
                .find(|c| c.tag == tag)
                .expect("resolve_tag only returns a tag present in CATALOGS");
            let (overlay_meta, overlay_strings) =
                load_catalog(&source.catalog_toml(overlay_source))?;
            strings.extend(overlay_strings);
            Ok(Self {
                tag: overlay_meta.tag,
                name: overlay_meta.name,
                strings,
            })
        }
    }
}

/// Where a resolve reads its TOML from — mirrors `theme.rs::Source`.
#[derive(Clone, Copy)]
enum Source {
    /// Disk in debug builds, embedded in release.
    Preferred,
    /// Always the compiled-in copy.
    Embedded,
}

impl Source {
    fn catalog_toml(self, catalog: &CatalogSource) -> Cow<'static, str> {
        #[cfg(debug_assertions)]
        if matches!(self, Source::Preferred) {
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("locales")
                .join(catalog.filename);
            match std::fs::read_to_string(&path) {
                Ok(src) => return Cow::Owned(src),
                Err(err) => {
                    log::debug!(
                        "i18n: could not read {}: {err}; using embedded",
                        path.display()
                    );
                }
            }
        }
        let _ = self;
        Cow::Borrowed(catalog.embedded)
    }
}

/// Resolve `explicit` (if it names a shipped catalog) or, failing that, the
/// OS's own preference list, to a tag that is guaranteed to be in
/// [`CATALOGS`]. Never fails — the ultimate fallback is [`DEFAULT_TAG`].
fn resolve_tag(explicit: Option<&str>) -> &'static str {
    if let Some(tag) = explicit {
        if let Some(matched) = match_catalog(tag) {
            return matched;
        }
        log::warn!(
            "i18n: configured locale {tag:?} has no catalog — following the system locale instead"
        );
    }
    for candidate in sys_locale::get_locales() {
        if let Some(matched) = match_catalog(&candidate) {
            return matched;
        }
    }
    DEFAULT_TAG
}

/// Exact tag match, else a bare-language-subtag match (`"en"` or a requested
/// `"en-GB"` both match a shipped `"en-US"`).
fn match_catalog(candidate: &str) -> Option<&'static str> {
    CATALOGS
        .iter()
        .find(|c| c.tag.eq_ignore_ascii_case(candidate))
        .map(|c| c.tag)
        .or_else(|| {
            let lang = candidate.split(['-', '_']).next().unwrap_or(candidate);
            CATALOGS
                .iter()
                .find(|c| {
                    let catalog_lang = c.tag.split(['-', '_']).next().unwrap_or(c.tag);
                    catalog_lang.eq_ignore_ascii_case(lang)
                })
                .map(|c| c.tag)
        })
}

struct CatalogMeta {
    tag: String,
    name: String,
}

fn load_catalog(src: &str) -> Result<(CatalogMeta, HashMap<String, String>), I18nError> {
    let root: toml::Value = toml::from_str(src)?;
    let table = root.as_table().ok_or(I18nError::NotATable)?;

    let meta_table = table
        .get("meta")
        .and_then(|v| v.as_table())
        .ok_or(I18nError::InvalidMeta)?;
    let tag = meta_table
        .get("tag")
        .and_then(|v| v.as_str())
        .ok_or(I18nError::InvalidMeta)?
        .to_owned();
    let name = meta_table
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or(I18nError::InvalidMeta)?
        .to_owned();

    let mut strings = HashMap::new();
    for (key, value) in table {
        if key == "meta" {
            continue;
        }
        flatten(key, value, &mut strings)?;
    }

    Ok((CatalogMeta { tag, name }, strings))
}

/// Recursively flattens nested tables into dotted keys; every leaf must be a
/// string.
fn flatten(
    prefix: &str,
    value: &toml::Value,
    out: &mut HashMap<String, String>,
) -> Result<(), I18nError> {
    match value {
        toml::Value::Table(table) => {
            for (key, value) in table {
                let path = format!("{prefix}.{key}");
                flatten(&path, value, out)?;
            }
            Ok(())
        }
        toml::Value::String(s) => {
            out.insert(prefix.to_owned(), s.clone());
            Ok(())
        }
        _ => Err(I18nError::NonStringLeaf(prefix.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flatten_nests_dotted_keys_and_rejects_non_string_leaves() {
        let toml = r#"
            [meta]
            tag = "de-DE"
            name = "Deutsch"

            [settings.general]
            title = "Allgemein"

            [menu]
            quit = "Beenden"
        "#;
        let (meta, strings) = load_catalog(toml).unwrap();
        assert_eq!(meta.tag, "de-DE");
        assert_eq!(meta.name, "Deutsch");
        assert_eq!(strings.get("settings.general.title").unwrap(), "Allgemein");
        assert_eq!(strings.get("menu.quit").unwrap(), "Beenden");

        let bad = r#"
            [meta]
            tag = "de-DE"
            name = "Deutsch"
            [x]
            y = 5
        "#;
        assert!(matches!(
            load_catalog(bad),
            Err(I18nError::NonStringLeaf(_))
        ));
    }

    #[test]
    fn missing_meta_is_an_error() {
        assert!(matches!(
            load_catalog("[x]\ny = \"z\""),
            Err(I18nError::InvalidMeta)
        ));
    }

    #[test]
    fn match_catalog_tries_exact_then_language_subtag() {
        assert_eq!(match_catalog("de-DE"), Some("de-DE"));
        assert_eq!(match_catalog("DE-de"), Some("de-DE"));
        assert_eq!(match_catalog("de"), Some("de-DE"));
        assert_eq!(match_catalog("de-AT"), Some("de-DE"));
        assert_eq!(match_catalog("fr-FR"), None);
    }

    #[test]
    fn resolve_tag_falls_back_to_default_when_nothing_matches() {
        assert_eq!(resolve_tag(Some("fr-FR")), DEFAULT_TAG);
    }

    #[test]
    fn resolve_tag_honours_an_explicit_matching_tag() {
        assert_eq!(resolve_tag(Some("de-DE")), "de-DE");
    }

    /// The real, shipped `locales/de-DE.toml` must parse — every catalog
    /// entry added across the app funnels through this file.
    #[test]
    fn embedded_de_de_catalog_parses() {
        let (meta, strings) = load_catalog(CATALOGS[0].embedded).unwrap();
        assert_eq!(meta.tag, "de-DE");
        assert_eq!(meta.name, "Deutsch");
        assert!(strings.len() > 50, "expected a substantial catalog");
    }

    struct Fixture {
        strings: HashMap<String, String>,
    }
    impl ActiveLocale for Fixture {
        fn t(&self, key: &str) -> SharedString {
            match self.strings.get(key) {
                Some(s) => SharedString::from(s.clone()),
                None => SharedString::from(key.to_owned()),
            }
        }
    }

    #[test]
    fn missing_key_returns_the_key_itself() {
        let f = Fixture {
            strings: HashMap::new(),
        };
        assert_eq!(f.t("no.such.key").as_ref(), "no.such.key");
    }

    #[test]
    fn t_fmt_substitutes_named_placeholders() {
        let mut strings = HashMap::new();
        strings.insert("greet".to_owned(), "Hallo {name}!".to_owned());
        let f = Fixture { strings };
        assert_eq!(f.t_fmt("greet", &[("name", "Welt")]), "Hallo Welt!");
    }

    #[test]
    fn t_plural_picks_one_vs_other() {
        let mut strings = HashMap::new();
        strings.insert("conflicts_one".to_owned(), "{n} Konflikt".to_owned());
        strings.insert("conflicts_other".to_owned(), "{n} Konflikte".to_owned());
        let f = Fixture { strings };
        assert_eq!(f.t_plural("conflicts", 1, &[]), "1 Konflikt");
        assert_eq!(f.t_plural("conflicts", 2, &[]), "2 Konflikte");
        assert_eq!(f.t_plural("conflicts", 0, &[]), "0 Konflikte");
    }
}
