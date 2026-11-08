//! Minimal RFC 3339 timestamp parsing (server `lastWatchedAt` etc.) without a date-time crate.

/// Days from 1970-01-01 to the given civil date (Howard Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Parses `YYYY-MM-DDTHH:MM:SS[.fff…](Z|±HH:MM)` (also a space instead of `T`, or a plain
/// date) into Unix milliseconds. Returns `None` for anything else.
pub fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    let num = |r: std::ops::Range<usize>| -> Option<i64> {
        let part = s.get(r)?;
        if part.bytes().all(|c| c.is_ascii_digit()) {
            part.parse().ok()
        } else {
            None
        }
    };
    if b.len() < 10 || b[4] != b'-' || b[7] != b'-' {
        return None;
    }
    let (y, mo, d) = (num(0..4)?, num(5..7)?, num(8..10)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    let days = days_from_civil(y, mo, d);
    if b.len() == 10 {
        return Some(days * 86_400_000);
    }
    if !matches!(b[10], b'T' | b't' | b' ') || b.len() < 19 || b[13] != b':' || b[16] != b':' {
        return None;
    }
    let (h, mi, sec) = (num(11..13)?, num(14..16)?, num(17..19)?);
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }
    let mut i = 19;
    let mut ms = 0i64;
    if b.get(i) == Some(&b'.') {
        i += 1;
        let start = i;
        while i < b.len() && b[i].is_ascii_digit() {
            i += 1;
        }
        if i == start {
            return None;
        }
        let frac = &s[start..i.min(start + 3)];
        ms = frac.parse::<i64>().ok()? * 10i64.pow(3 - frac.len() as u32);
    }
    let offset_min = match b.get(i) {
        None => 0, // no zone: treat as UTC
        Some(b'Z' | b'z') if i + 1 == b.len() => 0,
        Some(&sign @ (b'+' | b'-')) if b.len() == i + 6 && b[i + 3] == b':' => {
            let oh = num(i + 1..i + 3)?;
            let om = num(i + 4..i + 6)?;
            let v = oh * 60 + om;
            if sign == b'+' { v } else { -v }
        }
        _ => return None,
    };
    Some(((days * 24 + h) * 60 + mi - offset_min) * 60_000 + sec * 1000 + ms)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2026-11-08T12:34:56.789Z"),
            Some(1_794_141_296_789)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-11-08T15:34:56.789+03:00"),
            Some(1_794_141_296_789)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-11-08 12:34:56.7Z"),
            Some(1_794_141_296_700)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-11-08T12:34:56.123456Z"),
            Some(1_794_141_296_123)
        );
        assert_eq!(parse_rfc3339_ms("2000-02-29"), Some(951_782_400_000));
        assert_eq!(parse_rfc3339_ms("1969-12-31T23:59:59Z"), Some(-1_000));
        assert_eq!(parse_rfc3339_ms("garbage"), None);
        assert_eq!(parse_rfc3339_ms("2026-13-01T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_ms("2026-11-08T12:34:56.Z"), None);
        assert_eq!(parse_rfc3339_ms("2026-11-08T12:34:56+0300"), None);
    }
}
