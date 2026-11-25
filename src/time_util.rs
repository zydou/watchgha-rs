//! Time helpers: ISO parsing, human-friendly formatting, natural sort keys,
//! and the datetime bucketer used to group runs into events.

use jiff::tz::TimeZone;
use jiff::{Timestamp, ZonedDateTime};

use crate::error::WatchGhaError;

/// Parse an ISO-8601 timestamp from the GitHub API.
///
/// Mirrors `utils.to_datetime`: parse an RFC-3339 instant. GitHub emits e.g.
/// `2026-01-18T16:55:30Z`, which jiff parses directly (the original replaced a
/// trailing `Z` with `+00:00` only for older Python parsers).
pub fn to_datetime(isostr: &str) -> Result<Timestamp, WatchGhaError> {
    // An explicit offset / `Z`: parse as a Timestamp.
    if let Ok(ts) = isostr.parse::<Timestamp>() {
        return Ok(ts);
    }
    // Fall back to a naive civil datetime (no offset), treated as UTC.
    isostr
        .parse::<jiff::civil::DateTime>()
        .ok()
        .and_then(|ndt| ndt.to_zoned_datetime(TimeZone::UTC).ok())
        .map(|z| z.timestamp())
        .ok_or_else(|| WatchGhaError(format!("Couldn't parse datetime {isostr:?}")))
}

/// Format a timestamp the way the original's `utils.nice_time` does, in the
/// system-local timezone:
/// - different day, different year: `%Y-%m-%d %I:%M%p`
/// - different day, same year:      `%m-%d %I:%M%p`
/// - same day:                      `%I:%M%p`
///
/// The result is lowercased (so `AM`/`PM` become `am`/`pm`).
pub fn nice_time(dt: Timestamp) -> String {
    let local: ZonedDateTime = dt.to_zoned_datetime(TimeZone::system());
    let now = ZonedDateTime::now();
    let fmt = if local.date() != now.date() {
        if local.year() != now.year() {
            "%Y-%m-%d %I:%M%p"
        } else {
            "%m-%d %I:%M%p"
        }
    } else {
        "%I:%M%p"
    };
    local.strftime(fmt).to_string().to_lowercase()
}

/// A natural-sort key that orders strings the way humans expect.
///
/// Mirrors `utils.human_key`: split on runs of digits, convert digit runs to
/// integers, and append the original string as a tiebreaker so `"x1y"` and
/// `"x001y"` stay distinct. Numeric chunks compare numerically; the split is
/// done on the casefolded string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HumanKey {
    parts: Vec<KeyPart>,
    original: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum KeyPart {
    Text(String),
    Num(u64),
}

impl KeyPart {
    fn tryint(s: &str) -> KeyPart {
        match s.parse::<u64>() {
            Ok(n) => KeyPart::Num(n),
            Err(_) => KeyPart::Text(s.to_string()),
        }
    }
}

impl Ord for KeyPart {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Python compares int < str by type name in a sort key list? No — the
        // original builds a list mixing ints and strings and compares
        // element-wise; matching positions always have the same type because
        // `re.split` with a capturing group alternates non-digit/digit. So we
        // compare like with like, falling back to textual ordering.
        match (self, other) {
            (KeyPart::Num(a), KeyPart::Num(b)) => a.cmp(b),
            (KeyPart::Text(a), KeyPart::Text(b)) => a.cmp(b),
            // Cross-type shouldn't occur at the same index; order nums first.
            (KeyPart::Num(_), KeyPart::Text(_)) => std::cmp::Ordering::Less,
            (KeyPart::Text(_), KeyPart::Num(_)) => std::cmp::Ordering::Greater,
        }
    }
}

impl PartialOrd for KeyPart {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HumanKey {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.parts
            .cmp(&other.parts)
            .then_with(|| self.original.cmp(&other.original))
    }
}

impl PartialOrd for HumanKey {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// Split on runs of digits, keeping the delimiters, mirroring
/// `re.split(r"(\d+)", s.casefold())`.
pub fn human_key(s: &str) -> HumanKey {
    let folded = s.to_lowercase();
    let mut parts = Vec::new();
    let mut chars = folded.as_str();
    while !chars.is_empty() {
        let digit_end = chars
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(chars.len());
        if digit_end > 0 {
            parts.push(KeyPart::tryint(&chars[..digit_end]));
            chars = &chars[digit_end..];
        }
        if chars.is_empty() {
            break;
        }
        let text_end = chars
            .find(|c: char| c.is_ascii_digit())
            .unwrap_or(chars.len());
        parts.push(KeyPart::Text(chars[..text_end].to_string()));
        chars = &chars[text_end..];
    }
    HumanKey {
        parts,
        original: s.to_string(),
    }
}

/// Round like Python 3's built-in `round()` for floats: half-to-even
/// (banker's rounding). Rust's `f64::round()` rounds half away from zero,
/// which diverges on exact `.5` boundaries — and the bucketer's rounding hits
/// those boundaries regularly, so we replicate Python's behavior.
fn py_round(x: f64) -> i64 {
    let floor = x.floor();
    let frac = x - floor;
    let n = floor as i64;
    match frac.partial_cmp(&0.5) {
        Some(std::cmp::Ordering::Greater) => n + 1,
        Some(std::cmp::Ordering::Less) => n,
        // Exact .5: round half to even.
        _ => {
            if n % 2 == 0 {
                n
            } else {
                n + 1
            }
        }
    }
}

/// Fuzz timestamps into canonical instants so runs started close together
/// group into the same event.
///
/// Mirrors `bucketer.DatetimeBucketer` with a 5-second window (see
/// `data_core.bucketer`). A datetime maps to the first known "good" instant it
/// rounds to under either jitter (0 or window/2); if none match, it becomes a
/// new good instant.
#[derive(Default)]
pub struct DatetimeBucketer {
    window: i64,
    instants: Vec<Timestamp>,
    rounds: std::collections::HashMap<i64, Timestamp>,
}

impl DatetimeBucketer {
    pub fn new(window: i64) -> Self {
        Self {
            window,
            instants: Vec::new(),
            rounds: std::collections::HashMap::new(),
        }
    }

    fn roundings(&self, dt: Timestamp) -> Vec<i64> {
        let ts = dt.as_second();
        [0, self.window / 2]
            .iter()
            .map(|jitter| py_round((ts + jitter) as f64 / self.window as f64))
            .collect()
    }

    pub fn defuzz(&mut self, dt: Timestamp) -> Timestamp {
        if self.instants.contains(&dt) {
            return dt;
        }
        for rounded in self.roundings(dt) {
            if let Some(instant) = self.rounds.get(&rounded) {
                return *instant;
            }
        }
        self.instants.push(dt);
        for rounded in self.roundings(dt) {
            self.rounds.insert(rounded, dt);
        }
        dt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_zulu() {
        let dt = to_datetime("2026-01-18T16:55:30Z").unwrap();
        assert_eq!(dt.as_second(), 1768755330);
    }

    #[test]
    fn human_key_splits_digits() {
        // "z23A" -> numeric chunk 23 between text.
        let a = human_key("z23A");
        let b = human_key("z9B");
        // 9 < 23, so b sorts before a on the numeric chunk.
        assert!(b < a);
    }

    #[test]
    fn human_key_tiebreak_original() {
        // Same numeric value (1 == 001), so the original string breaks the tie.
        // "x001y" < "x1y" lexicographically, matching Python tuple comparison.
        assert!(human_key("x001y") < human_key("x1y"));
    }

    #[test]
    fn bucketer_groups_close_times() {
        let mut b = DatetimeBucketer::new(5);
        let t0 = Timestamp::from_second(1000).unwrap();
        let t1 = Timestamp::from_second(1002).unwrap();
        let d0 = b.defuzz(t0);
        let d1 = b.defuzz(t1);
        assert_eq!(d0, d1, "times 2s apart should bucket together");
    }

    #[test]
    fn python_round_is_half_to_even() {
        assert_eq!(py_round(200.5), 200);
        assert_eq!(py_round(201.5), 202);
        assert_eq!(py_round(200.4), 200);
        assert_eq!(py_round(200.6), 201);
    }
}
