//! Exact decimal amounts: parsing bank formats and formatting for hledger.

use std::fmt;

use anyhow::{Context, Result, anyhow, bail};
use rust_decimal::Decimal;
use serde::de::{self, Deserializer, Visitor};

/// How many digits an amount read from a file can have before the decimal
/// point.
pub const MAX_DIGITS: u32 = 15;
/// How many decimals an amount read from a file can have.
pub const MAX_DECIMALS: u32 = 8;

/// The amount, if it's one the books can hold: every amount that comes in from
/// a file (bank data, rules, hand-written transactions) goes through here.
///
/// A `Decimal` has 28 digits. A sum that needs more is rounded, and one past
/// 7.9 × 10²⁸ panics. Held to `MAX_DIGITS` and `MAX_DECIMALS`, an amount takes
/// 23 of them, so sums stay exact, and far from panicking, until hundreds of
/// thousands of the largest amounts there are have been added up.
pub fn in_range(d: Decimal) -> Result<Decimal> {
    if d.abs() >= Decimal::from(10_u64.pow(MAX_DIGITS)) {
        return Err(too_many_digits(d));
    }
    if d.normalize().scale() > MAX_DECIMALS {
        return Err(too_many_decimals(d));
    }
    Ok(d)
}

fn too_many_digits(amount: impl fmt::Display) -> anyhow::Error {
    anyhow!("amount out of range: {amount} has more than {MAX_DIGITS} digits before the decimal point")
}

fn too_many_decimals(amount: impl fmt::Display) -> anyhow::Error {
    anyhow!("amount out of range: {amount} has more than {MAX_DECIMALS} decimals")
}

/// Reads a number written with `.` as the decimal separator, exactly: one with
/// more digits than a `Decimal` holds is refused, not rounded. `written` is
/// how the file has it.
fn exact(plain: &str, written: &str) -> Result<Decimal> {
    // Counted in the text first: past what a `Decimal` holds, the error from
    // reading it doesn't say which end has too many.
    let unsigned = plain.trim_start_matches(['-', '+']);
    let (whole, decimals) = unsigned.split_once('.').unwrap_or((unsigned, ""));
    let digits = |s: &str| s.bytes().filter(u8::is_ascii_digit).count();
    if digits(whole.trim_start_matches('0')) > MAX_DIGITS as usize {
        return Err(too_many_digits(written.trim()));
    }
    if digits(decimals.trim_end_matches('0')) > MAX_DECIMALS as usize {
        return Err(too_many_decimals(written.trim()));
    }
    let d = Decimal::from_str_exact(plain).with_context(|| format!("invalid amount {written:?}"))?;
    in_range(d)
}

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
    exact(&cleaned, s)
}

/// Parses a plain number with `.` as decimal separator, e.g. `"-1234.56"`.
pub fn parse_plain(s: &str) -> Result<Decimal> {
    exact(s.trim(), s)
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
                in_range(Decimal::from(v)).map(Amount).map_err(E::custom)
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> Result<Amount, E> {
                in_range(Decimal::from(v)).map(Amount).map_err(E::custom)
            }
            fn visit_f64<E: de::Error>(self, v: f64) -> Result<Amount, E> {
                // Rust prints the shortest representation that round-trips, so a
                // hand-written `1234.56` comes back exactly.
                parse_plain(&v.to_string()).map(Amount).map_err(E::custom)
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
    use std::str::FromStr;

    use super::*;

    #[derive(serde::Deserialize)]
    struct Priced {
        amount: Amount,
    }

    #[test]
    fn amounts_are_held_to_15_digits_and_8_decimals() {
        let largest = "999999999999999.99999999";
        assert_eq!(parse_plain(largest).expect("the largest amount parses").to_string(), largest);
        assert_eq!(
            parse_plain("-999999999999999.99999999").expect("the smallest parses"),
            -parse_plain(largest).expect("parsed above")
        );
        // Zeros at the end aren't decimals to keep.
        assert_eq!(parse_plain("12.500000000000").expect("zeros at the end are fine"), Decimal::new(125, 1));
        assert_eq!(
            parse_danish("999.999.999.999.999,99").expect("Danish notation parses"),
            Decimal::from_str("999999999999999.99").expect("a decimal")
        );

        let digits = "has more than 15 digits before the decimal point";
        let decimals = "has more than 8 decimals";
        for (written, expected) in [
            ("1000000000000000", digits),
            ("-1000000000000000.00", digits),
            ("-0.000000001", decimals),
            // Past what a Decimal holds, where reading it would round: this is not 1.
            ("70000000000000000000000000000000", digits),
            ("1.00000000000000000000000000000000001", decimals),
        ] {
            let error = parse_plain(written).expect_err("the amount is out of range").to_string();
            assert_eq!(error, format!("amount out of range: {written} {expected}"));
            let written = written.replace('.', ",");
            let error = parse_danish(&written).expect_err("the amount is out of range").to_string();
            assert_eq!(error, format!("amount out of range: {written} {expected}"));
        }

        // In a config file, as a number or as text.
        for written in
            ["1000000000000000", "1000000000000000.0", "1e15", "\"1000000000000000\"", "0.000000001", "1e300"]
        {
            let Err(error) = toml::from_str::<Priced>(&format!("amount = {written}")) else {
                panic!("{written} is taken for an amount");
            };
            assert!(error.to_string().contains("amount out of range"), "{written}: {error}");
        }
        let ok: Priced = toml::from_str("amount = 999999999999999").expect("the largest whole amount is fine");
        assert_eq!(ok.amount.0, Decimal::from(999_999_999_999_999_u64));
    }

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
