//! # From_ line
//!
//! Parser and formatter of the `From_` line (the postmark) opening every
//! message of an mbox: `From `, the envelope sender, then an asctime(3)
//! date.
//!
//! Real files stretch the grammar, so the parser is lenient where writers
//! disagree and strict where it matters. It accepts any whitespace run
//! between fields (Apache archives put two spaces before the date), a
//! sender that is empty, quoted with spaces, `-` (Thunderbird) or
//! `MAILER-DAEMON`, a timezone before or after the year, and two-digit
//! years. It still demands a full day, month, day-of-month, time and year
//! sequence, which is what tells a separator from a body line that merely
//! starts with `From `. [`crate::scan`] relies on it for that.
//!
//! Refs: <https://manpages.debian.org/bookworm/mutt/mbox.5.en.html>

use alloc::{string::String, vec::Vec};

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// A parsed `From_` line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct MboxFromLine {
    /// Envelope sender, possibly empty.
    pub sender: String,
    /// Delivery date as seconds since the Unix epoch, shifted to UTC when
    /// the line carries a numeric timezone.
    pub timestamp: i64,
}

impl MboxFromLine {
    /// Parses `line`, with or without its line ending.
    ///
    /// Returns `None` when the line is not a `From_` line, which is how a
    /// separator is told from a body line starting with `From `.
    pub fn parse(line: &[u8]) -> Option<Self> {
        let rest = line.strip_prefix(b"From ")?;
        let rest = String::from_utf8_lossy(rest);
        let tokens: Vec<&str> = rest.split_ascii_whitespace().collect();

        for i in 0..tokens.len() {
            if let Some((timestamp, _)) = parse_date(&tokens[i..]) {
                let sender = tokens[..i].join(" ");
                return Some(Self { sender, timestamp });
            }
        }

        None
    }

    /// Formats the line, LF-terminated, for `sender` delivered at `secs`
    /// (UTC).
    ///
    /// An empty sender, or one containing whitespace, is replaced by
    /// `MAILER-DAEMON` so the line parses back to a single token.
    pub fn format(sender: &str, secs: i64) -> Vec<u8> {
        let sender = if sender.is_empty() || sender.contains(|c: char| c.is_ascii_whitespace()) {
            "MAILER-DAEMON"
        } else {
            sender
        };

        let days = secs.div_euclid(86_400);
        let rem = secs.rem_euclid(86_400);
        let (year, month, day) = civil_from_days(days);
        let weekday = DAYS[(days + 4).rem_euclid(7) as usize];
        let month = MONTHS[(month - 1) as usize];

        let line = format!(
            "From {sender} {weekday} {month} {day:>2} {:02}:{:02}:{:02} {year}\n",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60,
        );

        line.into_bytes()
    }
}

/// Parses `day month dd hh:mm[:ss] [tz] yyyy [tz]` at the head of
/// `tokens`, returning the UTC timestamp and the tokens consumed.
fn parse_date(tokens: &[&str]) -> Option<(i64, usize)> {
    let [day, month, mday, time, rest @ ..] = tokens else {
        return None;
    };

    DAYS.iter().find(|d| d.eq_ignore_ascii_case(day))?;
    let month = MONTHS.iter().position(|m| m.eq_ignore_ascii_case(month))? as i64 + 1;
    let mday: i64 = parse_digits(mday, 1, 2)?;
    if !(1..=31).contains(&mday) {
        return None;
    }

    let mut hms = time.split(':');
    let hours: i64 = parse_digits(hms.next()?, 1, 2)?;
    let minutes: i64 = parse_digits(hms.next()?, 2, 2)?;
    let seconds: i64 = match hms.next() {
        Some(s) => parse_digits(s, 2, 2)?,
        None => 0,
    };
    if hms.next().is_some() || hours > 23 || minutes > 59 || seconds > 60 {
        return None;
    }

    let mut year = None;
    let mut offset = 0;
    let mut consumed = 4;
    for token in rest.iter().take(3) {
        if year.is_none()
            && let Some(y) = parse_year(token)
        {
            year = Some(y);
            consumed += 1;
            continue;
        }
        if let Some(o) = parse_offset(token) {
            offset = o;
            consumed += 1;
            continue;
        }
        if token.chars().all(|c| c.is_ascii_alphabetic()) && token.len() <= 5 {
            consumed += 1;
            continue;
        }
        break;
    }

    let secs = days_from_civil(year?, month, mday) * 86_400 + hours * 3600 + minutes * 60 + seconds;
    Some((secs - offset, consumed))
}

fn parse_digits(s: &str, min: usize, max: usize) -> Option<i64> {
    if s.len() < min || s.len() > max || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse().ok()
}

fn parse_year(s: &str) -> Option<i64> {
    match s.len() {
        4 => parse_digits(s, 4, 4),
        2 => parse_digits(s, 2, 2).map(|y| if y >= 70 { 1900 + y } else { 2000 + y }),
        _ => None,
    }
}

fn parse_offset(s: &str) -> Option<i64> {
    let (sign, digits) = match s.as_bytes().first()? {
        b'+' => (1, &s[1..]),
        b'-' => (-1, &s[1..]),
        _ => return None,
    };
    let n = parse_digits(digits, 4, 4)?;
    Some(sign * (n / 100 * 3600 + n % 100 * 60))
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
///
/// Refs: <https://howardhinnant.github.io/date_algorithms.html#days_from_civil>
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Proleptic Gregorian date of a count of days since 1970-01-01.
///
/// Refs: <https://howardhinnant.github.io/date_algorithms.html#civil_from_days>
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

#[cfg(test)]
mod tests {
    use crate::from_line::MboxFromLine;

    fn parse(line: &str) -> Option<MboxFromLine> {
        MboxFromLine::parse(line.as_bytes())
    }

    #[test]
    fn real_world_lines_parse() {
        let cases = [
            (
                "From MAILER-DAEMON Thu Jan 18 21:41:44 2024\n",
                "MAILER-DAEMON",
                1_705_614_104,
            ),
            (
                "From dev-return-98476@httpd.apache.org  Tue Jan  2 13:47:51 2024\n",
                "dev-return-98476@httpd.apache.org",
                1_704_203_271,
            ),
            ("From mboxrd@z Thu Jan  1 00:00:00 1970\n", "mboxrd@z", 0),
            ("From - Mon Jan 01 00:00:00 2024\r\n", "-", 1_704_067_200),
            (
                "From 1780@xxx Mon Jan 01 01:00:00 +0100 2024\n",
                "1780@xxx",
                1_704_067_200,
            ),
            ("From a@b Mon Jan 1 00:00 CET 2024", "a@b", 1_704_067_200),
            ("From a@b Fri Jun 23 02:56:55 00", "a@b", 961_729_015),
            (
                "From a@b Mon Jan  1 00:00:00 -0100 2024",
                "a@b",
                1_704_070_800,
            ),
            (
                "From a@b Mon Jan  1 00:00:00 2024 remote from uunet",
                "a@b",
                1_704_067_200,
            ),
            (
                "From \"john doe\"@x Mon Jan  1 00:00:00 2024",
                "\"john doe\"@x",
                1_704_067_200,
            ),
            ("From  Mon Jan  1 00:00:00 2024", "", 1_704_067_200),
        ];

        for (line, sender, timestamp) in cases {
            let parsed = parse(line).unwrap_or_else(|| panic!("{line:?} should parse"));
            assert_eq!(parsed.sender, sender, "{line:?}");
            assert_eq!(parsed.timestamp, timestamp, "{line:?}");
        }
    }

    #[test]
    fn body_lines_do_not_parse() {
        for line in [
            "From the man page:\n",
            "From: someone@example.org\n",
            "From Monday on we meet at 10:00 in 2024\n",
            "From a@b Mon Jan 32 00:00:00 2024\n",
            "From a@b Mon Jan 1 25:00:00 2024\n",
            "From a@b Mon Jan 1 00:00:00\n",
            ">From a@b Mon Jan  1 00:00:00 2024\n",
            "From a@b Mon Foo  1 00:00:00 2024\n",
        ] {
            assert_eq!(parse(line), None, "{line:?}");
        }
    }

    #[test]
    fn format_round_trips() {
        for secs in [0, 1_704_067_200, 951_782_400, 4_102_444_799, -86_400] {
            let line = MboxFromLine::format("a@b", secs);
            let parsed = MboxFromLine::parse(&line).unwrap();
            assert_eq!(parsed.sender, "a@b");
            assert_eq!(parsed.timestamp, secs);
        }
        assert_eq!(
            MboxFromLine::format("a@b", 0),
            b"From a@b Thu Jan  1 00:00:00 1970\n".as_slice()
        );
    }

    #[test]
    fn format_replaces_unusable_senders() {
        let line = MboxFromLine::format("", 0);
        assert!(line.starts_with(b"From MAILER-DAEMON "));
        let line = MboxFromLine::format("a b", 0);
        assert!(line.starts_with(b"From MAILER-DAEMON "));
    }
}
