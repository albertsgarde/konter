//! Reading raw bank data into [`BankTransaction`]s (docs/spec.md §4).

mod chain;
pub mod enable_banking;
pub mod lunar_csv;
pub mod nordea_csv;
pub mod stored;

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};

use crate::config::{Config, Format};
use crate::findings::Findings;
use crate::model::BankTransaction;
use crate::store::{FileRead, Files, Reading, Table, TransactionId};

pub use stored::SourceData;

/// What a parser needs to know about the file it reads.
pub struct FileContext {
    pub source: String,
    pub number: String,
    pub currency: String,
    /// Path relative to `data/`.
    pub rel: String,
    pub file_name: String,
}

/// One parsed raw file.
pub struct RawFile {
    pub rel: String,
    pub format: Format,
    /// In file order.
    pub rows: Vec<BankTransaction>,
    /// Balances the bank reported, if the file (or its sidecar) has any.
    pub snapshots: Vec<stored::Snapshot>,
    /// When the file was exported or fetched, if known.
    pub exported: Option<jiff::civil::Date>,
}

/// Files that sit next to raw data but aren't transaction data.
pub fn is_sidecar(rel: &str) -> bool {
    rel.ends_with(SIDECAR)
}

/// Raw file bytes as text: UTF-8, without a byte order mark. `name` is for messages.
pub fn decode_text(bytes: Vec<u8>, name: &str) -> Result<String> {
    let text = String::from_utf8(bytes).with_context(|| format!("{name} is not UTF-8"))?;
    Ok(text.strip_prefix('\u{feff}').map(str::to_string).unwrap_or(text))
}

pub fn detect_format(file_name: &str, text: &str) -> Result<Format> {
    if enable_banking::looks_like(file_name, text) {
        Ok(Format::EnableBanking)
    } else if nordea_csv::looks_like(text) {
        Ok(Format::NordeaCsv)
    } else if lunar_csv::looks_like(text) {
        Ok(Format::LunarCsv)
    } else {
        bail!("{file_name}: unrecognized file format")
    }
}

/// Parses one raw file of `source_name`, given its text, its path relative to
/// `data/`, and the text of its balance sidecar if it has one.
pub fn parse_raw(config: &Config, source_name: &str, rel: &str, text: &str, sidecar: Option<&str>) -> Result<RawFile> {
    let source = &config.sources[source_name];
    let file_name = rel.rsplit('/').next().unwrap_or(rel);
    let format = detect_format(file_name, text).with_context(|| format!("data/{rel}"))?;
    if !source.formats.contains(&format) {
        bail!("{rel}: {format} data isn't allowed for {source_name} (formats = {:?})", source.formats);
    }
    let ctx = FileContext {
        source: source_name.to_string(),
        number: source.number.clone().unwrap_or_default(),
        currency: config.source_currency(source).to_string(),
        rel: rel.to_string(),
        file_name: file_name.to_string(),
    };
    let (rows, mut snapshots) = match format {
        Format::NordeaCsv => nordea_csv::parse(text, &ctx).map(|r| (r, Vec::new())),
        Format::LunarCsv => lunar_csv::parse(text, &ctx).map(|r| (r, Vec::new())),
        Format::EnableBanking => enable_banking::parse(text, &ctx),
    }
    .with_context(|| format!("parsing {rel}"))?;
    if let Some(sidecar) = sidecar {
        snapshots.push(parse_sidecar(&format!("{rel}{SIDECAR}"), sidecar)?);
    }
    let exported = match format {
        Format::NordeaCsv => nordea_csv::export_date_from_filename(&ctx.file_name),
        Format::EnableBanking => enable_banking::fetched_date(text),
        Format::LunarCsv => None,
    };
    Ok(RawFile { rel: rel.to_string(), format, rows, snapshots, exported })
}

/// Suffix of the file `add-export --balance` writes next to an export.
pub const SIDECAR: &str = ".balance.toml";

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BalanceSidecar {
    pub date: crate::dates::ConfigDate,
    pub balance: crate::money::Amount,
}

fn parse_sidecar(rel: &str, text: &str) -> Result<stored::Snapshot> {
    let s: BalanceSidecar = toml::from_str(text).with_context(|| format!("parsing {rel}"))?;
    Ok(stored::Snapshot { date: s.date.0, balance: s.balance.0, file: rel.to_string() })
}

/// All bank data, per source.
pub struct Loaded {
    pub sources: BTreeMap<String, SourceData>,
}

impl Loaded {
    pub fn transactions(&self) -> impl Iterator<Item = &BankTransaction> {
        self.sources.values().flat_map(|s| s.txs.iter())
    }

    /// The ID of the transaction that `id` means: its own, or, for a row of
    /// the table that counts as another transaction, that one's.
    pub fn id_of<'a>(&'a self, id: &'a str) -> &'a str {
        self.sources.values().find_map(|s| s.aliases.get(id)).map_or(id, String::as_str)
    }

    /// The transaction `id` means (see [`Loaded::id_of`]).
    pub fn find(&self, id: &str) -> Option<&BankTransaction> {
        let id = self.id_of(id);
        self.transactions().find(|t| t.id == id)
    }

    /// Whether `id` is a transaction of the table from before its source's start.
    pub fn is_before_start(&self, id: &str) -> bool {
        self.sources.values().any(|s| s.before_start.contains(id))
    }
}

/// Raw files (paths relative to `data/`) in a directory that belongs to no
/// imported source.
pub fn orphans<'a>(config: &Config, rels: &'a [String]) -> Vec<&'a String> {
    rels.iter().filter(|rel| !is_sidecar(rel) && config.source_for_dir(dir_of(rel)).is_none()).collect()
}

/// The raw files (paths relative to `data/`) that belong to source `name`.
pub fn files_of<'a>(config: &Config, name: &str, rels: &'a [String]) -> Vec<&'a String> {
    let dir = Config::data_dir(&config.sources[name]).unwrap_or_default();
    rels.iter().filter(|rel| !is_sidecar(rel) && dir_of(rel) == dir).collect()
}

fn dir_of(rel: &str) -> &str {
    rel.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

/// Reads one raw file of source `name` into the store: its rows into the
/// table (see [`Table::read`]), and the file among the files that have been
/// read, with when it was taken and the balance the bank reported. A balance
/// recorded beside an export is a file of its own there.
pub fn read_into(
    table: &mut Table,
    files: &mut Files,
    name: &str,
    file: &RawFile,
    mint: &mut dyn FnMut() -> TransactionId,
) -> Reading {
    let reading = table.read(name, &file.rows, mint);
    let balance_in = |rel: &str| file.snapshots.iter().find(|s| s.file == rel).map(|s| (s.date, s.balance));
    files.record(FileRead {
        file: file.rel.clone(),
        account: name.to_owned(),
        exported: file.exported,
        balance: balance_in(&file.rel),
    });
    for beside in file.snapshots.iter().filter(|s| s.file != file.rel) {
        files.record(FileRead {
            file: beside.file.clone(),
            account: name.to_owned(),
            exported: None,
            balance: Some((beside.date, beside.balance)),
        });
    }
    reading
}

/// The transactions of one imported source that the store has, and reports
/// what came of it.
pub fn stored_source(config: &Config, name: &str, table: &Table, files: &Files, findings: &mut Findings) -> SourceData {
    let source = &config.sources[name];
    if files.of(name).next().is_none() {
        findings.warn("data", format!("{name}: no raw data yet"));
    }
    let data = stored::from_table(name, config.source_start(source), table, files, findings);
    summarize(name, &data, findings);
    data
}

/// Says in `findings` what a source's data comes to.
fn summarize(name: &str, data: &SourceData, findings: &mut Findings) {
    if let (Some(first), Some(last)) = (data.first(), data.last()) {
        let opening =
            data.opening.map(|o| format!(", opening balance {}", crate::money::fmt_num(o))).unwrap_or_default();
        let chain = match (data.has_running_balance, data.chain_ok) {
            (true, true) => ", running balance unbroken",
            (true, false) => ", running balance BROKEN",
            (false, _) => ", no running balance",
        };
        findings.ok(
            "data",
            format!(
                "{name}: {} transactions {first} … {last} from {} files{opening}{chain}",
                data.txs.len(),
                data.files.len()
            ),
        );
    }
}
