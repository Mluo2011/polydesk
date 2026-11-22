#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OddsFormat {
    Cents,
    Prob,
    Decimal,
    Moneyline,
}

impl OddsFormat {
    pub fn from_str_code(s: &str) -> Self {
        match s {
            "prob" => Self::Prob,
            "decimal" => Self::Decimal,
            "moneyline" => Self::Moneyline,
            _ => Self::Cents,
        }
    }

    pub fn as_str_code(self) -> &'static str {
        match self {
            Self::Cents => "cents",
            Self::Prob => "prob",
            Self::Decimal => "decimal",
            Self::Moneyline => "moneyline",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Cents => "¢",
            Self::Prob => "%",
            Self::Decimal => "Dec",
            Self::Moneyline => "ML",
        }
    }

    pub fn cycle(self) -> Self {
        match self {
            Self::Cents => Self::Prob,
            Self::Prob => Self::Decimal,
            Self::Decimal => Self::Moneyline,
            Self::Moneyline => Self::Cents,
        }
    }
}

pub fn format_odds(price: Option<f64>, fmt: OddsFormat, tick_size: f64) -> String {
    let Some(raw) = price else {
        return "—".to_string();
    };
    if !raw.is_finite() || raw <= 0.0 {
        return "—".to_string();
    }
    let p = raw.clamp(0.0001, 0.9999);
    let decimals = if tick_size < 0.01 { 1 } else { 0 };

    match fmt {
        OddsFormat::Cents => format!("{:.prec$}¢", p * 100.0, prec = decimals),
        OddsFormat::Prob => format!("{:.prec$}%", p * 100.0, prec = decimals),
        OddsFormat::Decimal => format!("{:.2}x", 1.0 / p),
        OddsFormat::Moneyline => {
            if p >= 0.5 {
                let ml = -((p / (1.0 - p)) * 100.0).round() as i64;
                format!("{ml}")
            } else {
                let ml = (((1.0 - p) / p) * 100.0).round() as i64;
                format!("+{ml}")
            }
        }
    }
}

pub fn format_usd(v: f64, signed: bool) -> String {
    if !v.is_finite() {
        return "$0.00".to_string();
    }
    let abs = v.abs();
    let num = if abs >= 1_000_000.0 {
        format!("{:.2}M", abs / 1_000_000.0)
    } else if abs >= 10_000.0 {
        format!("{:.1}k", abs / 1_000.0)
    } else {
        format!("{abs:.2}")
    };
    if signed {
        if v >= 0.0 {
            format!("+${num}")
        } else {
            format!("-${num}")
        }
    } else if v < 0.0 {
        format!("-${num}")
    } else {
        format!("${num}")
    }
}

pub fn format_size(v: f64) -> String {
    if !v.is_finite() || v <= 0.0 {
        return String::new();
    }
    if v >= 1_000_000.0 {
        format!("{:.1}M", v / 1_000_000.0)
    } else if v >= 10_000.0 {
        format!("{:.1}k", v / 1_000.0)
    } else if v >= 100.0 {
        format!("{v:.0}")
    } else {
        format!("{v:.1}")
    }
}
