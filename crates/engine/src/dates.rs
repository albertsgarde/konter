//! Date helpers: config deserialization, periods, month arithmetic.

use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
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

/// How many days apart two dates are, whichever comes first.
pub fn days_between(a: Date, b: Date) -> i64 {
    (a.duration_since(b).as_secs() / 86_400).abs()
}

/// Adds months, clamping to the end of the month (Jan 31 + 1 month = Feb 28).
/// An error if that is past the last date there is.
pub fn add_months(date: Date, months: i32) -> Result<Date> {
    // jiff's month arithmetic already clamps to the last day of the month.
    date.checked_add(months.months()).map_err(|_| out_of_range(date, format_args!("{months} months")))
}

/// Dates end with year 9999. Schedules are stepped through from what the
/// rules say, so the rules can lead past it.
fn out_of_range(from: Date, step: fmt::Arguments) -> anyhow::Error {
    anyhow!("date out of range: {step} after {from} is past year 9999, the last there is")
}

const MAX_PERIOD_MONTHS: i32 = 100 * 12;
const MAX_PERIOD_DAYS: i32 = 100 * 366;

/// A repeating interval like "1 month" or "2 weeks", of at most 100 years.
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
        let period = match unit.trim_end_matches('s') {
            "day" => Some(Period::Days(n)),
            "week" => n.checked_mul(7).map(Period::Days),
            "month" => Some(Period::Months(n)),
            "year" => n.checked_mul(12).map(Period::Months),
            _ => bail!("invalid period unit in {s:?} (use days, weeks, months or years)"),
        };
        // Due dates are stepped through one period at a time, and dates end
        // at year 9999: a century is far more than any schedule needs.
        match period {
            Some(p @ Period::Days(d)) if d <= MAX_PERIOD_DAYS => Ok(p),
            Some(p @ Period::Months(m)) if m <= MAX_PERIOD_MONTHS => Ok(p),
            _ => bail!("period {s:?} is too long (at most 100 years)"),
        }
    }

    /// The `k`-th occurrence counted from `anchor` (k = 0 is the anchor itself).
    /// Computed from the anchor, so month-end dates don't drift. An error if
    /// it is past the last date there is.
    pub fn nth(&self, anchor: Date, k: i32) -> Result<Date> {
        match *self {
            Period::Days(n) => {
                let far = || out_of_range(anchor, format_args!("{k} times {n} days"));
                let days = n.checked_mul(k).ok_or_else(far)?;
                anchor.checked_add(days.days()).map_err(|_| far())
            }
            Period::Months(n) => {
                let months = n.checked_mul(k);
                let months = months.ok_or_else(|| out_of_range(anchor, format_args!("{k} times {n} months")))?;
                add_months(anchor, months)
            }
        }
    }

    /// Adds one period to a date.
    pub fn after(&self, date: Date) -> Result<Date> {
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
        assert_eq!(parse_ymd_slash("2026/03/05").expect("the date parses"), date(2026, 3, 5));
        assert_eq!(parse_dmy_dot("13.10.2026").expect("the date parses"), date(2026, 10, 13));
        assert!(parse_ymd_slash("Reserveret").is_err());
    }

    #[test]
    fn dates_past_the_last_one_are_errors() {
        let last = date(9999, 12, 31);
        assert_eq!(Period::Days(1).nth(date(9999, 12, 30), 1).ok(), Some(last));
        for (period, anchor, k) in [
            (Period::Days(1), last, 1),
            (Period::Months(1), date(9999, 12, 1), 1),
            // More days or months than a number holds.
            (Period::Days(36_600), date(2026, 1, 1), 100_000),
            (Period::Months(1_200), date(2026, 1, 1), 2_000_000),
        ] {
            let error = period.nth(anchor, k).expect_err("that's past year 9999").to_string();
            assert!(error.contains("date out of range") && error.contains(&anchor.to_string()), "{error}");
        }
        assert!(Period::Months(1).after(last).is_err());
        assert!(add_months(last, 1).is_err());
    }

    #[test]
    fn periods() {
        let p = Period::parse("1 month").expect("the period parses");
        assert_eq!(p.nth(date(2025, 1, 31), 1).ok(), Some(date(2025, 2, 28)));
        assert_eq!(p.nth(date(2025, 1, 31), 2).ok(), Some(date(2025, 3, 31)));
        assert_eq!(Period::parse("2 weeks").expect("the period parses"), Period::Days(14));
        assert_eq!(Period::parse("1 year").expect("the period parses"), Period::Months(12));
        assert!(Period::parse("monthly").is_err());
    }

    #[test]
    fn toml_dates() {
        #[derive(serde::Deserialize)]
        struct T {
            a: ConfigDate,
            b: ConfigDate,
        }
        let t: T = toml::from_str("a = 2025-01-01\nb = \"2025-02-03\"").expect("a date parses both ways");
        assert_eq!(t.a.0, date(2025, 1, 1));
        assert_eq!(t.b.0, date(2025, 2, 3));
    }

    #[test]
    fn a_period_is_at_most_a_hundred_years() {
        assert_eq!(Period::parse("100 years").expect("a century is fine"), Period::Months(1200));
        assert_eq!(Period::parse("36600 days").expect("a century of days is fine"), Period::Days(36_600));
        for long in ["101 years", "1201 months", "36601 days", "5229 weeks"] {
            let error = Period::parse(long).expect_err("that's more than a century").to_string();
            assert!(error.contains("is too long (at most 100 years)"), "{error}");
        }
    }

    #[test]
    fn something_else_where_a_date_goes_says_what_a_date_looks_like() {
        #[derive(Debug, serde::Deserialize)]
        struct Dated {
            #[expect(dead_code, reason = "only read to see it's refused")]
            first: ConfigDate,
        }
        let error = toml::from_str::<Dated>("first = 5").expect_err("5 is no date").to_string();
        assert!(error.contains("a date like 2025-01-01"), "{error}");
    }
}
