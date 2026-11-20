//! Formatting helpers with no UI.

/// "Ana María Pérez" → "AP"; "" → "?".
pub fn initials(name: &str) -> String {
    let words: Vec<&str> = name.split_whitespace().filter(|w| w.chars().next().is_some_and(char::is_alphanumeric)).collect();
    let pick = |w: &str| w.chars().next().map(|c| c.to_uppercase().collect::<String>()).unwrap_or_default();
    match words.as_slice() {
        [] => "?".into(),
        [one] => pick(one),
        [first, .., last] => format!("{}{}", pick(first), pick(last)),
    }
}

/// `2026-11-19 16:20` in the author's own time zone.
pub fn datetime(secs: i64, offset: i32) -> String {
    let t = secs + offset as i64;
    let days = t.div_euclid(86_400);
    let rem = t.rem_euclid(86_400);
    let (y, m, d) = civil(days);
    format!("{y:04}-{m:02}-{d:02} {:02}:{:02}", rem / 3600, rem % 3600 / 60)
}

/// Days since 1970-01-01 → (year, month, day). Howard Hinnant's algorithm.
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// 1234567 → "1 234 567" (narrow no-break space).
pub fn group(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push('\u{202F}');
        }
        out.push(ch);
    }
    out
}

/// 1_234_567_890 → "1.2 GB" (units from the catalog: fr "Go").
pub fn bytes(n: u64) -> String {
    const K: f64 = 1024.;
    let f = n as f64;
    if f < K {
        i18n::t!("app.size.b", n = n)
    } else if f < K * K {
        i18n::t!("app.size.kb", n = format!("{:.0}", f / K))
    } else if f < K * K * K {
        i18n::t!("app.size.mb", n = format!("{:.1}", f / K / K))
    } else if f < K * K * K * K {
        i18n::t!("app.size.gb", n = format!("{:.1}", f / K / K / K))
    } else {
        i18n::t!("app.size.tb", n = format!("{:.1}", f / K / K / K / K))
    }
}

/// How long ago `secs` (since the epoch) was, from `now`: "3 days ago".
pub fn ago(secs: i64, now: i64) -> String {
    let d = (now - secs).max(0);
    match d {
        0..60 => i18n::t!("app.ago.now"),
        60..3600 => i18n::tn!("app.ago.minutes", d / 60),
        3600..86_400 => i18n::tn!("app.ago.hours", d / 3600),
        86_400..2_592_000 => i18n::tn!("app.ago.days", d / 86_400),
        2_592_000..31_536_000 => i18n::tn!("app.ago.months", d / 2_592_000),
        _ => i18n::tn!("app.ago.years", d / 31_536_000),
    }
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initials_and_dates() {
        assert_eq!(initials("Ana María Pérez"), "AP");
        assert_eq!(initials("dependabot[bot]"), "D");
        assert_eq!(initials("  "), "?");
        assert_eq!(initials("álvaro ruiz"), "ÁR");
        assert_eq!(datetime(0, 0), "1970-01-01 00:00");
        assert_eq!(datetime(1_795_000_000, 0), "2026-11-18 11:06");
        assert_eq!(datetime(1_795_000_000, -5 * 3600), "2026-11-18 06:06");
        assert_eq!(datetime(951_782_400, 0), "2000-02-29 00:00");
        assert_eq!(group(60769), "60\u{202F}769");
        i18n::set_locale("en");
        assert_eq!(bytes(512), "512 B");
        assert_eq!(bytes(1_288_490_189), "1.2 GB");
        assert_eq!(ago(100, 100 + 3 * 86_400), "3 days ago");
        assert_eq!(ago(100, 130), "just now");
    }
}
