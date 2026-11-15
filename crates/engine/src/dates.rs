//! Date helpers: config deserialization, periods, month arithmetic.

use std::fmt;

use anyhow::{Context, Result, bail};
use jiff::ToSpan;
use jiff::civil::Date;
use serde::de::{self, Deserializer, MapAccess, Visitor};

/// A date in a config file: accepts TOML dates (`2025-01-01`) and strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigDate(pub Date);

impl<'de> serde::Deserialize<'de> for ConfigDate {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = ConfigDate;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a date like 2025-01-01")
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<ConfigDate, E> {
                v.parse::<Date>().map(ConfigDate).map_err(E::custom)
            }
            // The toml crate hands datetimes over as a single-entry map.
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<ConfigDate, A::Error> {
                let Some((_, value)) = map.next_entry::<String, String>()? else {
                    return Err(de::Error::custom("expected a date"));
                };
                value.parse::<Date>().map(ConfigDate).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_any(V)
    }
}

/// Parses `YYYY/MM/DD` (Nordea).
pub fn parse_ymd_slash(s: &str) -> Result<Date> {
    let parts: Vec<&str> = s.trim().split('/').collect();
    if parts.len() != 3 {
        bail!("invalid date {s:?}, expected YYYY/MM/DD");
    }
    ymd(parts[0], parts[1], parts[2]).with_context(|| format!("invalid date {s:?}"))
}

/// Parses `DD.MM.YYYY` (Lunar).
pub fn parse_dmy_dot(s: &str) -> Result<Date> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.len() != 3 {
        bail!("invalid date {s:?}, expected DD.MM.YYYY");
    }
    ymd(parts[2], parts[1], parts[0]).with_context(|| format!("invalid date {s:?}"))
}

fn ymd(y: &str, m: &str, d: &str) -> Result<Date> {
    Ok(Date::new(y.parse()?, m.parse()?, d.parse()?)?)
}

/// Adds months, clamping to the end of the month (Jan 31 + 1 month = Feb 28).
pub fn add_months(date: Date, months: i32) -> Date {
    // jiff's month arithmetic already clamps to the last day of the month.
    date.checked_add(months.months()).expect("date out of range")
}

/// A repeating interval like "1 month" or "2 weeks".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Period {
    Days(i32),
    Months(i32),
}

impl Period {
    pub fn parse(s: &str) -> Result<Period> {
        let mut it = s.split_whitespace();
        let (Some(n), Some(unit), None) = (it.next(), it.next(), it.next()) else {
            bail!("invalid period {s:?}, expected e.g. \"1 month\"");
        };
        let n: i32 = n.parse().with_context(|| format!("invalid period {s:?}"))?;
        if n <= 0 {
            bail!("period must be positive: {s:?}");
        }
        Ok(match unit.trim_end_matches('s') {
            "day" => Period::Days(n),
            "week" => Period::Days(7 * n),
            "month" => Period::Months(n),
            "year" => Period::Months(12 * n),
            _ => bail!("invalid period unit in {s:?} (use days, weeks, months or years)"),
        })
    }

    /// The `k`-th occurrence counted from `anchor` (k = 0 is the anchor itself).
    /// Computed from the anchor, so month-end dates don't drift.
    pub fn nth(&self, anchor: Date, k: i32) -> Date {
        match *self {
            Period::Days(n) => anchor.checked_add((n * k).days()).expect("date out of range"),
            Period::Months(n) => add_months(anchor, n * k),
        }
    }

    /// Adds one period to a date.
    pub fn after(&self, date: Date) -> Date {
        self.nth(date, 1)
    }
}

impl<'de> serde::Deserialize<'de> for Period {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Period::parse(&s).map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;

    #[test]
    fn parsing() {
        assert_eq!(parse_ymd_slash("2026/03/05").unwrap(), date(2026, 3, 5));
        assert_eq!(parse_dmy_dot("13.10.2026").unwrap(), date(2026, 10, 13));
        assert!(parse_ymd_slash("Reserveret").is_err());
    }

    #[test]
    fn periods() {
        let p = Period::parse("1 month").unwrap();
        assert_eq!(p.nth(date(2025, 1, 31), 1), date(2025, 2, 28));
        assert_eq!(p.nth(date(2025, 1, 31), 2), date(2025, 3, 31));
        assert_eq!(Period::parse("2 weeks").unwrap(), Period::Days(14));
        assert_eq!(Period::parse("1 year").unwrap(), Period::Months(12));
        assert!(Period::parse("monthly").is_err());
    }

    #[test]
    fn toml_dates() {
        #[derive(serde::Deserialize)]
        struct T {
            a: ConfigDate,
            b: ConfigDate,
        }
        let t: T = toml::from_str("a = 2025-01-01\nb = \"2025-02-03\"").unwrap();
        assert_eq!(t.a.0, date(2025, 1, 1));
        assert_eq!(t.b.0, date(2025, 2, 3));
    }
}
