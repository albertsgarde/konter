//! The store of transactions: our own record of every bank transaction, in
//! one format for all banks, kept as a plain table in a text file.
//!
//! A bank's files show the same transaction differently from one format to
//! the next, and can't always tell two transactions apart, so an ID derived
//! from them can change when more data arrives. Here a transaction gets its
//! ID once, when it's first read, and keeps it. Reading a file reconciles its
//! rows with the table ([`Table::read`]): a row that is a transaction the
//! table has fills in what that one lacks, and any other row is added. What
//! the table has is never overwritten, and reading never fails: a row that
//! may be a transaction the table has is added and marked (`maybe_same_as`),
//! and a copy that says something else than its transaction is noted on it
//! (`conflicts`), both for a person to settle.
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
    /// The copies of it that say something else than this row about a field
    /// both have, for a person to settle. The row keeps what it had.
    pub conflicts: Vec<Conflict>,
    /// Every place in the raw files it was read from.
    pub sources: Vec<Origin>,
}

/// A copy of a transaction that says something else than its row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    /// Where the copy is in the raw files.
    pub place: Origin,
    /// The columns it differs in.
    pub columns: Vec<String>,
}

/// The table's columns, in order. The ones that are awkward to read come last.
const COLUMNS: [&str; 19] = [
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
    "conflicts",
    "sources",
];

/// Between the places in `sources` and the copies in `conflicts`: no path
/// under `data/` has one.
const BETWEEN_PLACES: char = '|';

/// Between a copy's place and its columns in `conflicts`, and between the columns.
const BEFORE_COLUMNS: char = '=';
const BETWEEN_COLUMNS: char = '+';

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

fn place_of(place: &str) -> Result<Origin> {
    let (file, line) = place.rsplit_once(':').with_context(|| format!("{place:?} isn't file:line"))?;
    let line = line.parse().with_context(|| format!("{place:?} isn't file:line"))?;
    Ok(Origin { file: file.to_owned(), line })
}

fn place(origin: &Origin) -> String {
    format!("{}:{}", origin.file, origin.line)
}

fn sources_of(value: &str) -> Result<Vec<Origin>> {
    value.split(BETWEEN_PLACES).filter(|place| !place.is_empty()).map(place_of).collect()
}

/// A `conflicts` cell: `file:line=column+column` for each copy.
fn conflicts_of(value: &str) -> Result<Vec<Conflict>> {
    value
        .split(BETWEEN_PLACES)
        .filter(|conflict| !conflict.is_empty())
        .map(|conflict| {
            let (place, columns) = conflict
                .rsplit_once(BEFORE_COLUMNS)
                .with_context(|| format!("{conflict:?} isn't file:line{BEFORE_COLUMNS}columns"))?;
            let columns: Vec<String> = columns.split(BETWEEN_COLUMNS).map(str::to_owned).collect();
            if let Some(unknown) = columns.iter().find(|column| !COLUMNS.contains(&column.as_str())) {
                bail!("{conflict:?} names {unknown:?}, which isn't a column");
            }
            Ok(Conflict { place: place_of(place)?, columns })
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
            conflicts: Vec::new(),
            sources: row.origins.clone(),
        }
    }

    fn from_cells(cells: &[&str; 19]) -> Result<Stored> {
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
            conflicts,
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
            conflicts: conflicts_of(conflicts)?,
            sources: sources_of(sources)?,
        })
    }

    fn cells(&self) -> [String; 19] {
        let places: Vec<String> = self.sources.iter().map(place).collect();
        let conflicts: Vec<String> = self
            .conflicts
            .iter()
            .map(|c| format!("{}{BEFORE_COLUMNS}{}", place(&c.place), c.columns.join(&BETWEEN_COLUMNS.to_string())))
            .collect();
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
            conflicts.join(&BETWEEN_PLACES.to_string()),
            places.join(&BETWEEN_PLACES.to_string()),
        ]
    }

    /// The transaction in a few words, for messages.
    pub fn short(&self) -> String {
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

    /// What the row has in `column`, as the table's text has it. `None` if
    /// that isn't a column.
    pub fn cell(&self, column: &str) -> Option<String> {
        let at = COLUMNS.iter().position(|name| *name == column)?;
        self.cells().into_iter().nth(at)
    }

    /// This transaction as `row`, a copy of it, has it.
    pub fn read_as(&self, row: &BankTransaction) -> Stored {
        Stored::new(self.id.clone(), &self.account, row)
    }

    /// The columns in which `copy` says something else than this row about
    /// a field both have, each with what the two say.
    fn differences(&self, copy: &Stored) -> Vec<(&'static str, String)> {
        let mut found = Vec::new();
        differ("currency", self.currency.as_ref(), copy.currency.as_ref(), &mut found);
        differ("text", self.text.as_ref(), copy.text.as_ref(), &mut found);
        differ("counterparty_name", self.counterparty_name.as_ref(), copy.counterparty_name.as_ref(), &mut found);
        differ("counterparty", self.counterparty.as_ref(), copy.counterparty.as_ref(), &mut found);
        differ("balance", self.balance.as_ref(), copy.balance.as_ref(), &mut found);
        differ("reference", self.reference.as_ref(), copy.reference.as_ref(), &mut found);
        differ("time", self.time.as_ref(), copy.time.as_ref(), &mut found);
        differ("value_date", self.value_date.as_ref(), copy.value_date.as_ref(), &mut found);
        differ("transaction_date", self.transaction_date.as_ref(), copy.transaction_date.as_ref(), &mut found);
        differ("original_amount", self.original_amount.as_ref(), copy.original_amount.as_ref(), &mut found);
        differ("original_currency", self.original_currency.as_ref(), copy.original_currency.as_ref(), &mut found);
        found
    }

    /// Takes over what `copy` has and this row lacks. What this row has stays.
    fn absorb(&mut self, copy: &Stored) {
        fill(&mut self.currency, copy.currency.as_ref());
        fill(&mut self.text, copy.text.as_ref());
        fill(&mut self.counterparty_name, copy.counterparty_name.as_ref());
        fill(&mut self.counterparty, copy.counterparty.as_ref());
        fill(&mut self.balance, copy.balance.as_ref());
        fill(&mut self.reference, copy.reference.as_ref());
        fill(&mut self.time, copy.time.as_ref());
        fill(&mut self.value_date, copy.value_date.as_ref());
        fill(&mut self.transaction_date, copy.transaction_date.as_ref());
        fill(&mut self.original_amount, copy.original_amount.as_ref());
        fill(&mut self.original_currency, copy.original_currency.as_ref());
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

/// Notes in `found` that a copy says something else about `column` than the
/// table has, if both say something.
fn differ<T: PartialEq + fmt::Debug>(
    column: &'static str,
    has: Option<&T>,
    says: Option<&T>,
    found: &mut Vec<(&'static str, String)>,
) {
    if let (Some(has), Some(says)) = (has, says)
        && has != says
    {
        found.push((column, format!("{column} is {has:?} in the table and {says:?}")));
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
    /// What copies said that differs from the table, a line for each column.
    /// The transactions have them in `conflicts`.
    pub conflicts: Vec<String>,
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
            let cells: &[&str; 19] = cells
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
            row.absorb(&other);
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

    /// The rows that were added without knowing whether they are a
    /// transaction the table already had, each with that transaction: what
    /// `maybe_same_as` names, or the one that has since been joined to. A
    /// row that has turned out to be that transaction itself isn't among them.
    pub fn unsure(&self) -> Vec<(&Stored, &Stored)> {
        self.rows
            .values()
            .filter_map(|row| Some((row, self.resolve(row.maybe_same_as.as_ref()?)?)))
            .filter(|(row, other)| self.resolve(&row.id).is_some_and(|row| row.id != other.id))
            .collect()
    }

    /// The copies that say something else than the table, each with the row
    /// that noted it.
    pub fn conflicts(&self) -> Vec<(&Stored, &Conflict)> {
        self.rows.values().flat_map(|row| row.conflicts.iter().map(move |conflict| (row, conflict))).collect()
    }

    /// The raw files that rows have been read from. Reading a file puts
    /// every one of its rows in the table, so a file with rows that is here
    /// has been read.
    pub fn files(&self) -> BTreeSet<&str> {
        self.rows.values().flat_map(|row| &row.sources).map(|origin| origin.file.as_str()).collect()
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
    /// What the table has is never overwritten. Where a copy with a
    /// transaction's reference says something else about a field both have,
    /// the transaction keeps what it had and notes the copy in `conflicts`.
    pub fn read(
        &mut self,
        account: &str,
        rows: &[BankTransaction],
        mint: &mut dyn FnMut() -> TransactionId,
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
                self.fill(&id, row, &mut reading.conflicts);
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
            .filter(|stored| by_reference.is_empty() && stored.looks_like(row) && stored.text == text)
            .find(|stored| stored.differences(&stored.read_as(row)).is_empty())
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
    /// What it has stays, and where the copy says something else, the
    /// transaction notes the copy and `said` gets a line for each column.
    fn fill(&mut self, id: &TransactionId, row: &BankTransaction, said: &mut Vec<String>) {
        let Some(stored) = self.rows.get_mut(id) else { return };
        let copy = stored.read_as(row);
        let differences = stored.differences(&copy);
        if !differences.is_empty() {
            let from = row.origin();
            said.extend(differences.iter().map(|(_, says)| format!("{}: {says} in {}", stored.short(), place(from))));
            let columns = differences.iter().map(|(column, _)| (*column).to_owned()).collect();
            stored.conflicts.push(Conflict { place: from.clone(), columns });
        }
        stored.absorb(&copy);
        for origin in &row.origins {
            if !stored.sources.contains(origin) {
                stored.sources.push(origin.clone());
            }
        }
    }
}
