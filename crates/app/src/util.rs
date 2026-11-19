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
    }
}
