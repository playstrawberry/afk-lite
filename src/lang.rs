//! The script's texts: `lang/{en,fr,es,tr}.lang`, embedded and checked for the same keys while it compiles.
use sp_lang::{Catalog, Language};

static LANG: Catalog = sp_lang::catalog!("afk-lite");

/// The text of `key` in the language of `locale` (English for any other), with `{0}` filled from `args`.
pub fn text(locale: &str, key: &str, args: &[&str]) -> String {
    let raw = LANG
        .get_or_en(Language::of_locale(locale), key)
        .unwrap_or_default();
    sp_lang::render(raw, args, "")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_key_the_script_names_has_a_text_in_every_language() {
        let own = [
            "description",
            "permission_use",
            "command_afk",
            "players_only",
            "no_permission",
            "you_afk",
            "you_back",
            "now_afk",
            "no_longer_afk",
            "cooldown",
            "busy",
            "config_error",
            "data_error",
            "save_error",
        ];
        for key in own {
            for language in Language::ALL {
                assert!(
                    LANG.get(language, key).is_some_and(|t| !t.is_empty()),
                    "{key} in {}",
                    language.code()
                );
            }
        }
        assert_eq!(LANG.len(), own.len(), "a key nobody names");
    }

    #[test]
    fn a_name_is_filled_in_once_in_the_readers_language() {
        assert!(text("tr_tr", "now_afk", &["Steve"]).contains("Steve"));
        assert_ne!(
            text("tr_tr", "now_afk", &["Steve"]),
            text("en_us", "now_afk", &["Steve"])
        );
        assert!(text("en_us", "now_afk", &["{0}"]).contains("{0}"));
        assert_eq!(text("xx_yy", "you_afk", &[]), text("en_us", "you_afk", &[]));
    }
}
