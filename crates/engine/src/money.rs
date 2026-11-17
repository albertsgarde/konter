//! Exact decimal amounts: parsing bank formats and formatting for hledger.

use std::fmt;
use std::str::FromStr;

use anyhow::{Context, Result, bail};
use rust_decimal::Decimal;
use serde::de::{self, Deserializer, Visitor};

/// Parses a Danish-formatted number: `.` as thousands separator, `,` as decimal
/// separator, e.g. `"-1.234,56"`.
pub fn parse_danish(s: &str) -> Result<Decimal> {
    let cleaned: String = s
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace() && *c != '.')
        .map(|c| match c {
            ',' => '.',
            '\u{2212}' => '-', // unicode minus
            c => c,
        })
        .collect();
    if cleaned.is_empty() {
        bail!("empty amount");
    }
    Decimal::from_str(&cleaned).with_context(|| format!("invalid amount {s:?}"))
}

/// Parses a plain number with `.` as decimal separator, e.g. `"-1234.56"`.
pub fn parse_plain(s: &str) -> Result<Decimal> {
    Decimal::from_str(s.trim()).with_context(|| format!("invalid amount {s:?}"))
}

/// Canonical text form used for hashing: no trailing zeros.
pub fn canonical(d: Decimal) -> String {
    d.normalize().to_string()
}

/// Formats an amount with at least two decimals, e.g. `-4320.00`.
pub fn fmt_num(d: Decimal) -> String {
    let d = d.normalize();
    if d.scale() <= 2 { format!("{:.2}", d) } else { d.to_string() }
}

/// Formats an amount with its commodity for hledger, e.g. `-4320.00 DKK`.
pub fn fmt_amount(d: Decimal, commodity: &str) -> String {
    format!("{} {}", fmt_num(d), commodity)
}

/// An amount in a config file: accepts TOML integers, floats and strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Amount(pub Decimal);

impl fmt::Display for Amount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", fmt_num(self.0))
    }
}

impl<'de> serde::Deserialize<'de> for Amount {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct V;
        impl Visitor<'_> for V {
            type Value = Amount;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("an amount (number or string)")
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> Result<Amount, E> {
                Ok(Amount(Decimal::from(v)))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Amount, E> {
                Ok(Amount(Decimal::from(v)))
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Amount, E> {
                // Rust prints the shortest representation that round-trips, so a
                // hand-written `1234.56` comes back exactly.
                Decimal::from_str(&v.to_string()).map(Amount).map_err(E::custom)
            }
            fn visit_str<E: de::Error>(self, v: &str) -> Result<Amount, E> {
                parse_plain(v).map(Amount).map_err(E::custom)
            }
        }
        deserializer.deserialize_any(V)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn danish() {
        assert_eq!(
            parse_danish("-1.234,56").expect("Danish notation parses"),
            Decimal::from_str("-1234.56").expect("a decimal")
        );
        assert_eq!(
            parse_danish("6500,00").expect("Danish notation parses"),
            Decimal::from_str("6500").expect("a decimal")
        );
        assert_eq!(
            parse_danish("-0,02").expect("Danish notation parses"),
            Decimal::from_str("-0.02").expect("a decimal")
        );
        assert!(parse_danish("").is_err());
        assert!(parse_danish("abc").is_err());
    }

    #[test]
    fn formatting() {
        assert_eq!(fmt_num(Decimal::from_str("-4320").expect("a decimal")), "-4320.00");
        assert_eq!(fmt_num(Decimal::from_str("12.5").expect("a decimal")), "12.50");
        assert_eq!(fmt_num(Decimal::from_str("0.125").expect("a decimal")), "0.125");
        assert_eq!(canonical(Decimal::from_str("4320.00").expect("a decimal")), "4320");
    }
}
