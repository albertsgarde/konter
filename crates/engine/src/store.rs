//! The store of transactions: our own record of every bank transaction, in
//! one format for all banks, kept as a plain table in a text file.
//!
//! A bank's files show the same transaction differently from one format to
//! the next, and can't always tell two transactions apart, so an ID derived
//! from them can change when more data arrives. Here a transaction gets its
//! ID once, when it's first read, and keeps it. Reading a file reconciles its
//! rows with the table ([`Table::read`]): a row that is a transaction the
//! table has fills in what that one lacks, and any other row is added. What
//! the table has is never overwritten: a copy that says something else fails
//! the read.
//!
//! Nothing builds from the table yet: the books still come from merging the
//! raw files (`sources::merge`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use anyhow::{Context, Result, bail};
use jiff::civil::{Date, Time};
use rust_decimal::Decimal;

use crate::model::{BankTransaction, Origin};
use crate::money::{canonical, parse_plain};

/// A transaction's ID in the store: a UUID, given when its row is added and
/// never derived from the row again.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TransactionId(String);

impl TransactionId {
    /// A random (version 4) UUID from 16 random bytes. The engine has no
    /// source of randomness of its own, so the caller brings the bytes.
    pub fn from_random(mut bytes: [u8; 16]) -> TransactionId {
        // The version, 4, is the high half of byte 6, and the variant the two
        // high bits of byte 8.
        bytes[6] = bytes[6] % 0x10 + 0x40;
        bytes[8] = bytes[8] % 0x40 + 0x80;
        let mut text = String::with_capacity(36);
        for (i, byte) in bytes.iter().enumerate() {
            if [4, 6, 8, 10].contains(&i) {
                text.push('-');
            }
            text.push_str(&format!("{byte:02x}"));
        }
        TransactionId(text)
    }

    /// An ID as the table has it: `8-4-4-4-12` lowercase hex digits.
    pub fn parse(text: &str) -> Result<TransactionId> {
        let well_formed = text.len() == 36
            && text.chars().enumerate().all(|(i, c)| match i {
                8 | 13 | 18 | 23 => c == '-',
                _ => matches!(c, '0'..='9' | 'a'..='f'),
            });
        if !well_formed {
            bail!("{text:?} isn't a transaction ID (a UUID in lowercase)");
        }
        Ok(TransactionId(text.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// One row of the table: a transaction as far as we know it. Only the
/// account, the date and the amount are always there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stored {
    pub id: TransactionId,
    /// The source it belongs to, as named in `config/sources.toml`.
    pub account: String,
    pub date: Date,
    pub amount: Decimal,
    pub currency: Option<String>,
    pub text: Option<String>,
    pub counterparty_name: Option<String>,
    pub counterparty: Option<String>,
    /// The running balance after it.
    pub balance: Option<Decimal>,
    /// The bank's own reference for it.
    pub reference: Option<String>,
    pub time: Option<Time>,
    pub value_date: Option<Date>,
    pub transaction_date: Option<Date>,
    pub original_amount: Option<Decimal>,
    pub original_currency: Option<String>,
    /// Set when this row turned out to be the same transaction as another:
    /// that one's ID. Builds will skip this row, and what names its ID means
    /// the other.
    pub same_as: Option<TransactionId>,
    /// Set when the row was added without knowing whether it's a transaction
    /// the table already had: that one's ID, for a person to settle.
    pub maybe_same_as: Option<TransactionId>,
    /// Every place in the raw files it was read from.
    pub sources: Vec<Origin>,
}

/// The table's columns, in order. The ones that are awkward to read come last.
const COLUMNS: [&str; 18] = [
    "id",
    "account",
    "date",
    "amount",
    "currency",
    "text",
    "counterparty_name",
    "counterparty",
    "balance",
    "reference",
    "time",
    "value_date",
    "transaction_date",
    "original_amount",
    "original_currency",
    "same_as",
    "maybe_same_as",
    "sources",
];

/// Between the places in `sources`: no path under `data/` has one.
const BETWEEN_SOURCES: char = '|';

fn text_of(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
}

fn parsed<T>(value: &str, parse: impl FnOnce(&str) -> Result<T>) -> Result<Option<T>> {
    if value.is_empty() { Ok(None) } else { parse(value).map(Some) }
}

fn date_of(value: &str) -> Result<Date> {
    value.parse().with_context(|| format!("{value:?} isn't a date"))
}

fn time_of(value: &str) -> Result<Time> {
    value.parse().with_context(|| format!("{value:?} isn't a time of day"))
}

fn sources_of(value: &str) -> Result<Vec<Origin>> {
    value
        .split(BETWEEN_SOURCES)
        .filter(|place| !place.is_empty())
        .map(|place| {
            let (file, line) = place.rsplit_once(':').with_context(|| format!("{place:?} isn't file:line"))?;
            let line = line.parse().with_context(|| format!("{place:?} isn't file:line"))?;
            Ok(Origin { file: file.to_owned(), line })
        })
        .collect()
}

fn written<T>(value: Option<&T>, write: impl FnOnce(&T) -> String) -> String {
    value.map(write).unwrap_or_default()
}

impl Stored {
    /// A new row for `row`, a transaction of `account` that the table didn't have.
    fn new(id: TransactionId, account: &str, row: &BankTransaction) -> Stored {
        Stored {
            id,
            account: account.to_owned(),
            date: row.booked,
            amount: row.amount,
            currency: text_of(&row.currency),
            text: text_of(&row.description),
            counterparty_name: row.counterparty_name.clone(),
            counterparty: row.counterparty.clone(),
            balance: row.balance_after,
            reference: row.bank_ref.clone(),
            time: row.time,
            value_date: row.value_date,
            transaction_date: row.transaction_date,
            original_amount: row.original_amount,
            original_currency: row.original_currency.clone(),
            same_as: None,
            maybe_same_as: None,
            sources: row.origins.clone(),
        }
    }

    fn from_cells(cells: &[&str; 18]) -> Result<Stored> {
        let [
            id,
            account,
            date,
            amount,
            currency,
            text,
            counterparty_name,
            counterparty,
            balance,
            reference,
            time,
            value_date,
            transaction_date,
            original_amount,
            original_currency,
            same_as,
            maybe_same_as,
            sources,
        ] = *cells;
        if account.is_empty() {
            bail!("a transaction has no account");
        }
        Ok(Stored {
            id: TransactionId::parse(id)?,
            account: account.to_owned(),
            date: date_of(date)?,
            amount: parse_plain(amount)?,
            currency: text_of(currency),
            text: text_of(text),
            counterparty_name: text_of(counterparty_name),
            counterparty: text_of(counterparty),
            balance: parsed(balance, parse_plain)?,
            reference: text_of(reference),
            time: parsed(time, time_of)?,
            value_date: parsed(value_date, date_of)?,
            transaction_date: parsed(transaction_date, date_of)?,
            original_amount: parsed(original_amount, parse_plain)?,
            original_currency: text_of(original_currency),
            same_as: parsed(same_as, TransactionId::parse)?,
            maybe_same_as: parsed(maybe_same_as, TransactionId::parse)?,
            sources: sources_of(sources)?,
        })
    }

    fn cells(&self) -> [String; 18] {
        let places: Vec<String> = self.sources.iter().map(|o| format!("{}:{}", o.file, o.line)).collect();
        [
            self.id.to_string(),
            self.account.clone(),
            self.date.to_string(),
            canonical(self.amount),
            self.currency.clone().unwrap_or_default(),
            self.text.clone().unwrap_or_default(),
            self.counterparty_name.clone().unwrap_or_default(),
            self.counterparty.clone().unwrap_or_default(),
            written(self.balance.as_ref(), |b| canonical(*b)),
            self.reference.clone().unwrap_or_default(),
            written(self.time.as_ref(), Time::to_string),
            written(self.value_date.as_ref(), Date::to_string),
            written(self.transaction_date.as_ref(), Date::to_string),
            written(self.original_amount.as_ref(), |a| canonical(*a)),
            self.original_currency.clone().unwrap_or_default(),
            written(self.same_as.as_ref(), TransactionId::to_string),
            written(self.maybe_same_as.as_ref(), TransactionId::to_string),
            places.join(&BETWEEN_SOURCES.to_string()),
        ]
    }

    fn short(&self) -> String {
        format!("{} {} {} {:?}", self.account, self.date, canonical(self.amount), self.text.as_deref().unwrap_or(""))
    }

    /// Whether `row` could be this transaction: the same day and amount, and
    /// nothing both have that says otherwise.
    fn looks_like(&self, row: &BankTransaction) -> bool {
        self.date == row.booked
            && self.amount == row.amount
            && agree(self.balance.as_ref(), row.balance_after.as_ref())
            && agree(self.reference.as_ref(), row.bank_ref.as_ref())
    }

    fn has_reference_of(&self, row: &BankTransaction) -> bool {
        self.reference.is_some() && self.reference == row.bank_ref
    }
}

/// Whether two copies of a field can be of one transaction: they say the
/// same, or one of them says nothing.
fn agree<T: PartialEq>(stored: Option<&T>, incoming: Option<&T>) -> bool {
    match (stored, incoming) {
        (Some(stored), Some(incoming)) => stored == incoming,
        _ => true,
    }
}

/// Fills `slot` from `value` if it's empty. A value that is there stays.
fn fill<T: Clone>(slot: &mut Option<T>, value: Option<&T>) {
    if slot.is_none() {
        *slot = value.cloned();
    }
}

/// Fills `slot` from a copy's `value` if it's empty. A value that is there
/// stays, and a copy that says something else is noted in `said`.
fn settle<T: Clone + PartialEq + fmt::Debug>(
    field: &str,
    slot: &mut Option<T>,
    value: Option<&T>,
    said: &mut Vec<String>,
) {
    match (slot.as_ref(), value) {
        (Some(has), Some(value)) if has != value => {
            said.push(format!("{field} is {has:?} in the table and {value:?}"));
        }
        (None, Some(value)) => *slot = Some(value.clone()),
        _ => {}
    }
}

/// What reading a file did to the table.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Reading {
    /// Rows that were transactions the table already had.
    pub matched: usize,
    /// The rows added.
    pub added: Vec<TransactionId>,
    /// Rows added that may be a transaction the table already had: the new
    /// row's ID, and the other's. The new row has it as `maybe_same_as`.
    pub unsure: Vec<(TransactionId, TransactionId)>,
}

/// Every transaction, by ID.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Table {
    rows: BTreeMap<TransactionId, Stored>,
}

impl Table {
    /// Reads the table from its file's text.
    pub fn parse(text: &str) -> Result<Table> {
        let mut reader = csv::ReaderBuilder::new().from_reader(text.as_bytes());
        let header = reader.headers().context("the table's first line")?;
        if !header.iter().eq(COLUMNS) {
            bail!("the table's columns are {:?}, not {COLUMNS:?}", header.iter().collect::<Vec<_>>());
        }
        let mut rows = BTreeMap::new();
        for (i, record) in reader.records().enumerate() {
            let line = i + 2;
            let record = record.with_context(|| format!("line {line} of the table"))?;
            let cells: Vec<&str> = record.iter().collect();
            let cells: &[&str; 18] = cells
                .as_slice()
                .try_into()
                .ok()
                .with_context(|| format!("line {line} of the table has {} cells", record.len()))?;
            let row = Stored::from_cells(cells).with_context(|| format!("line {line} of the table"))?;
            if let Some(twice) = rows.insert(row.id.clone(), row) {
                bail!("line {line} of the table: {} is there twice", twice.id);
            }
        }
        let table = Table { rows };
        for row in table.rows.values() {
            for other in [&row.same_as, &row.maybe_same_as].into_iter().flatten() {
                if other == &row.id || !table.rows.contains_key(other) {
                    bail!("{} is said to be the same as {other}, which isn't another transaction", row.id);
                }
            }
            if table.resolve(&row.id).is_none() {
                bail!("{} is said to be the same as rows that lead back to it", row.id);
            }
        }
        Ok(table)
    }

    /// The table as its file's text: one line per transaction, sorted by ID,
    /// so that two copies of the books that each added rows merge without
    /// conflicts.
    pub fn write(&self) -> Result<String> {
        let mut writer = csv::Writer::from_writer(Vec::new());
        writer.write_record(COLUMNS)?;
        for row in self.rows.values() {
            writer.write_record(row.cells())?;
        }
        let bytes = writer.into_inner().map_err(|e| anyhow::anyhow!("writing the table: {e}"))?;
        String::from_utf8(bytes).context("the table is text")
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The row with this ID, also when it has been joined to another.
    pub fn get(&self, id: &TransactionId) -> Option<&Stored> {
        self.rows.get(id)
    }

    /// The transaction an ID means: its own row, or the one it has been
    /// joined to through `same_as`.
    pub fn resolve(&self, id: &TransactionId) -> Option<&Stored> {
        let mut row = self.rows.get(id)?;
        // No longer than the table, should rows point at each other in a ring.
        for _ in 0..self.rows.len() {
            match &row.same_as {
                Some(other) => row = self.rows.get(other)?,
                None => return Some(row),
            }
        }
        None
    }

    /// Records that `a` and `b` are one transaction. The row with the lower
    /// ID is the one kept, so that two copies of the books that join the same
    /// pair choose alike: it takes over what the other row has and it lacks,
    /// and the other row's `same_as` says so. Nothing is removed. Rows that
    /// can't be one transaction are refused.
    pub fn join(&mut self, a: &TransactionId, b: &TransactionId) -> Result<()> {
        let unknown = |id: &TransactionId| format!("the table has no transaction {id}");
        let a = self.resolve(a).with_context(|| unknown(a))?.clone();
        let b = self.resolve(b).with_context(|| unknown(b))?.clone();
        let (kept, other) = if a.id <= b.id { (a, b) } else { (b, a) };
        if kept.id == other.id {
            return Ok(());
        }
        let one = kept.account == other.account
            && kept.date == other.date
            && kept.amount == other.amount
            && agree(kept.balance.as_ref(), other.balance.as_ref())
            && agree(kept.reference.as_ref(), other.reference.as_ref())
            && agree(kept.currency.as_ref(), other.currency.as_ref());
        if !one {
            bail!("{} and {} can't be one transaction: {} / {}", kept.id, other.id, kept.short(), other.short());
        }
        if let Some(row) = self.rows.get_mut(&kept.id) {
            fill(&mut row.currency, other.currency.as_ref());
            fill(&mut row.text, other.text.as_ref());
            fill(&mut row.counterparty_name, other.counterparty_name.as_ref());
            fill(&mut row.counterparty, other.counterparty.as_ref());
            fill(&mut row.balance, other.balance.as_ref());
            fill(&mut row.reference, other.reference.as_ref());
            fill(&mut row.time, other.time.as_ref());
            fill(&mut row.value_date, other.value_date.as_ref());
            fill(&mut row.transaction_date, other.transaction_date.as_ref());
            fill(&mut row.original_amount, other.original_amount.as_ref());
            fill(&mut row.original_currency, other.original_currency.as_ref());
            if row.maybe_same_as.as_ref() == Some(&other.id) {
                row.maybe_same_as = None;
            }
        }
        if let Some(row) = self.rows.get_mut(&other.id) {
            row.same_as = Some(kept.id);
            row.maybe_same_as = None;
        }
        Ok(())
    }

    /// The transactions: every row that hasn't been joined to another.
    pub fn transactions(&self) -> impl Iterator<Item = &Stored> {
        self.rows.values().filter(|row| row.same_as.is_none())
    }

    /// Takes in the rows of one file of `account`. A row that is surely a
    /// transaction the table has fills in what that one lacks; any other row
    /// is added, with `maybe_same_as` set if it may be one the table has.
    /// `mint` gives a new ID each time it's called.
    ///
    /// Sure means: the same bank reference, day and amount; or the same day,
    /// amount and text, with nothing both have that says otherwise. A file
    /// has a transaction once, so no two of its rows are taken for the same
    /// one. A row that was read from the same place before is the row it
    /// became then, and adds nothing.
    ///
    /// What the table has is never overwritten. If a copy says something
    /// else than the table about a field both have, the read fails, naming
    /// each such field, and the table is left as it was.
    pub fn read(
        &mut self,
        account: &str,
        rows: &[BankTransaction],
        mint: &mut dyn FnMut() -> TransactionId,
    ) -> Result<Reading> {
        let mut table = self.clone();
        let mut said = Vec::new();
        let reading = table.take_in(account, rows, mint, &mut said);
        if !said.is_empty() {
            bail!(
                "{} copies say something else than the table, which was left as it was:\n  {}",
                said.len(),
                said.join("\n  ")
            );
        }
        *self = table;
        Ok(reading)
    }

    /// What [`Table::read`] does, noting in `said` where a copy says
    /// something else than the table.
    fn take_in(
        &mut self,
        account: &str,
        rows: &[BankTransaction],
        mint: &mut dyn FnMut() -> TransactionId,
        said: &mut Vec<String>,
    ) -> Reading {
        let mut reading = Reading::default();
        let mut rows: Vec<&BankTransaction> = rows.iter().collect();
        rows.sort_by_key(|row| (row.booked, row.time, row.order_hint));
        // Rows of the table that a row of this file has been taken for, or
        // that this file added.
        let mut taken: BTreeSet<TransactionId> = BTreeSet::new();
        let mut unmatched = Vec::new();
        for row in rows {
            if let Some(id) = self.read_before(account, row) {
                taken.insert(id);
                reading.matched += 1;
            } else if let Some(id) = self.surely(account, row, &taken) {
                self.fill(&id, row, said);
                taken.insert(id);
                reading.matched += 1;
            } else {
                unmatched.push(row);
            }
        }
        for row in unmatched {
            let other = self.candidates(account, row, &taken).first().map(|stored| stored.id.clone());
            let mut id = mint();
            while self.rows.contains_key(&id) {
                id = mint();
            }
            let mut stored = Stored::new(id.clone(), account, row);
            stored.maybe_same_as.clone_from(&other);
            self.rows.insert(id.clone(), stored);
            taken.insert(id.clone());
            if let Some(other) = other {
                taken.insert(other.clone());
                reading.unsure.push((id.clone(), other));
            }
            reading.added.push(id);
        }
        reading
    }

    /// The transaction that `row` became when its place in its file was read
    /// before, or the one that has since been joined to.
    fn read_before(&self, account: &str, row: &BankTransaction) -> Option<TransactionId> {
        let stored = self
            .rows
            .values()
            .find(|stored| stored.account == account && row.origins.iter().any(|o| stored.sources.contains(o)))?;
        Some(self.resolve(&stored.id).unwrap_or(stored).id.clone())
    }

    /// The transaction `row` surely is, if the table has it.
    fn surely(&self, account: &str, row: &BankTransaction, taken: &BTreeSet<TransactionId>) -> Option<TransactionId> {
        let candidates = self.candidates(account, row, taken);
        let by_reference: Vec<&&Stored> =
            candidates.iter().filter(|stored| stored.has_reference_of(row) && stored.looks_like(row)).collect();
        if let [only] = by_reference.as_slice() {
            return Some(only.id.clone());
        }
        let text = text_of(&row.description);
        candidates
            .iter()
            .find(|stored| by_reference.is_empty() && stored.looks_like(row) && stored.text == text)
            .map(|stored| stored.id.clone())
    }

    /// The transactions of `account` that `row` could be and that no other
    /// row of its file has been taken for: first those with its bank
    /// reference, then those that look like it.
    fn candidates(&self, account: &str, row: &BankTransaction, taken: &BTreeSet<TransactionId>) -> Vec<&Stored> {
        let mut candidates: Vec<&Stored> = self
            .transactions()
            .filter(|stored| stored.account == account && !taken.contains(&stored.id))
            .filter(|stored| stored.has_reference_of(row) || stored.looks_like(row))
            .collect();
        candidates.sort_by_key(|stored| !stored.has_reference_of(row));
        candidates
    }

    /// Fills in what the transaction `id` lacks from `row`, a copy of it.
    /// What it has stays, and where the copy says something else that is
    /// noted in `said`.
    fn fill(&mut self, id: &TransactionId, row: &BankTransaction, said: &mut Vec<String>) {
        let Some(stored) = self.rows.get_mut(id) else { return };
        let short = stored.short();
        let mut differs = Vec::new();
        settle("the currency", &mut stored.currency, text_of(&row.currency).as_ref(), &mut differs);
        settle("the text", &mut stored.text, text_of(&row.description).as_ref(), &mut differs);
        settle("the counterparty's name", &mut stored.counterparty_name, row.counterparty_name.as_ref(), &mut differs);
        settle("the counterparty", &mut stored.counterparty, row.counterparty.as_ref(), &mut differs);
        settle("the balance", &mut stored.balance, row.balance_after.as_ref(), &mut differs);
        settle("the reference", &mut stored.reference, row.bank_ref.as_ref(), &mut differs);
        settle("the time of day", &mut stored.time, row.time.as_ref(), &mut differs);
        settle("the value date", &mut stored.value_date, row.value_date.as_ref(), &mut differs);
        settle("the transaction date", &mut stored.transaction_date, row.transaction_date.as_ref(), &mut differs);
        settle("the original amount", &mut stored.original_amount, row.original_amount.as_ref(), &mut differs);
        settle("the original currency", &mut stored.original_currency, row.original_currency.as_ref(), &mut differs);
        let file = row.origins.first().map_or("?", |o| o.file.as_str());
        said.extend(differs.into_iter().map(|differs| format!("{short}: {differs} in {file}")));
        for origin in &row.origins {
            if !stored.sources.contains(origin) {
                stored.sources.push(origin.clone());
            }
        }
    }
}
