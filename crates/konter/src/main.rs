use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use jiff::civil::Date;
use rust_decimal::Decimal;

use konter_diagnostics::gaps::Filter;
use konter_engine::config::{Config, Format};
use konter_engine::dates::Period;
use konter_engine::engine::{Status, fallback_account};
use konter_engine::findings::{Findings, Severity};
use konter_engine::hash::sha256_hex;
use konter_engine::journal::render_transaction;
use konter_engine::model::{BankTransaction, name_instead, quoted_description};
use konter_engine::money::fmt_num;
use konter_engine::sources::{self, nordea_csv};
use konter_engine::subscriptions::Flow;
use konter_repo::books;
use konter_repo::fetch;
use konter_repo::init;
use konter_repo::load;
use konter_repo::manifest::{self, Manifest};
use konter_repo::repo::Repo;
use konter_report::site::{self, Page};

#[derive(Parser)]
#[command(
    name = "konter",
    version = env!("KONTER_VERSION"),
    about = "Turns raw bank data into hledger books using a rules file"
)]
struct Cli {
    /// Repository root (default: nearest parent directory with config/sources.toml).
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Make new, empty books in a directory, and make it a git repo.
    Init {
        /// Where the books go (made if it's missing). --root doesn't apply.
        dir: PathBuf,
    },
    /// Rebuild ledger/generated.journal and report what needs attention.
    Build {
        /// The date the books are built for (default: today).
        #[arg(long)]
        as_of: Option<Date>,
        /// Print every detail line instead of the first few per item.
        #[arg(long)]
        full: bool,
        /// Treat warnings as errors.
        #[arg(long)]
        strict: bool,
    },
    /// Write the site in build/ (open build/index.html): spending per account
    /// and month, and where the unmapped money is. `konter build` also writes it.
    Report,
    /// Check that ledger/generated.journal matches a fresh build, and let
    /// hledger check the books independently (`hledger check --strict`).
    Verify {
        #[arg(long)]
        full: bool,
        #[arg(long)]
        strict: bool,
    },
    /// Show every subscription: balance, deficits, what it's paid until, next due date, commitment.
    Subscriptions {
        #[arg(long)]
        as_of: Option<Date>,
    },
    /// Propose pattern rules for groups of similar unmatched transactions.
    Suggest {
        /// Only this source.
        source: Option<String>,
        /// Smallest group to suggest a rule for (single ones: `konter unmatched`).
        #[arg(long, default_value_t = 2)]
        min: usize,
    },
    /// Show where the unmapped money is: per account, then the largest groups of
    /// similar unmapped transactions, ranked by amount.
    Gaps {
        /// Only these sources (default: all).
        sources: Vec<String>,
        /// Leave out this source (repeatable), e.g. --skip nordea:spending.
        #[arg(long)]
        skip: Vec<String>,
        /// Only transactions booked on or after this date.
        #[arg(long)]
        from: Option<Date>,
        /// Only transactions booked on or before this date.
        #[arg(long)]
        to: Option<Date>,
        /// How many groups to list; 0 lists all.
        #[arg(long, default_value_t = 25)]
        top: usize,
    },
    /// Show per month how much of each account is mapped.
    Coverage {
        /// Only this source.
        source: Option<String>,
    },
    /// What needs attention, as a short message: new transactions to map (not
    /// marked later), numbered, and the build's errors and warnings. Prints
    /// nothing when there's nothing to say.
    Digest {
        /// Print JSON instead, with the transaction IDs behind the numbers and
        /// every finding's details. Printed even when there's nothing to say.
        #[arg(long)]
        json: bool,
    },
    /// Print a rule stub for every transaction no rule matches, except those marked later.
    Unmatched {
        /// Only this source, e.g. nordea:main.
        source: Option<String>,
    },
    /// List bank transactions in booking order.
    List {
        /// Only this source, e.g. nordea:main.
        source: Option<String>,
    },
    /// Show everything about one bank transaction.
    Explain { id: String },
    /// List the banks Enable Banking supports in a country.
    Banks {
        #[arg(long, default_value = "DK")]
        country: String,
    },
    /// Link a bank through Enable Banking (opens a consent valid for up to 180 days).
    ///
    /// Asks for the redirect URL in the terminal, or, without a terminal (over
    /// chat, say), in two steps: --start prints the URL to log in at, and
    /// --finish takes the URL the bank redirected to.
    Link {
        /// Bank name as in [banks.<name>] in config/sources.toml, e.g. nordea.
        bank: String,
        /// Requested consent length in days (capped by the bank's maximum).
        #[arg(long, default_value_t = 180)]
        days: u64,
        /// Only print the URL to log in at; finish with --finish.
        #[arg(long, conflicts_with = "finish")]
        start: bool,
        /// Finish a link begun with --start, with the URL the bank redirected to.
        #[arg(long, value_name = "REDIRECT_URL")]
        finish: Option<String>,
    },
    /// Show what Enable Banking reports about a linked bank's session.
    Session { bank: String },
    /// Fetch new transactions from Enable Banking into data/.
    Fetch {
        /// Only this source.
        #[arg(long)]
        source: Option<String>,
        /// Fetch from this date instead of 30 days before the last fetched transaction.
        #[arg(long)]
        from: Option<Date>,
    },
    /// Add a raw export file to data/ and check it against existing data.
    AddExport {
        file: PathBuf,
        /// Source the file belongs to (not needed for Nordea CSVs with their original name).
        #[arg(long)]
        source: Option<String>,
        /// File name to store it under (default: the file's own name).
        #[arg(long)]
        name: Option<String>,
        /// The balance the bank showed when you exported. Only needed for
        /// exports without a running-balance column; becomes a balance check.
        #[arg(long)]
        balance: Option<String>,
        /// The date --balance applies to (default: today).
        #[arg(long, requires = "balance")]
        balance_date: Option<Date>,
    },
}

/// Standard output, for `write!` and `writeln!`. `println!` panics when the
/// write fails, as it does once whoever reads the output has gone away
/// (`konter list | head`). This hands the failure back instead, so the command
/// stops there and `main` ends it quietly.
struct Stdout;

impl Stdout {
    fn write_fmt(&mut self, args: std::fmt::Arguments<'_>) -> Result<(), StdoutError> {
        std::io::Write::write_fmt(&mut std::io::stdout(), args).map_err(StdoutError)
    }
}

/// A write to standard output that failed.
#[derive(Debug)]
struct StdoutError(std::io::Error);

impl std::fmt::Display for StdoutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "writing to standard output: {}", self.0)
    }
}

impl std::error::Error for StdoutError {}

/// What a shell reports for a command killed by SIGPIPE (128 + 13), which is
/// how a command that writes to a closed pipe ends unless it handles it.
const CLOSED_PIPE: u8 = 141;

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => match e.downcast_ref::<StdoutError>() {
            Some(StdoutError(io)) if io.kind() == std::io::ErrorKind::BrokenPipe => ExitCode::from(CLOSED_PIPE),
            _ => {
                eprintln!("error: {e:#}");
                ExitCode::from(2)
            }
        },
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    // The one command that doesn't work on existing books.
    if let Command::Init { dir } = &cli.command {
        return init(dir);
    }
    let repo = &Repo::discover(cli.root.as_deref().unwrap_or(Path::new(".")))?;
    let config = &load::config(repo)?;
    match cli.command {
        Command::Build { as_of, full, strict } => build(repo, config, as_of, full, strict),
        Command::Report => report(repo, config),
        Command::Verify { full, strict } => verify(repo, config, full, strict),
        Command::Subscriptions { as_of } => subscriptions(repo, config, as_of),
        Command::Suggest { source, min } => suggest(repo, config, source.as_deref(), min),
        Command::Gaps { sources, skip, from, to, top } => gaps(repo, config, &Filter { sources, skip, from, to }, top),
        Command::Digest { json } => digest(repo, config, json),
        Command::Coverage { source } => coverage(repo, config, source.as_deref()),
        Command::Unmatched { source } => unmatched(repo, config, source.as_deref()),
        Command::List { source } => list(repo, config, source.as_deref()),
        Command::Explain { id } => explain(repo, config, &id),
        Command::AddExport { file, source, name, balance, balance_date } => {
            let balance = balance.map(|b| konter_engine::money::parse_plain(&b)).transpose()?;
            let snapshot = balance.map(|b| (balance_date.unwrap_or_else(today), b));
            add_export(repo, config, &file, source, name, snapshot)
        }
        Command::Banks { country } => banks(&country),
        Command::Link { bank, days, start, finish } => link(config, &bank, days, start, finish),
        Command::Session { bank } => session(&bank),
        Command::Fetch { source, from } => fetch_all(repo, config, source, from),
        Command::Init { .. } => unreachable!("init is handled before the books are found"),
    }
}

fn today() -> Date {
    jiff::Zoned::now().date()
}

/// `konter build`: writes the journals and the site, and reports what the build found.
fn build(repo: &Repo, config: &Config, as_of: Option<Date>, full: bool, strict: bool) -> Result<ExitCode> {
    let as_of = as_of.unwrap_or_else(today);
    let mut build = books::build(repo, config, as_of)?;
    books::write(repo, &build)?;
    consent_warnings(config, &mut build.findings);
    writeln!(Stdout, "wrote ledger/{} (--as-of {as_of})", books::GENERATED)?;
    let dir = site_dir(repo);
    match write_site(repo, config, &build) {
        Ok(()) => writeln!(Stdout, "wrote the site in {}/\n", repo.rel(&dir))?,
        Err(e) => {
            writeln!(Stdout)?;
            build.findings.warn("report", format!("couldn't write the site in {}/: {e:#}", repo.rel(&dir)));
        }
    }
    write!(Stdout, "{}", build.findings.render(full))?;
    Ok(exit_code(&build.findings, strict))
}

/// `konter report`: writes the site for the books as they were last built.
fn report(repo: &Repo, config: &Config) -> Result<ExitCode> {
    let journal = std::fs::read_to_string(repo.ledger(books::GENERATED)).unwrap_or_default();
    let as_of = konter_engine::journal::as_of_from_header(&journal)
        .context("ledger/generated.journal has no `--as-of` header; run `konter build` first")?;
    let build = books::build(repo, config, as_of)?;
    write_site(repo, config, &build)?;
    let dir = site_dir(repo);
    writeln!(Stdout, "wrote the site in {}/  (open file://{})", repo.rel(&dir), dir.join(Page::Home.file()).display())?;
    Ok(ExitCode::SUCCESS)
}

/// `konter verify`: checks the written journals against a fresh build, and with hledger.
fn verify(repo: &Repo, config: &Config, full: bool, strict: bool) -> Result<ExitCode> {
    let mut findings = books::verify(repo, config)?;
    // An independent check of the journals as written.
    books::hledger_check(repo, &mut findings);
    write!(Stdout, "{}", findings.render(full))?;
    Ok(exit_code(&findings, strict))
}

/// `konter subscriptions`: where each subscription stands.
fn subscriptions(repo: &Repo, config: &Config, as_of: Option<Date>) -> Result<ExitCode> {
    let as_of = as_of.unwrap_or_else(today);
    let build = books::build(repo, config, as_of)?;
    writeln!(
        Stdout,
        "{:<24} {:<8} {:>10} {:>11}  {:<28} {:<10}  {:<10}  {:<10}",
        "subscription", "kind", "price", "balance", "status", "paid until", "next due", "committed"
    )?;
    for (sub, state) in build.rules.subscriptions.iter().zip(&build.subscriptions) {
        let status = match (state.oldest_unpaid, state.credit) {
            (Some(d), c) => format!("{} owed since {d}", fmt_num(-c)),
            (None, c) if c > rust_decimal::Decimal::ZERO => format!("{} paid ahead", fmt_num(c)),
            _ => "ok".into(),
        };
        let opt = |d: Option<Date>| d.map(|d| d.to_string()).unwrap_or_else(|| "-".into());
        writeln!(
            Stdout,
            "{:<24} {:<8} {:>10} {:>11}  {:<28} {:<10}  {:<10}  {:<10}",
            sub.name,
            sub.kind_name(),
            if sub.variable { format!("~{}", fmt_num(state.price)) } else { fmt_num(state.price) },
            fmt_num(state.credit),
            status,
            opt(state.paid_until),
            opt(state.next_due),
            opt(state.committed_until)
        )?;
    }
    writeln!(Stdout, "\nbalance: paid minus due as of {as_of}; negative means owed.")?;
    writeln!(Stdout, "paid until: the last due date the payments cover, oldest first.")?;
    if build.rules.subscriptions.iter().any(|s| s.variable) {
        writeln!(Stdout, "~price: an estimate; each payment settles one due date, which books what was paid.")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `konter suggest`: rule stubs for groups of similar unmatched transactions.
fn suggest(repo: &Repo, config: &Config, source: Option<&str>, min: usize) -> Result<ExitCode> {
    let build = books::build(repo, config, today())?;
    let categories = reference_categories(repo);
    let (text, groups) = konter_diagnostics::suggest::suggest(&build, &categories, source, min);
    write!(Stdout, "{text}")?;
    eprintln!(
        "{groups} suggested rules; fill in `account` (or use another output) and paste into the source's file in config/rules/"
    );
    Ok(ExitCode::SUCCESS)
}

/// `konter gaps`: where the unmapped money is. `top`: how many groups to list, 0 for all.
fn gaps(repo: &Repo, config: &Config, filter: &Filter, top: usize) -> Result<ExitCode> {
    for name in filter.sources.iter().chain(&filter.skip) {
        if !config.sources.get(name).is_some_and(|s| s.imported()) {
            bail!("{name} is not an imported source");
        }
    }
    let build = books::build(repo, config, today())?;
    let gaps = konter_diagnostics::gaps::gaps(config, &build.loaded, &build.generated, filter);
    write!(Stdout, "{}", gaps.render(config, &reference_categories(repo), top))?;
    Ok(ExitCode::SUCCESS)
}

/// `konter digest`: what needs attention, as a short message or as JSON.
fn digest(repo: &Repo, config: &Config, json: bool) -> Result<ExitCode> {
    let mut build = books::build(repo, config, today())?;
    consent_warnings(config, &mut build.findings);
    let digest = konter_diagnostics::digest::digest(&build);
    if json {
        writeln!(Stdout, "{}", serde_json::to_string_pretty(&digest.json())?)?;
    } else {
        write!(Stdout, "{}", digest.text())?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `konter coverage`: how much of each account is mapped, per month.
fn coverage(repo: &Repo, config: &Config, source: Option<&str>) -> Result<ExitCode> {
    let build = books::build(repo, config, today())?;
    write!(Stdout, "{}", konter_diagnostics::coverage::coverage(&build, source))?;
    Ok(ExitCode::SUCCESS)
}

/// `konter unmatched`: a pin stub for every transaction no rule matches, except those marked later.
fn unmatched(repo: &Repo, config: &Config, source: Option<&str>) -> Result<ExitCode> {
    let build = books::build(repo, config, today())?;
    let (mut count, mut later) = (0, 0);
    for t in build.loaded.transactions() {
        if source.is_some_and(|s| s != t.source) {
            continue;
        }
        match build.generated.outcomes[&t.id].status {
            Status::Unmatched => {}
            Status::Later(_) => {
                later += 1;
                continue;
            }
            _ => continue,
        }
        count += 1;
        writeln!(Stdout, "# {}  {}  {}  {}", t.source, t.booked, fmt_num(t.amount), quoted_description(t))?;
        writeln!(Stdout, "[pin.\"{}\"]", t.id)?;
        writeln!(Stdout, "account = \"\"  # was {}\n", fallback_account(&t.source, t.amount))?;
    }
    match later {
        0 => eprintln!("{count} unmatched transactions"),
        _ => eprintln!("{count} unmatched transactions, and {later} more marked later (left out)"),
    }
    Ok(ExitCode::SUCCESS)
}

/// `konter list`: the bank transactions, in booking order.
fn list(repo: &Repo, config: &Config, source: Option<&str>) -> Result<ExitCode> {
    let mut findings = Findings::default();
    let loaded = load::all_sources(repo, config, &mut findings)?;
    for (name, data) in &loaded.sources {
        if source.is_some_and(|s| s != name) {
            continue;
        }
        for t in &data.txs {
            writeln!(
                Stdout,
                "{}  {}  {:>12}  {:>12}  {}",
                t.id,
                t.booked,
                fmt_num(t.amount),
                t.balance_after.map(fmt_num).unwrap_or_default(),
                name_instead(t).map_or_else(|| t.description.clone(), |name| format!("{} [{name}]", t.description))
            )?;
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `konter explain`: everything about one bank transaction, and what the rules made of it.
fn explain(repo: &Repo, config: &Config, id: &str) -> Result<ExitCode> {
    let build = books::build(repo, config, today())?;
    let t = build.loaded.find(id).with_context(|| format!("no bank transaction with ID {id}"))?;
    print_transaction(t)?;
    let outcome = &build.generated.outcomes[id];
    writeln!(Stdout, "\nrules:")?;
    if let Some(pin) = build.rules.pins.get(id) {
        writeln!(Stdout, "  pin in {} (always wins)", pin.file)?;
    }
    if let Some(later) = build.rules.later.get(id) {
        let note = later.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default();
        writeln!(Stdout, "  marked later in {}{note}", later.file)?;
    }
    if outcome.candidates.is_empty() {
        writeln!(Stdout, "  no pattern rule matches")?;
    }
    for (name, priority) in &outcome.candidates {
        let file = build.rules.patterns.iter().find(|r| &r.name == name).map_or("?", |r| r.file.as_str());
        writeln!(Stdout, "  rule {name:?} in {file} matches (priority {priority})")?;
    }
    writeln!(Stdout, "  → {:?}", outcome.status)?;
    writeln!(Stdout, "\njournal output:")?;
    if outcome.ledger.is_empty() {
        writeln!(Stdout, "  (none: {})", outcome.ignored.as_deref().unwrap_or("ignored"))?;
    }
    for &i in &outcome.ledger {
        let mut text = String::new();
        render_transaction(&mut text, &build.generated.txs[i]);
        writeln!(Stdout, "{text}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `konter banks`: the banks Enable Banking supports in a country.
fn banks(country: &str) -> Result<ExitCode> {
    let client = fetch::Client::new(&fetch::Settings::load()?)?;
    for a in client.aspsps(country)? {
        let days = a.get("maximum_consent_validity").and_then(|v| v.as_u64()).map(|s| s / 86_400);
        writeln!(
            Stdout,
            "{:<40} max consent {} days",
            a.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
            days.map(|d| d.to_string()).unwrap_or_else(|| "?".into())
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `konter link`: links a bank, in the terminal or in two steps (`--start`, then `--finish`).
fn link(config: &Config, bank: &str, days: u64, start: bool, finish: Option<String>) -> Result<ExitCode> {
    match (start, finish) {
        (false, None) => {
            let (url, pending) = start_link(config, bank, days)?;
            let redirect = fetch::Settings::load()?.redirect_url;
            writeln!(Stdout, "1. Open this URL and log in with MitID:\n\n   {url}\n")?;
            writeln!(Stdout, "2. You'll end up on {redirect} (the page may fail to load; that's fine).")?;
            writeln!(Stdout, "   Paste the full URL from the address bar here and press Enter:")?;
            let mut pasted = String::new();
            std::io::stdin().read_line(&mut pasted)?;
            finish_link(config, bank, &pasted, &pending)
        }
        (true, _) => {
            let (url, pending) = start_link(config, bank, days)?;
            let mut links = fetch::PendingLinks::load()?;
            links.banks.insert(bank.to_string(), pending);
            links.save()?;
            writeln!(Stdout, "{url}")?;
            eprintln!("Log in there, then run `konter link {bank} --finish '<the URL you end up on>'`.");
            Ok(ExitCode::SUCCESS)
        }
        (false, Some(redirect)) => {
            let mut links = fetch::PendingLinks::load()?;
            let pending = links
                .banks
                .get(bank)
                .with_context(|| format!("no link to {bank} begun: run `konter link {bank} --start` first"))?;
            let code = finish_link(config, bank, &redirect, pending)?;
            // The code in the redirect is single-use, so the pending link is
            // spent now that it has worked. One that didn't work is kept, so
            // that pasting the wrong URL doesn't mean logging in again.
            links.banks.remove(bank);
            links.save()?;
            Ok(code)
        }
    }
}

/// `konter session`: what Enable Banking reports about a linked bank's session.
fn session(bank: &str) -> Result<ExitCode> {
    let sessions = fetch::Sessions::load()?;
    let session = sessions.banks.get(bank).with_context(|| format!("{bank} isn't linked"))?;
    let client = fetch::Client::new(&fetch::Settings::load()?)?;
    let mut info = client.get(&format!("/sessions/{}", session.session_id), &[])?;
    if let Some(obj) = info.as_object_mut() {
        obj.remove("session_id");
    }
    writeln!(Stdout, "{}", serde_json::to_string_pretty(&info)?)?;
    for account in &session.accounts {
        let details = client.get(&format!("/accounts/{}/details", account.uid), &[])?;
        let field = |k: &str| details.get(k).and_then(|v| v.as_str()).unwrap_or("-").to_string();
        writeln!(
            Stdout,
            "{:<20} name: {}  product: {}  details: {}",
            account.iban.as_deref().unwrap_or("?"),
            field("name"),
            field("product"),
            field("details")
        )?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Where the site goes: build/, which isn't committed.
fn site_dir(repo: &Repo) -> PathBuf {
    repo.root.join("build")
}

/// Writes the site: the spending, income and gaps pages and the front page
/// leading to them. The gaps are over all sources and all time.
fn write_site(repo: &Repo, config: &Config, build: &konter_engine::pipeline::Build) -> Result<()> {
    let dir = site_dir(repo);
    let as_of = build.as_of;
    write_by_month(repo, config, build)?;
    let gaps = konter_diagnostics::gaps::gaps(config, &build.loaded, &build.generated, &Default::default());
    let currency = &config.books.currency;
    let body = gaps.html(config, &reference_categories(repo));
    site::write(&dir, Page::Gaps, &site::document(Page::Gaps, &gaps.subtitle(currency), &body))?;
    let cards = [
        (Page::Spending, "Expenses and money set aside, per account and month.".to_string()),
        (Page::Income, "Income per account and month.".to_string()),
        (Page::Gaps, gaps.headline(currency)),
    ];
    site::write(&dir, Page::Home, &site::home(&format!("As of {as_of}"), &cards))
}

/// Writes the spending and income pages, telling them what they need to know
/// about the books: currency, as-of, the accounts set aside, and the
/// subscriptions billed less often than monthly.
fn write_by_month(repo: &Repo, config: &Config, build: &konter_engine::pipeline::Build) -> Result<()> {
    let options = konter_report::Options {
        currency: config.books.currency.clone(),
        as_of: build.as_of.to_string(),
        set_aside: config
            .sources
            .iter()
            .filter(|(_, s)| s.set_aside)
            .map(|(name, s)| konter_report::SetAside { source: name.clone(), ledger: s.ledger.clone() })
            .collect(),
        spans: build
            .rules
            .subscriptions
            .iter()
            .filter_map(|s| match (&s.every, &s.flow) {
                (Period::Months(n), Flow::Expense(_) | Flow::Income(_)) if *n > 1 => Some((s.name.clone(), *n)),
                _ => None,
            })
            .collect(),
    };
    konter_report::write(&report_books(&build.books), &site_dir(repo), &options)
}

/// The books as the report reads them.
fn report_books(books: &[konter_engine::model::LedgerTransaction]) -> Vec<konter_report::Transaction> {
    books
        .iter()
        .map(|tx| konter_report::Transaction {
            date: tx.date,
            description: tx.description.clone(),
            tags: tx.tags.clone(),
            comments: tx.comments.clone(),
            postings: tx
                .postings
                .iter()
                .map(|p| konter_report::Posting {
                    account: p.account.clone(),
                    amount: p.amount,
                    commodity: p.commodity.clone(),
                })
                .collect(),
        })
        .collect()
}

/// The categories in the reference export, if there is one.
fn reference_categories(repo: &Repo) -> std::collections::BTreeMap<(Date, Decimal), String> {
    load::reference_export(repo)
        .map(|text| konter_diagnostics::suggest::reference_categories(&text))
        .unwrap_or_default()
}

fn exit_code(findings: &Findings, strict: bool) -> ExitCode {
    let failed = findings.has_errors() || (strict && findings.count(Severity::Warning) > 0);
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn print_transaction(t: &BankTransaction) -> Result<()> {
    writeln!(Stdout, "{}  ({})", t.id, t.source)?;
    writeln!(Stdout, "  booked        {}", t.booked)?;
    let opt = |label: &str, v: Option<String>| match v {
        Some(v) => writeln!(Stdout, "  {label:<13} {v}"),
        None => Ok(()),
    };
    opt("time", t.time.map(|x| x.strftime("%H:%M").to_string()))?;
    opt("value date", t.value_date.map(|d| d.to_string()))?;
    opt("purchased", t.transaction_date.map(|d| d.to_string()))?;
    writeln!(Stdout, "  amount        {} {}", fmt_num(t.amount), t.currency)?;
    opt(
        "original",
        t.original_amount.map(|a| format!("{} {}", fmt_num(a), t.original_currency.clone().unwrap_or_default())),
    )?;
    writeln!(Stdout, "  description   {:?}", t.description)?;
    opt("counterparty", t.counterparty.clone())?;
    opt("cp. name", t.counterparty_name.clone())?;
    opt("balance after", t.balance_after.map(fmt_num))?;
    opt("bank ref", t.bank_ref.clone())?;
    writeln!(Stdout, "  raw ({}):", t.format)?;
    for (k, v) in &t.raw {
        writeln!(Stdout, "    {k:<15} {v:?}")?;
    }
    writeln!(Stdout, "  found in:")?;
    for o in &t.origins {
        writeln!(Stdout, "    data/{}:{}", o.file, o.line)?;
    }
    Ok(())
}

fn add_export(
    repo: &Repo,
    config: &Config,
    file: &Path,
    source: Option<String>,
    name: Option<String>,
    snapshot: Option<(Date, rust_decimal::Decimal)>,
) -> Result<ExitCode> {
    let text = load::read_text(file)?;
    let file_name = file.file_name().context("not a file")?.to_string_lossy().into_owned();
    let format = sources::detect_format(&file_name, &text)?;
    let source_name = match source {
        Some(s) => s,
        None => {
            let number = (format == Format::NordeaCsv).then(|| nordea_csv::number_from_filename(&file_name)).flatten();
            let Some(number) = number else {
                bail!("can't tell which account {file_name} belongs to; pass --source");
            };
            config
                .sources
                .iter()
                .find(|(_, s)| {
                    s.bank == "nordea"
                        && s.number.as_deref().map(|n| n.trim_start_matches('0'))
                            == Some(number.trim_start_matches('0'))
                })
                .map(|(n, _)| n.clone())
                .with_context(|| format!("no source in config/sources.toml has Nordea account number {number}"))?
        }
    };
    let src = config.sources.get(&source_name).with_context(|| format!("unknown source {source_name}"))?;
    if !src.imported() {
        bail!(
            "{source_name} isn't imported yet. In config/sources.toml, give it `number`, `id_prefix` and \
             `formats = [\"{format}\"]` (and maybe `unmatched`), then run this again"
        );
    }
    if !src.formats.contains(&format) {
        bail!("{format} data isn't allowed for {source_name} (formats = {:?})", src.formats);
    }
    let dir = Config::data_dir(src).context("source has no data directory")?;
    let rel = format!("{dir}/{}", name.unwrap_or(file_name));
    let bytes = std::fs::read(file)?;
    let sidecar = snapshot.map(|(date, balance)| {
        let text =
            format!("# Balance shown by the bank at export time.\ndate = {date}\nbalance = \"{}\"\n", fmt_num(balance));
        (format!("{rel}{}", sources::SIDECAR), text)
    });
    let lock = manifest::lock(repo)?;
    let mut manifest = Manifest::load(&repo.manifest())?;
    // Everything is checked before anything is written.
    let in_place = manifest.check_add(&repo.data(), &rel, &bytes, "use --name")?;
    let sidecar_in_place = match &sidecar {
        Some((sidecar, text)) => manifest.check_add(
            &repo.data(),
            sidecar,
            text.as_bytes(),
            "a balance is only recorded once (if that one is wrong, delete the file and its line in data/MANIFEST, \
             then run this again)",
        )?,
        None => false,
    };
    if in_place {
        writeln!(Stdout, "data/{rel} is already in place")?;
    } else {
        add_raw_file(repo, &rel, &bytes)?;
        writeln!(Stdout, "copied to data/{rel}")?;
    }
    manifest.entries.insert(rel, sha256_hex(&bytes));
    if let Some((sidecar, text)) = &sidecar {
        if sidecar_in_place {
            writeln!(Stdout, "data/{sidecar} is already in place")?;
        } else {
            add_raw_file(repo, sidecar, text.as_bytes())?;
            writeln!(Stdout, "wrote data/{sidecar}")?;
        }
        manifest.entries.insert(sidecar.clone(), sha256_hex(text.as_bytes()));
    }
    manifest.save(&repo.manifest())?;
    drop(lock);
    writeln!(Stdout, "recorded in data/MANIFEST\n")?;

    let mut findings = Findings::default();
    load::source(repo, config, &source_name, &mut findings)?;
    write!(Stdout, "{}", findings.render(false))?;
    Ok(exit_code(&findings, false))
}

/// Writes a new raw file to data/. Raw files are never overwritten, so one
/// that's already there is an error.
fn add_raw_file(repo: &Repo, rel: &str, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let path = repo.data().join(rel);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .with_context(|| format!("adding data/{rel}"))?;
    file.write_all(bytes).with_context(|| format!("writing data/{rel}"))
}

/// Warns about Enable Banking consents that end within two weeks. Reads the
/// session file outside the repository, so it's not part of the reproducible build.
fn consent_warnings(config: &Config, findings: &mut Findings) {
    let Ok(sessions) = fetch::Sessions::load() else { return };
    let now = jiff::Timestamp::now();
    for (bank, session) in &sessions.banks {
        if !config.banks.contains_key(bank) {
            continue;
        }
        let Ok(until) = session.valid_until.parse::<jiff::Timestamp>() else { continue };
        let days = (until.as_second() - now.as_second()) / 86_400;
        if days < 0 {
            findings
                .warn("consent", format!("Enable Banking consent for {bank} has expired; run `konter link {bank}`"));
        } else if days < 14 {
            findings.warn(
                "consent",
                format!("Enable Banking consent for {bank} expires in {days} days; run `konter link {bank}`"),
            );
        }
    }
}

fn init(dir: &Path) -> Result<ExitCode> {
    let start = today().first_of_month();
    let (git, written) = init::init(dir, start)?;
    writeln!(Stdout, "Made new books in {}/, starting {start}:", dir.display())?;
    for rel in written {
        writeln!(Stdout, "  {rel}")?;
    }
    match git {
        init::Git::Initialized => writeln!(Stdout, "It's a new git repo (with jj: `jj git init --colocate` in it).")?,
        init::Git::Existing => writeln!(Stdout, "It was a git repo already.")?,
        init::Git::Failed(why) => {
            writeln!(Stdout, "It isn't a git repo yet ({why}): make it one before adding bank data.")?
        }
    }
    writeln!(
        Stdout,
        "\nThe books are private: keep them in a private repo. Next, add your bank accounts to\n\
         config/sources.toml (the examples there show how), get bank data in (konter's\n\
         docs/fetching.md), and `konter build`."
    )?;
    Ok(ExitCode::SUCCESS)
}

/// Begins linking a bank: the URL to log in at, and what the redirect has to
/// carry back for `finish_link`.
fn start_link(config: &Config, bank: &str, days: u64) -> Result<(String, fetch::PendingLink)> {
    let eb = config
        .banks
        .get(bank)
        .and_then(|b| b.enable_banking.as_ref())
        .with_context(|| format!("no [banks.{bank}.enable_banking] in config/sources.toml"))?;
    let settings = fetch::Settings::load()?;
    let client = fetch::Client::new(&settings)?;
    let aspsp = client
        .aspsps(&eb.country)?
        .into_iter()
        .find(|a| a.get("name").and_then(|n| n.as_str()) == Some(eb.aspsp.as_str()))
        .with_context(|| {
            format!("Enable Banking has no bank named {:?} in {} (see `konter banks`)", eb.aspsp, eb.country)
        })?;
    let max_secs = aspsp.get("maximum_consent_validity").and_then(|v| v.as_u64()).unwrap_or(90 * 86_400);
    let now = fetch::now_secs();
    let valid_until = fetch::rfc3339(now + (days * 86_400).min(max_secs));
    let state = sha256_hex(format!("{now}-{}", std::process::id()).as_bytes())[..16].to_string();
    let url = client.start_auth(&eb.aspsp, &eb.country, &valid_until, &state, &settings.redirect_url)?;
    Ok((url, fetch::PendingLink { state, valid_until }))
}

/// Finishes linking a bank from the URL the bank redirected to (or just its
/// code), and stores the session.
fn finish_link(config: &Config, bank: &str, redirect: &str, pending: &fetch::PendingLink) -> Result<ExitCode> {
    let client = fetch::Client::new(&fetch::Settings::load()?)?;
    let code = fetch::parse_redirect(redirect, &pending.state)?;
    let valid_until = &pending.valid_until;
    let session = client.create_session(&code)?;
    let accounts: Vec<fetch::SessionAccount> = session
        .get("accounts")
        .and_then(|a| a.as_array())
        .into_iter()
        .flatten()
        .filter_map(|a| {
            Some(fetch::SessionAccount {
                uid: a.get("uid")?.as_str()?.to_string(),
                iban: a.get("account_id").and_then(|i| i.get("iban")).and_then(|i| i.as_str()).map(str::to_string),
            })
        })
        .collect();
    let stored = fetch::Session {
        session_id: session.get("session_id").and_then(|s| s.as_str()).context("no session_id")?.to_string(),
        valid_until: session
            .get("access")
            .and_then(|a| a.get("valid_until"))
            .and_then(|v| v.as_str())
            .unwrap_or(valid_until)
            .to_string(),
        accounts,
    };
    let mut sessions = fetch::Sessions::load()?;
    writeln!(Stdout, "\nLinked {bank} until {}. Accounts:", stored.valid_until)?;
    sessions.banks.insert(bank.to_string(), stored);
    sessions.save()?;
    for a in &sessions.banks[bank].accounts {
        let iban = a.iban.as_deref().unwrap_or("?");
        let source = source_for_iban(config, bank, iban).unwrap_or("not in config/sources.toml; add it to import it");
        writeln!(Stdout, "  {iban:<24} {source}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The source whose IBAN, or account number, matches an IBAN.
fn source_for_iban<'a>(config: &'a Config, bank: &str, iban: &str) -> Option<&'a str> {
    let compact: String = iban.chars().filter(|c| !c.is_whitespace()).collect();
    config
        .sources
        .iter()
        .find(|(_, s)| {
            s.bank == bank
                && (s.iban.as_deref() == Some(compact.as_str())
                    || s.number.as_deref().is_some_and(|n| compact.ends_with(n)))
        })
        .map(|(name, _)| name.as_str())
}

fn fetch_all(repo: &Repo, config: &Config, only: Option<String>, from: Option<Date>) -> Result<ExitCode> {
    if let Some(only) = &only {
        let source = config.sources.get(only).with_context(|| format!("unknown source {only}"))?;
        if !source.formats.contains(&Format::EnableBanking) {
            bail!("{only} isn't fetched: it has no \"enable-banking\" in its formats in config/sources.toml");
        }
    }
    let settings = fetch::Settings::load()?;
    let sessions = fetch::Sessions::load()?;
    let client = fetch::Client::new(&settings)?;
    let mut findings = Findings::default();
    let mut fetched_any = false;
    for (name, source) in &config.sources {
        if only.as_ref().is_some_and(|o| o != name) || !source.formats.contains(&Format::EnableBanking) {
            continue;
        }
        let Some(session) = sessions.banks.get(&source.bank) else {
            findings.warn("fetch", format!("{name}: {} isn't linked; run `konter link {}`", source.bank, source.bank));
            continue;
        };
        if session.valid_until.parse::<jiff::Timestamp>().is_ok_and(|t| t < jiff::Timestamp::now()) {
            findings.error(
                "fetch",
                format!("{name}: consent expired {}; run `konter link {}`", session.valid_until, source.bank),
            );
            continue;
        }
        let Some(account) = session
            .accounts
            .iter()
            .find(|a| a.iban.as_deref().and_then(|i| source_for_iban(config, &source.bank, i)) == Some(name.as_str()))
        else {
            findings.error("fetch", format!("{name}: no linked account matches its number/IBAN"));
            continue;
        };
        // The earlier fetches. Their rows may be represented by CSV copies
        // after merging, so look at the files.
        let existing = load::source(repo, config, name, &mut Findings::default())?;
        let fetches = || existing.files.iter().filter(|f| f.rel.ends_with(".json"));
        // Overlap the previous fetch by 30 days, so a re-fetch can be checked.
        let date_from = from.unwrap_or_else(|| {
            let last = fetches().filter_map(|f| f.last).max();
            last.map_or_else(|| config.source_start(source), |d| d.saturating_sub(jiff::Span::new().days(30)))
        });
        writeln!(Stdout, "{name}: fetching from {date_from} …")?;
        let (pages, date_from) = client.transaction_pages(&account.uid, &date_from.to_string())?;
        let balances = client.balances(&account.uid)?;
        let now = jiff::Timestamp::now();
        let doc = serde_json::json!({
            sources::enable_banking::MARKER: 1,
            "fetched_at": now.to_string(),
            "bank": source.bank,
            "account_uid": account.uid,
            "date_from": date_from.to_string(),
            "pages": pages,
            "balances": balances,
        });
        let dir = Config::data_dir(source).context("source has no data directory")?;
        // Don't store a fetch that adds nothing, so fetching often is harmless.
        let new = booked_refs(&doc).difference(&stored_refs(repo, config, name, &dir)?).count();
        let unreferenced = booked_refs_missing(&doc);
        // ...unless the last stored fetch is old: storing one now and then
        // records how far the data reaches, even for a quiet account.
        let last_fetch = fetches().filter_map(|f| f.exported).max();
        let recent = last_fetch.is_some_and(|d| konter_engine::transfers::days_between(today(), d) <= 30);
        if new == 0 && unreferenced == 0 && recent {
            writeln!(Stdout, "  nothing new")?;
            fetched_any = true;
            continue;
        }
        writeln!(Stdout, "  {} new transactions", new + unreferenced)?;
        let rel = format!("{dir}/eb-{}.json", now.strftime("%Y-%m-%dT%H-%M-%S"));
        let text = fetch::to_raw_json(&fetch::strip_nulls(doc));
        let lock = manifest::lock(repo)?;
        let mut manifest = Manifest::load(&repo.manifest())?;
        add_raw_file(repo, &rel, text.as_bytes())?;
        manifest.entries.insert(rel.clone(), sha256_hex(text.as_bytes()));
        manifest.save(&repo.manifest())?;
        drop(lock);
        writeln!(Stdout, "  wrote data/{rel}")?;
        load::source(repo, config, name, &mut findings)?;
        fetched_any = true;
    }
    if !fetched_any && !findings.has_errors() {
        findings.warn("fetch", "nothing to fetch: no source has \"enable-banking\" in its formats");
    }
    write!(Stdout, "\n{}", findings.render(false))?;
    Ok(exit_code(&findings, false))
}

/// Booked transactions in a fetch, by bank reference.
fn booked_transactions(doc: &serde_json::Value) -> impl Iterator<Item = &serde_json::Value> {
    doc["pages"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|p| p["transactions"].as_array().into_iter().flatten())
        .filter(|t| t["status"].as_str().is_none_or(|s| s == "BOOK"))
}

fn booked_refs(doc: &serde_json::Value) -> std::collections::BTreeSet<String> {
    booked_transactions(doc)
        .filter_map(|t| t["entry_reference"].as_str())
        .filter(|r| *r != "0")
        .map(str::to_string)
        .collect()
}

/// Booked transactions without a usable reference: can't tell if they're new.
fn booked_refs_missing(doc: &serde_json::Value) -> usize {
    booked_transactions(doc).filter(|t| t["entry_reference"].as_str().is_none_or(|r| r == "0")).count()
}

/// Bank references in the source's earlier fetches.
fn stored_refs(repo: &Repo, config: &Config, name: &str, dir: &str) -> Result<std::collections::BTreeSet<String>> {
    let mut refs = std::collections::BTreeSet::new();
    for rel in konter_repo::manifest::raw_files(repo)? {
        if rel.starts_with(&format!("{dir}/")) && rel.ends_with(".json") {
            let file = load::raw_file(repo, config, name, &rel)?;
            refs.extend(file.rows.into_iter().filter_map(|t| t.bank_ref));
        }
    }
    Ok(refs)
}
