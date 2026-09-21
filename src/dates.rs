//! Best-effort publish-date parsing.
//!
//! The selector files point at whatever markup each site uses, so the raw value
//! can be an ISO timestamp, "18.09.2026 14:30", "18 сентября 2026" or
//! "3 hours ago". Everything naive is treated as UTC; that is accurate enough
//! for day-granularity date filtering and the raw string is always kept.

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, TimeZone, Utc};
use regex::Regex;

const DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%d %H:%M",
    "%Y/%m/%d %H:%M:%S",
    "%Y/%m/%d %H:%M",
    "%d.%m.%Y %H:%M:%S",
    "%d.%m.%Y %H:%M",
    "%d/%m/%Y %H:%M:%S",
    "%d/%m/%Y %H:%M",
    "%d-%m-%Y %H:%M",
    "%d %B %Y %H:%M",
    "%d %b %Y %H:%M",
    "%B %d, %Y %H:%M",
    "%b %d, %Y %H:%M",
    "%B %d %Y %H:%M",
];

const DATE_FORMATS: &[&str] = &[
    "%Y-%m-%d",
    "%Y/%m/%d",
    "%d.%m.%Y",
    "%d/%m/%Y",
    "%d-%m-%Y",
    "%d %B %Y",
    "%d %b %Y",
    "%B %d, %Y",
    "%b %d, %Y",
    "%B %d %Y",
];

/// Genitive Russian month names as they appear in datelines, longest first so
/// that e.g. "мая" does not shadow a longer match.
const RU_MONTHS: &[(&str, &str)] = &[
    ("января", "January"),
    ("февраля", "February"),
    ("марта", "March"),
    ("апреля", "April"),
    ("мая", "May"),
    ("июня", "June"),
    ("июля", "July"),
    ("августа", "August"),
    ("сентября", "September"),
    ("октября", "October"),
    ("ноября", "November"),
    ("декабря", "December"),
    ("январь", "January"),
    ("февраль", "February"),
    ("март", "March"),
    ("апрель", "April"),
    ("май", "May"),
    ("июнь", "June"),
    ("июль", "July"),
    ("август", "August"),
    ("сентябрь", "September"),
    ("октябрь", "October"),
    ("ноябрь", "November"),
    ("декабрь", "December"),
];

pub fn parse_date(raw: &str) -> Option<DateTime<Utc>> {
    let cleaned = clean(raw);
    if cleaned.is_empty() {
        return None;
    }

    if let Some(dt) = try_all(&cleaned) {
        return Some(dt);
    }

    // Retry with Russian month names swapped for English ones.
    let translated = translate_months(&cleaned);
    if translated != cleaned {
        if let Some(dt) = try_all(&translated) {
            return Some(dt);
        }
    }

    relative(&cleaned)
}

fn try_all(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    if let Ok(dt) = DateTime::parse_from_rfc2822(s) {
        return Some(dt.with_timezone(&Utc));
    }
    // "2026-09-18T14:30:00+0300" (no colon in the offset)
    for fmt in ["%Y-%m-%dT%H:%M:%S%z", "%Y-%m-%d %H:%M:%S%z"] {
        if let Ok(dt) = DateTime::parse_from_str(s, fmt) {
            return Some(dt.with_timezone(&Utc));
        }
    }
    for fmt in DATETIME_FORMATS {
        if let Ok(ndt) = NaiveDateTime::parse_from_str(s, fmt) {
            return Some(Utc.from_utc_datetime(&ndt));
        }
    }
    for fmt in DATE_FORMATS {
        if let Ok(nd) = NaiveDate::parse_from_str(s, fmt) {
            return nd
                .and_hms_opt(0, 0, 0)
                .map(|ndt| Utc.from_utc_datetime(&ndt));
        }
    }
    embedded(s)
}

/// Pull an ISO-ish date (optionally with a time) out of a longer string such as
/// "Published: 2026-09-18 14:30 | Section: World".
fn embedded(s: &str) -> Option<DateTime<Utc>> {
    let iso =
        Regex::new(r"(\d{4})[-/](\d{1,2})[-/](\d{1,2})(?:[T ](\d{1,2}):(\d{2})(?::(\d{2}))?)?")
            .ok()?;
    if let Some(c) = iso.captures(s) {
        return build(
            num(&c, 1)?,
            num(&c, 2)?,
            num(&c, 3)?,
            num(&c, 4).unwrap_or(0),
            num(&c, 5).unwrap_or(0),
            num(&c, 6).unwrap_or(0),
        );
    }

    let dmy =
        Regex::new(r"(\d{1,2})[.\-/](\d{1,2})[.\-/](\d{4})(?:[ ,]+(\d{1,2}):(\d{2}))?").ok()?;
    if let Some(c) = dmy.captures(s) {
        return build(
            num(&c, 3)?,
            num(&c, 2)?,
            num(&c, 1)?,
            num(&c, 4).unwrap_or(0),
            num(&c, 5).unwrap_or(0),
            0,
        );
    }

    // "18 September 2026 14:30" / "September 18, 2026"
    let named =
        Regex::new(r"(?i)(\d{1,2})\s+([a-z]{3,9})\s+(\d{4})|([a-z]{3,9})\s+(\d{1,2}),?\s+(\d{4})")
            .ok()?;
    if let Some(c) = named.captures(s) {
        let (d, m, y) = if c.get(1).is_some() {
            (num(&c, 1)?, month_number(c.get(2)?.as_str())?, num(&c, 3)?)
        } else {
            (num(&c, 5)?, month_number(c.get(4)?.as_str())?, num(&c, 6)?)
        };
        let time = Regex::new(r"(\d{1,2}):(\d{2})").ok()?;
        let (hh, mm) = match time.captures(s) {
            Some(t) => (num(&t, 1).unwrap_or(0), num(&t, 2).unwrap_or(0)),
            None => (0, 0),
        };
        return build(y, m, d, hh, mm, 0);
    }

    None
}

fn num(c: &regex::Captures, i: usize) -> Option<i64> {
    c.get(i).and_then(|m| m.as_str().parse::<i64>().ok())
}

fn build(y: i64, mo: i64, d: i64, h: i64, mi: i64, s: i64) -> Option<DateTime<Utc>> {
    let date = NaiveDate::from_ymd_opt(y as i32, mo as u32, d as u32)?;
    let ndt = date.and_hms_opt(h as u32, mi as u32, s as u32)?;
    Some(Utc.from_utc_datetime(&ndt))
}

fn month_number(name: &str) -> Option<i64> {
    let n = name.to_ascii_lowercase();
    let months = [
        "january",
        "february",
        "march",
        "april",
        "may",
        "june",
        "july",
        "august",
        "september",
        "october",
        "november",
        "december",
    ];
    months
        .iter()
        .position(|m| m.starts_with(&n[..n.len().min(3)]) && n.len() >= 3)
        .map(|i| i as i64 + 1)
}

fn relative(s: &str) -> Option<DateTime<Utc>> {
    let re =
        Regex::new(r"(?i)(\d+)\s*(second|minute|min|hour|day|week|month|year)s?\s*ago").ok()?;
    let c = re.captures(s)?;
    let n = c.get(1)?.as_str().parse::<i64>().ok()?;
    let unit = c.get(2)?.as_str().to_ascii_lowercase();
    let delta = match unit.as_str() {
        "second" => Duration::seconds(n),
        "minute" | "min" => Duration::minutes(n),
        "hour" => Duration::hours(n),
        "day" => Duration::days(n),
        "week" => Duration::weeks(n),
        "month" => Duration::days(n * 30),
        "year" => Duration::days(n * 365),
        _ => return None,
    };
    Some(Utc::now() - delta)
}

fn clean(raw: &str) -> String {
    let collapsed: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed
        .trim()
        .trim_matches(|c: char| c == '|' || c == '·' || c == '•')
        .trim()
        .to_string()
}

fn translate_months(s: &str) -> String {
    let lower = s.to_lowercase();
    let mut out = lower;
    for (ru, en) in RU_MONTHS {
        if out.contains(ru) {
            out = out.replace(ru, en);
            break;
        }
    }
    // chrono's %B wants "September", not "september".
    out.split(' ')
        .map(|w| {
            let mut ch = w.chars();
            match ch.next() {
                Some(f) if f.is_alphabetic() => f.to_uppercase().collect::<String>() + ch.as_str(),
                _ => w.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ymd(dt: DateTime<Utc>) -> (i32, u32, u32) {
        use chrono::Datelike;
        (dt.year(), dt.month(), dt.day())
    }

    #[test]
    fn formats() {
        assert_eq!(
            ymd(parse_date("2026-09-18T14:30:00+03:00").unwrap()),
            (2026, 9, 18)
        );
        assert_eq!(ymd(parse_date("2026-09-18 14:30").unwrap()), (2026, 9, 18));
        assert_eq!(ymd(parse_date("18.09.2026 14:30").unwrap()), (2026, 9, 18));
        assert_eq!(
            ymd(parse_date("September 18, 2026").unwrap()),
            (2026, 9, 18)
        );
        assert_eq!(ymd(parse_date("18 September 2026").unwrap()), (2026, 9, 18));
        assert_eq!(ymd(parse_date("18 сентября 2026").unwrap()), (2026, 9, 18));
        assert_eq!(
            ymd(parse_date("Опубликовано 18.09.2026 в 14:30").unwrap()),
            (2026, 9, 18)
        );
        assert!(parse_date("").is_none());
        assert!(parse_date("no date here").is_none());
    }
}
