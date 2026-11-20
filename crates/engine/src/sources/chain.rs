//! Ordering a source's transactions so the running balance chains
//! (docs/spec.md §4): each one starts from the balance the one before it left.

use std::collections::BTreeMap;

use jiff::civil::Date;
use rust_decimal::Decimal;

use crate::findings::Findings;
use crate::model::BankTransaction;
use crate::money::fmt_num;

pub(super) fn short(t: &BankTransaction) -> String {
    format!("{} {:>10} {:?}", t.booked, fmt_num(t.amount), t.description)
}

/// Puts transactions that all have a running balance, sorted by day, in the
/// order the balance chains in, and reports where it doesn't. Where the
/// balance allows several orders of a day, they stay in the order given.
/// Returns them with whether the chain is unbroken.
pub(super) fn in_chain_order(
    name: &str,
    txs: Vec<BankTransaction>,
    findings: &mut Findings,
) -> (Vec<BankTransaction>, bool) {
    let (order, breaks) = chain(&txs);
    let mut txs: Vec<Option<BankTransaction>> = txs.into_iter().map(Some).collect();
    let ordered = order.into_iter().map(|i| txs[i].take().expect("the chain has every transaction once")).collect();
    let chain_ok = breaks.is_empty();
    if !chain_ok {
        findings
            .error("balance chain", format!("{name}: running balance breaks {} times", breaks.len()))
            .details(breaks);
    }
    (ordered, chain_ok)
}

/// The running balance after a transaction that is being chained.
fn after(t: &BankTransaction) -> Decimal {
    t.balance_after.expect("only transactions with a running balance are chained")
}

/// The running balance before a transaction that is being chained.
fn before(t: &BankTransaction) -> Decimal {
    after(t) - t.amount
}

/// `chain` makes a day only for a date that has a transaction, and an order
/// of a day has all of them.
const DAY_HAS_ONE: &str = "a day that is chained has a transaction";

/// Orders transactions (all with running balances) so each one starts from the
/// previous one's balance. Where the balance allows several orders, they
/// stay in the order given. Returns the order (indices) and descriptions of breaks.
fn chain(txs: &[BankTransaction]) -> (Vec<usize>, Vec<String>) {
    let mut days: BTreeMap<Date, Vec<usize>> = BTreeMap::new();
    for (i, t) in txs.iter().enumerate() {
        days.entry(t.booked).or_default().push(i);
    }
    let days: Vec<(Date, Vec<usize>)> = days.into_iter().collect();
    let mut order = Vec::with_capacity(txs.len());
    let mut breaks = Vec::new();
    let mut current: Option<(Date, Decimal)> = None;
    for (k, (day, idxs)) in days.iter().enumerate() {
        let day = *day;
        let day_order = match current.and_then(|(_, b)| chain_day(txs, idxs, Some(b), None)) {
            Some(o) => o,
            None => {
                let own = chain_day_unanchored(txs, idxs, days.get(k + 1).map(|(_, next)| next.as_slice()));
                match (&own, current) {
                    (Some(o), Some((prev_day, b))) => {
                        let starts = before(&txs[o[0]]);
                        breaks.push(format!(
                            "between {prev_day} and {day}: balance was {}, next transaction starts from {} \
                             (difference {}); a transaction is probably missing",
                            fmt_num(b),
                            fmt_num(starts),
                            fmt_num(starts - b)
                        ));
                    }
                    (Some(_), None) => {}
                    (None, _) => breaks.push(format!("on {day}: the day's transactions don't form a chain")),
                }
                own.unwrap_or_else(|| idxs.clone())
            }
        };
        current = Some((day, after(&txs[*day_order.last().expect(DAY_HAS_ONE)])));
        order.extend(day_order);
    }
    (order, breaks)
}

/// An order for a day with no balance before it to start from: the first day,
/// or the one after a break. Several orders can chain (a charge and its refund,
/// either way round), so if the first one found doesn't lead into the next day,
/// prefer one that ends where the next day starts.
fn chain_day_unanchored(txs: &[BankTransaction], idxs: &[usize], next: Option<&[usize]>) -> Option<Vec<usize>> {
    let found = chain_day(txs, idxs, None, None)?;
    let Some(next) = next else { return Some(found) };
    let leads_on = |end: Decimal| chain_day(txs, next, Some(end), None).is_some();
    if leads_on(after(&txs[*found.last().expect(DAY_HAS_ONE)])) {
        return Some(found);
    }
    let mut tried = Vec::new();
    for &j in next {
        let end = before(&txs[j]);
        if tried.contains(&end) {
            continue;
        }
        tried.push(end);
        if let Some(o) = chain_day(txs, idxs, None, Some(end))
            && leads_on(end)
        {
            return Some(o);
        }
    }
    Some(found)
}

/// Finds an order of one day's transactions where each starts from the previous
/// one's balance, starting from `start` (or from any transaction if `None`) and
/// ending on `end`, if given. Candidates are tried in the given order, so ties
/// keep it.
fn chain_day(
    txs: &[BankTransaction],
    idxs: &[usize],
    start: Option<Decimal>,
    end: Option<Decimal>,
) -> Option<Vec<usize>> {
    fn dfs(
        txs: &[BankTransaction],
        idxs: &[usize],
        current: Decimal,
        end: Option<Decimal>,
        used: &mut [bool],
        path: &mut Vec<usize>,
        budget: &mut u32,
    ) -> bool {
        if path.len() == idxs.len() {
            return end.is_none_or(|e| e == current);
        }
        if *budget == 0 {
            return false;
        }
        *budget -= 1;
        for (j, &i) in idxs.iter().enumerate() {
            if !used[j] && txs[i].balance_before() == Some(current) {
                used[j] = true;
                path.push(i);
                if dfs(txs, idxs, after(&txs[i]), end, used, path, budget) {
                    return true;
                }
                used[j] = false;
                path.pop();
            }
        }
        false
    }
    let starts: Vec<Decimal> = match start {
        Some(b) => vec![b],
        None => idxs.iter().map(|&i| before(&txs[i])).collect(),
    };
    for b in starts {
        let mut used = vec![false; idxs.len()];
        let mut path = Vec::new();
        let mut budget = 100_000;
        if dfs(txs, idxs, b, end, &mut used, &mut path, &mut budget) {
            return Some(path);
        }
    }
    None
}
