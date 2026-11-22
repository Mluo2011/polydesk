#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    En,
    Zh,
}

impl Lang {
    pub fn from_code(s: &str) -> Self {
        if s.eq_ignore_ascii_case("zh") {
            Self::Zh
        } else {
            Self::En
        }
    }

    pub fn as_code(self) -> &'static str {
        match self {
            Self::En => "en",
            Self::Zh => "zh",
        }
    }

    pub fn toggle(self) -> Self {
        match self {
            Self::En => Self::Zh,
            Self::Zh => Self::En,
        }
    }
}

#[inline]
pub fn tr(lang: Lang, en: &'static str, zh: &'static str) -> &'static str {
    match lang {
        Lang::En => en,
        Lang::Zh => zh,
    }
}
