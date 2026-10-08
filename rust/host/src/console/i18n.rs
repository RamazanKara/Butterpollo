//! Reuse the previous locale catalogues without a browser script or JS build.
use super::esc;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, OnceLock},
};

macro_rules! catalogues {
    ($($locale:literal),* $(,)?) => {
        const CATALOGUES: &[(&str, &str)] = &[$((
            $locale,
            include_str!(concat!("../../../assets/locale/", $locale, ".json"))
        )),*];
    };
}
catalogues!(
    "bg", "cs", "de", "en", "en_GB", "en_US", "es", "fr", "hu", "it", "ja", "ko", "pl", "pt",
    "pt_BR", "ru", "sv", "tr", "uk", "vi", "zh", "zh_TW"
);

type Dictionary = BTreeMap<String, String>;
type Cache = Mutex<BTreeMap<&'static str, Arc<Dictionary>>>;
pub fn language_name(code: &str) -> &str {
    match code {
        "en" => "English",
        "en_GB" => "English (UK)",
        "en_US" => "English (US)",
        "bg" => "Български",
        "cs" => "Čeština",
        "de" => "Deutsch",
        "es" => "Español",
        "fr" => "Français",
        "hu" => "Magyar",
        "it" => "Italiano",
        "ja" => "日本語",
        "ko" => "한국어",
        "pl" => "Polski",
        "pt" => "Português",
        "pt_BR" => "Português (Brasil)",
        "ru" => "Русский",
        "sv" => "Svenska",
        "tr" => "Türkçe",
        "uk" => "Українська",
        "vi" => "Tiếng Việt",
        "zh" => "中文（简体）",
        "zh_TW" => "中文（繁體）",
        _ => code,
    }
}
pub fn locale(raw: &str) -> &'static str {
    let normalized = raw.trim().replace('-', "_");
    CATALOGUES
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(&normalized))
        .or_else(|| {
            CATALOGUES.iter().find(|(name, _)| {
                name.eq_ignore_ascii_case(normalized.split('_').next().unwrap_or(""))
            })
        })
        .map_or("en", |(name, _)| *name)
}
fn dictionary(language: &'static str) -> Arc<Dictionary> {
    static CACHE: OnceLock<Cache> = OnceLock::new();
    let mut cache = CACHE.get_or_init(Default::default).lock().unwrap();
    cache
        .entry(language)
        .or_insert_with(|| {
            fn flatten(value: &Value, path: &str, output: &mut Dictionary) {
                if let Some(object) = value.as_object() {
                    for (key, value) in object {
                        flatten(value, &format!("{path}/{key}"), output);
                    }
                } else if let Some(text) = value.as_str() {
                    output.insert(path.into(), text.into());
                }
            }
            let mut result = Dictionary::new();
            let source = CATALOGUES
                .iter()
                .find(|(name, _)| *name == language)
                .unwrap()
                .1;
            flatten(
                &serde_json::from_str(source).expect("bundled locale JSON"),
                "",
                &mut result,
            );
            Arc::new(result)
        })
        .clone()
}
fn key(english: &str) -> Option<&'static str> {
    match english {
        "Overview" => Some("/navbar/home"),
        "Library" => Some("/navbar/applications"),
        "Devices" => Some("/clients/nav"),
        "Settings" => Some("/navbar/configuration"),
        "API tokens" => Some("/navbar/api_tokens"),
        "Sign out" => Some("/navbar/logout"),
        "Sign in" => Some("/auth/login_sign_in"),
        "System" => Some("/navbar/theme_auto"),
        "Appearance" => Some("/navbar/toggle_theme"),
        "Application name" => Some("/apps/app_name"),
        "Command (leave empty for desktop)" => Some("/apps/cmd"),
        "Working directory" => Some("/apps/working_dir"),
        "Use default" => Some("/_common/default_global"),
        _ => None,
    }
}
fn english_keys() -> &'static BTreeMap<String, String> {
    static KEYS: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    KEYS.get_or_init(|| {
        let mut keys = BTreeMap::new();
        for (path, text) in dictionary("en").iter() {
            // HTML and interpolated messages need their original parameter API.
            if !text.contains(['<', '>', '{', '}']) {
                keys.entry(text.to_ascii_lowercase())
                    .or_insert_with(|| path.clone());
            }
        }
        keys
    })
}
fn lookup<'a>(language: &'a Dictionary, english: &str, path: Option<&str>) -> Option<&'a str> {
    let path = path.or_else(|| key(english)).or_else(|| {
        english_keys()
            .get(&english.to_ascii_lowercase())
            .map(String::as_str)
    })?;
    language
        .get(path)
        .filter(|v| !v.is_empty() && !v.contains(['<', '>', '{', '}']))
        .map(String::as_str)
}
pub fn message(english: &str) -> String {
    label("", english)
}
pub fn label(name: &str, english: &str) -> String {
    let (section, name) = if let Some(name) = name.strip_prefix("app_") {
        ("apps", name)
    } else {
        (
            "config",
            name.strip_prefix("cfg_")
                .or_else(|| name.strip_prefix("client_"))
                .unwrap_or(name),
        )
    };
    let path = format!("/{section}/{}", name.replace('-', "_"));
    let path = dictionary("en").contains_key(&path).then_some(path);
    format!(
        "\u{fdd0}{}\u{fdd1}",
        serde_json::to_string(&(path, english)).unwrap()
    )
}
/// Protect configuration, names, commands and JSON from language substitution.
pub fn data(value: &str) -> String {
    format!("\u{fdd2}{}\u{fdd3}", esc(value))
}
pub fn render(body: &str, raw_locale: &str) -> String {
    let selected = locale(raw_locale);
    let dictionary = dictionary(selected);
    let english = selected == "en";
    let mut output = String::with_capacity(body.len());
    let mut remaining = body;
    let mut in_tag = false;
    let mut quote = None;
    while !remaining.is_empty() {
        if let Some(rest) = remaining.strip_prefix('\u{fdd2}')
            && let Some((value, rest)) = rest.split_once('\u{fdd3}')
        {
            output.push_str(value);
            remaining = rest;
            continue;
        }
        if let Some(rest) = remaining.strip_prefix('\u{fdd0}')
            && let Some((value, rest)) = rest.split_once('\u{fdd1}')
            && let Ok((path, fallback)) = serde_json::from_str::<(Option<String>, String)>(value)
        {
            let text = if english {
                &fallback
            } else {
                lookup(&dictionary, &fallback, path.as_deref()).unwrap_or(&fallback)
            };
            output.push_str(&esc(text));
            remaining = rest;
            continue;
        }
        let first = remaining.chars().next().unwrap();
        if in_tag || first == '<' {
            if first == '<' && !in_tag {
                in_tag = true;
            } else if matches!(first, '\'' | '"') {
                if quote == Some(first) {
                    quote = None;
                } else if quote.is_none() {
                    quote = Some(first);
                }
            } else if first == '>' && quote.is_none() {
                in_tag = false;
            }
            output.push(first);
            remaining = &remaining[first.len_utf8()..];
        } else {
            let end = remaining
                .find(['<', '\u{fdd0}', '\u{fdd2}'])
                .unwrap_or(remaining.len())
                .max(first.len_utf8());
            let text = &remaining[..end];
            let trimmed = text.trim();
            if !english && let Some(translated) = lookup(&dictionary, trimmed, None) {
                let start = text.len() - text.trim_start().len();
                output.push_str(&text[..start]);
                output.push_str(&esc(translated));
                output.push_str(&text[start + trimmed.len()..]);
            } else {
                output.push_str(text);
            }
            remaining = &remaining[end..];
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn inherited_locales_translate_controls_and_preserve_names_values_and_json() {
        let body = format!(
            "<h1>{}</h1><label>{}<input value=\"{}\"></label><h2>{}</h2><pre>{}</pre><button>Save</button>",
            message("Settings"),
            label("cfg_locale", "Language"),
            data("Save"),
            data("Settings"),
            data("{\"Save\":\"Settings\"}")
        );
        let de = render(&body, "de-DE");
        assert!(de.contains("<h1>Konfiguration</h1>"));
        assert!(de.contains("<h2>Settings</h2>"));
        assert!(de.contains("value=\"Save\""));
        assert!(de.contains("{&quot;Save&quot;:&quot;Settings&quot;}"));
        assert!(de.contains("<button>Speichern</button>"));
        let malicious = data("\u{fdd0}[null,\"Settings\"]\u{fdd1}<script>");
        assert_eq!(
            render(&malicious, "de"),
            "&#64976;[null,&quot;Settings&quot;]&#64977;&lt;script&gt;"
        );
        assert_eq!(locale("../../evil"), "en");
        assert_eq!(locale("zh-TW"), "zh_TW");
        for (name, _) in CATALOGUES {
            assert!(!dictionary(name).is_empty());
        }
    }
}
