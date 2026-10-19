//! Formatting helpers so plan values read like the CLI flags people type.

/// Round to `digits` decimals.
pub fn round_to(x: f64, digits: i32) -> f64 {
    if !x.is_finite() {
        return x;
    }
    let p = 10f64.powi(digits);
    let r = (x * p).round() / p;
    if r == 0.0 {
        0.0
    } else {
        r
    }
}

/// A number with at most two decimals and no trailing zeros.
pub fn num(x: f64) -> String {
    let s = format!("{:.2}", round_to(x, 2));
    let s = s.trim_end_matches('0').trim_end_matches('.').to_string();
    if s == "-0" {
        "0".into()
    } else {
        s
    }
}

/// `-3dB`, `+1.5dB`, `0dB`.
pub fn fmt_db(x: f64) -> String {
    let n = num(x);
    if x > 0.0 && n != "0" {
        format!("+{n}dB")
    } else {
        format!("{n}dB")
    }
}

/// `120Hz`, `2kHz`, `1.25kHz`.
pub fn fmt_freq(f: f64) -> String {
    if f >= 1000.0 {
        format!("{}kHz", num(f / 1000.0))
    } else {
        format!("{}Hz", num(f))
    }
}

/// A fraction as a percentage: `0.975` → `97.5%`.
pub fn fmt_pct(x: f64) -> String {
    format!("{}%", num(x * 100.0))
}

/// JSON-friendly number: non-finite values become null.
pub fn jnum(x: f64, digits: i32) -> serde_json::Value {
    if x.is_finite() {
        serde_json::json!(round_to(x, digits))
    } else {
        serde_json::Value::Null
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ferraus_core::units;

    #[test]
    fn formats_parse_back() {
        assert_eq!(fmt_db(-3.0), "-3dB");
        assert_eq!(fmt_db(1.5), "+1.5dB");
        assert_eq!(fmt_db(0.0), "0dB");
        assert_eq!(fmt_freq(120.0), "120Hz");
        assert_eq!(fmt_freq(2000.0), "2kHz");
        assert_eq!(fmt_freq(31.5), "31.5Hz");
        assert_eq!(fmt_pct(0.975), "97.5%");
        assert_eq!(units::parse_db(&fmt_db(1.25)).unwrap(), 1.25);
        assert_eq!(units::parse_freq(&fmt_freq(1250.0)).unwrap(), 1250.0);
        assert!((units::parse_percent(&fmt_pct(0.5)).unwrap() - 0.5).abs() < 1e-12);
    }
}
