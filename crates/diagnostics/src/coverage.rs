//! `konter coverage`: per source and month, how much is mapped.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use rust_decimal::Decimal;

use konter_engine::engine::Status;
use konter_engine::pipeline::Build;

/// Per month, the transactions and how many of them, and how much of the
/// amount, rules or pins map. Only `source`, if given.
pub fn coverage(build: &Build, source: Option<&str>) -> String {
    let mut out = String::new();
    for (name, data) in &build.loaded.sources {
        if source.is_some_and(|s| s != name) || data.txs.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{name}\n  {:<8} {:>5} {:>7} {:>14}", "month", "txs", "mapped", "% of amount");
        let mut months: BTreeMap<String, (usize, usize, Decimal, Decimal)> = BTreeMap::new();
        for t in &data.txs {
            let m = months.entry(t.booked.strftime("%Y-%m").to_string()).or_default();
            let mapped = matches!(build.generated.outcomes[&t.id].status, Status::Rule(_) | Status::Pin);
            m.0 += 1;
            m.2 += t.amount.abs();
            if mapped {
                m.1 += 1;
                m.3 += t.amount.abs();
            }
        }
        for (month, (n, mapped, total, mapped_amount)) in months {
            let pct = if total.is_zero() {
                Decimal::ONE_HUNDRED
            } else {
                (mapped_amount / total * Decimal::ONE_HUNDRED).round()
            };
            let _ = writeln!(out, "  {month:<8} {n:>5} {mapped:>7} {pct:>13}%");
        }
        let _ = writeln!(out);
    }
    out
}
