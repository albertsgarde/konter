//! Pairing the two sides of internal transfers (docs/spec.md §6).

use jiff::civil::Date;
use rust_decimal::Decimal;

/// One side of a transfer between two imported sources.
#[derive(Clone, Debug)]
pub struct Leg {
    /// Index of the ledger transaction this leg produced.
    pub ledger_idx: usize,
    pub from: String,
    pub to: String,
    pub date: Date,
    pub amount: Decimal,
    pub id: String,
}

pub fn days_between(a: Date, b: Date) -> i64 {
    (a.duration_since(b).as_secs() / 86_400).abs()
}

/// Pairs legs going opposite ways between the same two sources, with opposite
/// amounts and dates at most `window` days apart. Closest dates win; ties go to
/// the earlier date, then the lower ID. Returns pairs and unpaired legs (indices).
pub fn pair(legs: &[Leg], window: i64) -> (Vec<(usize, usize)>, Vec<usize>) {
    let mut forward: Vec<usize> = (0..legs.len()).filter(|&i| legs[i].from < legs[i].to).collect();
    forward.sort_by(|&a, &b| (legs[a].date, &legs[a].id).cmp(&(legs[b].date, &legs[b].id)));
    let mut used = vec![false; legs.len()];
    let mut pairs = Vec::new();
    for i in forward {
        let a = &legs[i];
        let best = (0..legs.len())
            .filter(|&j| {
                let b = &legs[j];
                !used[j]
                    && b.from == a.to
                    && b.to == a.from
                    && b.amount == -a.amount
                    && days_between(a.date, b.date) <= window
            })
            .min_by(|&x, &y| {
                let key = |j: usize| (days_between(a.date, legs[j].date), legs[j].date, legs[j].id.clone());
                key(x).cmp(&key(y))
            });
        if let Some(j) = best {
            used[i] = true;
            used[j] = true;
            pairs.push((i, j));
        }
    }
    let unpaired = (0..legs.len()).filter(|&i| !used[i]).collect();
    (pairs, unpaired)
}

#[cfg(test)]
mod tests {
    use super::*;
    use jiff::civil::date;
    use std::str::FromStr;

    fn leg(from: &str, to: &str, d: Date, amount: &str, id: &str) -> Leg {
        Leg {
            ledger_idx: 0,
            from: from.into(),
            to: to.into(),
            date: d,
            amount: Decimal::from_str(amount).unwrap(),
            id: id.into(),
        }
    }

    #[test]
    fn pairs_closest_opposite_legs() {
        let legs = vec![
            leg("a", "b", date(2025, 12, 5), "-1000", "a1"),
            leg("a", "b", date(2025, 12, 5), "-1000", "a2"),
            leg("b", "a", date(2025, 12, 8), "1000", "b1"),
            leg("b", "a", date(2025, 12, 8), "1000", "b2"),
            leg("b", "a", date(2026, 10, 11), "1000", "b3"), // before its counterpart
            leg("a", "b", date(2026, 10, 12), "-1000", "a3"),
            leg("b", "a", date(2025, 10, 17), "4200", "b4"), // no counterpart
            leg("a", "b", date(2025, 1, 1), "-50", "a4"),
            leg("b", "a", date(2025, 1, 20), "50", "b5"), // too far apart
        ];
        let (pairs, unpaired) = pair(&legs, 5);
        let ids: Vec<(&str, &str)> = pairs.iter().map(|&(i, j)| (legs[i].id.as_str(), legs[j].id.as_str())).collect();
        assert_eq!(ids, [("a1", "b1"), ("a2", "b2"), ("a3", "b3")]);
        let left: Vec<&str> = unpaired.iter().map(|&i| legs[i].id.as_str()).collect();
        assert_eq!(left, ["b4", "a4", "b5"]);
    }
}
