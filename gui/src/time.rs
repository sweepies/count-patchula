//! The little time arithmetic the window needs: the report's RFC 3339 stamps
//! as seconds since the Unix epoch, and how long ago or how far off they are.

/// Seconds since the Unix epoch for an RFC 3339 timestamp such as
/// `2026-09-24T19:36:54Z` or `2026-09-24T19:36:54.120+02:00`.
pub fn parse(text: &str) -> Option<i64> {
    let bytes = text.as_bytes();
    let number = |range: std::ops::Range<usize>| -> Option<i64> {
        let part = bytes.get(range)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0, |n, d| n * 10 + i64::from(d - b'0')))
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    let punctuation = [(4, b'-'), (7, b'-'), (13, b':'), (16, b':')];
    if punctuation
        .iter()
        .any(|(at, byte)| bytes.get(*at) != Some(byte))
        || !matches!(bytes.get(10), Some(b'T' | b't' | b' '))
        || !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
    {
        return None;
    }
    // A fraction of a second, then the zone.
    let mut at = 19;
    if bytes.get(at) == Some(&b'.') {
        at += 1;
        while bytes.get(at).is_some_and(u8::is_ascii_digit) {
            at += 1;
        }
    }
    let offset = match bytes.get(at)? {
        b'Z' | b'z' if at + 1 == bytes.len() => 0,
        sign @ (b'+' | b'-') if at + 6 == bytes.len() && bytes[at + 3] == b':' => {
            let seconds = number(at + 1..at + 3)? * 3600 + number(at + 4..at + 6)? * 60;
            if *sign == b'+' { seconds } else { -seconds }
        }
        _ => return None,
    };
    Some(days(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset)
}

/// Days from 1970-01-01 to a date in the proleptic Gregorian calendar.
fn days(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let of_era = year.rem_euclid(400);
    let of_year = (153 * ((month + 9) % 12) + 2) / 5 + day - 1;
    era * 146_097 + of_era * 365 + of_era / 4 - of_era / 100 + of_year - 719_468
}

/// The clock, in seconds since the Unix epoch.
pub fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}

/// A span as the Count writes it elsewhere: hours up to two days, then days.
pub fn span(seconds: i64) -> String {
    let hours = seconds.max(0) / 3600;
    if hours < 48 {
        format!("{hours}h")
    } else {
        format!("{}d", hours / 24)
    }
}

/// The date of a timestamp, as written: `2026-09-24`.
pub fn date(text: &str) -> &str {
    text.get(..10).unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_parse_with_fractions_and_offsets() {
        assert_eq!(parse("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(parse("2026-09-24T19:36:54Z"), Some(1_790_278_614));
        assert_eq!(parse("2026-09-24T19:36:54.120Z"), Some(1_790_278_614));
        assert_eq!(parse("2026-09-24T21:36:54+02:00"), Some(1_790_278_614));
        assert_eq!(parse("2026-09-24T14:36:54-05:00"), Some(1_790_278_614));
        assert_eq!(parse("2000-02-29T12:00:00Z"), Some(951_825_600));
    }

    #[test]
    fn anything_else_is_not_a_time() {
        for text in [
            "",
            "2026-09-24",
            "2026-13-01T00:00:00Z",
            "2026-09-24T19:36:54",
            "soon",
        ] {
            assert_eq!(parse(text), None, "{text}");
        }
    }

    #[test]
    fn spans_switch_from_hours_to_days_at_two_days() {
        assert_eq!(span(0), "0h");
        assert_eq!(span(47 * 3600 + 3599), "47h");
        assert_eq!(span(48 * 3600), "2d");
        assert_eq!(span(-5), "0h");
    }
}
