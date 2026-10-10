//! `~/.config/kosuzu/config.json` load/save.
//!
//! - read failure / invalid JSON / non-object top level -> all defaults;
//! - unknown keys are dropped; a mistyped field falls back to its own default
//!   while other fields still apply;
//! - write failure is silently ignored.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    pub source_lang: String,
    pub target_lang: String,
    /// small / medium / tiny
    pub ocr_model: String,
    /// auto | grim | niri | portal
    pub screenshot_backend: String,
    pub auto_translate_after_ocr: bool,
    /// google | llm
    pub translate_engine: String,
    /// any OpenAI-compatible endpoint root: an API provider, or a local server
    pub llm_base_url: String,
    pub llm_model: String,
    pub llm_api_key: String,
    /// pinned: keep the popup visible even when unfocused
    pub popup_lock: bool,
    /// show the source card in the compact popup (off: translation only)
    pub popup_show_input: bool,
    pub show_status_bar: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            source_lang: "auto".into(),
            target_lang: "en".into(),
            ocr_model: "small".into(),
            screenshot_backend: "auto".into(),
            auto_translate_after_ocr: true,
            translate_engine: "google".into(),
            llm_base_url: String::new(),
            llm_model: String::new(),
            llm_api_key: String::new(),
            popup_lock: false,
            popup_show_input: true,
            show_status_bar: true,
        }
    }
}

/// `$XDG_*` unset or empty falls back to `~/.<fallback>`.
fn xdg_dir(var: &str, fallback: &str) -> PathBuf {
    std::env::var_os(var)
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| {
            let home = std::env::var_os("HOME").unwrap_or_default();
            PathBuf::from(home).join(fallback)
        })
}

/// `$XDG_DATA_HOME` or `~/.local/share`.
pub fn data_dir() -> PathBuf {
    xdg_dir("XDG_DATA_HOME", ".local/share")
}

impl Config {
    pub fn path() -> PathBuf {
        xdg_dir("XDG_CONFIG_HOME", ".config")
            .join("kosuzu")
            .join("config.json")
    }

    pub fn load() -> Self {
        Self::load_from(&Self::path())
    }

    pub fn load_from(path: &std::path::Path) -> Self {
        let Ok(raw) = std::fs::read_to_string(path) else {
            return Self::default();
        };
        let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) else {
            return Self::default();
        };
        let Some(obj) = val.as_object() else {
            return Self::default();
        };
        let mut cfg = Self::default();
        // per-field deserialize: a mistyped key is dropped on its own
        macro_rules! take {
            ($($field:ident),* $(,)?) => {$(
                if let Some(v) = obj.get(stringify!($field)) {
                    if let Ok(x) = serde_json::from_value(v.clone()) {
                        cfg.$field = x;
                    }
                }
            )*};
        }
        take!(
            source_lang,
            target_lang,
            ocr_model,
            screenshot_backend,
            auto_translate_after_ocr,
            translate_engine,
            llm_base_url,
            llm_model,
            llm_api_key,
            popup_lock,
            popup_show_input,
            show_status_bar,
        );
        cfg
    }

    pub fn save(&self) {
        self.save_to(&Self::path())
    }

    pub fn save_to(&self, path: &std::path::Path) {
        let Ok(json) = serde_json::to_string_pretty(self) else {
            return;
        };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return;
            }
        }
        if std::fs::write(path, json).is_err() {
            return;
        }
        #[cfg(unix)]
        // the api key sits in plain text; keep the file to the owner
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("kosuzu-config-test-{}-{name}", std::process::id()))
    }

    #[test]
    fn missing_file_yields_defaults() {
        let cfg = Config::load_from(&tmp("missing.json"));
        assert_eq!(cfg, Config::default());
    }

    #[test]
    fn invalid_json_yields_defaults() {
        let p = tmp("bad.json");
        std::fs::write(&p, "{oops").unwrap();
        let cfg = Config::load_from(&p);
        assert_eq!(cfg, Config::default());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn unknown_keys_and_wrong_types_dropped_per_field() {
        let p = tmp("mixed.json");
        std::fs::write(
            &p,
            r#"{
              "source_lang": "en",
              "unknown_option": 42,
              "target_lang": 123
            }"#,
        )
        .unwrap();
        let cfg = Config::load_from(&p);
        assert_eq!(cfg.source_lang, "en"); // type ok -> applies
        assert_eq!(cfg.target_lang, "en"); // wrong type -> default
        assert_eq!(cfg.ocr_model, "small"); // unknown keys do not affect known fields
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn non_object_top_level_yields_defaults() {
        let p = tmp("array.json");
        std::fs::write(&p, "[1,2,3]").unwrap();
        assert_eq!(Config::load_from(&p), Config::default());
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn save_load_roundtrip() {
        let p = tmp("roundtrip.json");
        let mut cfg = Config::default();
        cfg.source_lang = "ja".into();
        cfg.popup_lock = true;
        cfg.save_to(&p);
        assert_eq!(Config::load_from(&p), cfg);
        let _ = std::fs::remove_file(&p);
    }

    #[test]
    fn save_ignores_unwritable_path() {
        // save failures must stay silent
        let cfg = Config::default();
        cfg.save_to(std::path::Path::new("/nonexistent-dir/kosuzu/cfg.json"));
    }

    #[cfg(unix)]
    #[test]
    fn saved_config_is_owner_only_it_holds_the_api_key() {
        use std::os::unix::fs::PermissionsExt;
        let p = tmp("perm.json");
        let mut cfg = Config::default();
        cfg.llm_api_key = "sk-secret".into();
        cfg.save_to(&p);
        let mode = std::fs::metadata(&p).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "api key must not be world-readable");
        let _ = std::fs::remove_file(&p);
    }
}
