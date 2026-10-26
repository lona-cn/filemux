use std::fmt;

pub const PREF_LANGUAGE_KEY: &str = "filemux.language";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Language {
    English,
    Chinese,
}

impl Language {
    pub fn detect() -> Self {
        Self::from_locale(sys_locale::get_locale().as_deref())
    }

    pub fn from_locale(locale: Option<&str>) -> Self {
        let primary = locale
            .unwrap_or_default()
            .trim()
            .split(['-', '_', '.', '@'])
            .next()
            .unwrap_or_default();
        if primary.eq_ignore_ascii_case("zh") {
            Self::Chinese
        } else {
            Self::English
        }
    }

    pub const fn text(self, en: &'static str, zh: &'static str) -> &'static str {
        match self {
            Self::English => en,
            Self::Chinese => zh,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LanguagePreference {
    #[default]
    System,
    English,
    Chinese,
}

impl LanguagePreference {
    pub fn from_saved(value: Option<&str>) -> Self {
        match value {
            Some("en") => Self::English,
            Some("zh-CN") => Self::Chinese,
            _ => Self::System,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::System => "auto",
            Self::English => "en",
            Self::Chinese => "zh-CN",
        }
    }

    pub const fn resolve(self, system: Language) -> Language {
        match self {
            Self::System => system,
            Self::English => Language::English,
            Self::Chinese => Language::Chinese,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UiText {
    pub en: &'static str,
    pub zh: &'static str,
}

impl UiText {
    pub const fn new(en: &'static str, zh: &'static str) -> Self {
        Self { en, zh }
    }

    pub const fn get(self, language: Language) -> &'static str {
        language.text(self.en, self.zh)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UiNotice {
    pub text: UiText,
    pub detail: Option<String>,
}

impl UiNotice {
    pub const fn new(en: &'static str, zh: &'static str) -> Self {
        Self {
            text: UiText::new(en, zh),
            detail: None,
        }
    }

    pub fn with_detail(en: &'static str, zh: &'static str, detail: impl Into<String>) -> Self {
        Self {
            text: UiText::new(en, zh),
            detail: Some(detail.into()),
        }
    }
}

impl fmt::Display for UiNotice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.text.en)?;
        if let Some(detail) = &self.detail {
            write!(formatter, ": {detail}")?;
        }
        Ok(())
    }
}

impl std::error::Error for UiNotice {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chinese_primary_language_accepts_scripts_regions_and_posix_locales() {
        for locale in [
            "zh",
            "zh-CN",
            "zh-TW",
            "zh-Hans",
            "zh-Hant-HK",
            "ZH_cn",
            "zh_CN.UTF-8",
            "zh_TW.Big5@variant",
        ] {
            assert_eq!(
                Language::from_locale(Some(locale)),
                Language::Chinese,
                "{locale}"
            );
        }
        for locale in [
            "", "en-US", "en_CN", "ja-JP", "C", "POSIX", "zhang", "uz-CN",
        ] {
            assert_eq!(
                Language::from_locale(Some(locale)),
                Language::English,
                "{locale}"
            );
        }
        assert_eq!(Language::from_locale(None), Language::English);
    }

    #[test]
    fn saved_manual_choices_override_both_system_languages() {
        for (preference, expected) in [
            (LanguagePreference::English, Language::English),
            (LanguagePreference::Chinese, Language::Chinese),
        ] {
            let restored = LanguagePreference::from_saved(Some(preference.as_str()));
            assert_eq!(restored, preference);
            for system in [Language::English, Language::Chinese] {
                assert_eq!(restored.resolve(system), expected);
            }
        }
    }

    #[test]
    fn automatic_and_unknown_preferences_follow_the_current_system() {
        assert_eq!(LanguagePreference::System.as_str(), "auto");
        for saved in [None, Some("auto"), Some("unknown")] {
            let preference = LanguagePreference::from_saved(saved);
            assert_eq!(preference, LanguagePreference::default());
            for system in [Language::English, Language::Chinese] {
                assert_eq!(preference.resolve(system), system);
            }
        }
    }
}
