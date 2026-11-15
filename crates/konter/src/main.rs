use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use rust_decimal::Decimal;

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
use konter_repo::load;
use konter_repo::manifest::{self, Manifest};
use konter_repo::repo::Repo;
use konter_report::site::{self, Page};

#[derive(Parser)]
#[command(name = "konter", about = "Turns raw bank data into hledger books using a rules file")]
struct Cli {
    /// Repository root (default: nearest parent directory with config/sources.toml).
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Rebuild ledger/generated.journal and report what needs attention.
    Build {
        /// The date the books are built for (default: today).
        #[arg(long)]
        as_of: Option<jiff::civil::Date>,
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
        as_of: Option<jiff::civil::Date>,
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
        from: Option<jiff::civil::Date>,
        /// Only transactions booked on or before this date.
        #[arg(long)]
        to: Option<jiff::civil::Date>,
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
    Link {
        /// Bank name as in [banks.<name>] in config/sources.toml, e.g. nordea.
        bank: String,
        /// Requested consent length in days (capped by the bank's maximum).
        #[arg(long, default_value_t = 180)]
        days: u64,
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
        from: Option<jiff::civil::Date>,
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
        balance_date: Option<jiff::civil::Date>,
    },
}

fn main() -> ExitCode {
    // Exit quietly when piped into e.g. `head`, instead of panicking.
    #[cfg(unix)]
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e:#}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode> {
    let cli = Cli::parse();
    let repo = Repo::discover(cli.root.as_deref().unwrap_or(Path::new(".")))?;
    let config = load::config(&repo)?;
    match cli.command {
        Command::Build { as_of, full, strict } => {
            let as_of = as_of.unwrap_or_else(|| jiff::Zoned::now().date());
            let mut build = books::build(&repo, &config, as_of)?;
            books::write(&repo, &build)?;
            consent_warnings(&config, &mut build.findings);
            println!("wrote ledger/{} (--as-of {as_of})", books::GENERATED);
            let dir = site_dir(&repo);
            match write_site(&repo, &config, &build) {
                Ok(()) => println!("wrote the site in {}/\n", repo.rel(&dir)),
                Err(e) => {
                    println!();
                    build.findings.warn("report", format!("couldn't write the site in {}/: {e:#}", repo.rel(&dir)));
                }
            }
            print!("{}", build.findings.render(full));
            Ok(exit_code(&build.findings, strict))
        }
        Command::Report => {
            let journal = std::fs::read_to_string(repo.ledger(books::GENERATED)).unwrap_or_default();
            let as_of = konter_engine::journal::as_of_from_header(&journal)
                .context("ledger/generated.journal has no `--as-of` header; run `konter build` first")?;
            let build = books::build(&repo, &config, as_of)?;
            write_site(&repo, &config, &build)?;
            let dir = site_dir(&repo);
            println!("wrote the site in {}/  (open file://{})", repo.rel(&dir), dir.join(Page::Home.file()).display());
            Ok(ExitCode::SUCCESS)
        }
        Command::Verify { full, strict } => {
            let mut findings = books::verify(&repo, &config)?;
            // An independent check of the journals as written.
            books::hledger_check(&repo, &mut findings);
            print!("{}", findings.render(full));
            Ok(exit_code(&findings, strict))
        }
        Command::Subscriptions { as_of } => {
            let as_of = as_of.unwrap_or_else(|| jiff::Zoned::now().date());
            let build = books::build(&repo, &config, as_of)?;
            println!(
                "{:<24} {:<8} {:>10} {:>11}  {:<28} {:<10}  {:<10}  {:<10}",
                "subscription", "kind", "price", "balance", "status", "paid until", "next due", "committed"
            );
            for (sub, state) in build.rules.subscriptions.iter().zip(&build.subscriptions) {
                let status = match (state.oldest_unpaid, state.credit) {
                    (Some(d), c) => format!("{} owed since {d}", fmt_num(-c)),
                    (None, c) if c > rust_decimal::Decimal::ZERO => format!("{} paid ahead", fmt_num(c)),
                    _ => "ok".into(),
                };
                let opt = |d: Option<jiff::civil::Date>| d.map(|d| d.to_string()).unwrap_or_else(|| "-".into());
                println!(
                    "{:<24} {:<8} {:>10} {:>11}  {:<28} {:<10}  {:<10}  {:<10}",
                    sub.name,
                    sub.kind_name(),
                    if sub.variable { format!("~{}", fmt_num(state.price)) } else { fmt_num(state.price) },
                    fmt_num(state.credit),
                    status,
                    opt(state.paid_until),
                    opt(state.next_due),
                    opt(state.committed_until)
                );
            }
            println!("\nbalance: paid minus due as of {as_of}; negative means owed.");
            println!("paid until: the last due date the payments cover, oldest first.");
            if build.rules.subscriptions.iter().any(|s| s.variable) {
                println!("~price: an estimate; each payment settles one due date, which books what was paid.");
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Suggest { source, min } => {
            let build = books::build(&repo, &config, jiff::Zoned::now().date())?;
            let categories = reference_categories(&repo);
            let (text, groups) = konter_diagnostics::suggest::suggest(&build, &categories, source.as_deref(), min);
            print!("{text}");
            eprintln!(
                "{groups} suggested rules; fill in `account` (or use another output) and paste into the source's file in config/rules/"
            );
            Ok(ExitCode::SUCCESS)
        }
        Command::Gaps { sources, skip, from, to, top } => {
            for name in sources.iter().chain(&skip) {
                if !config.sources.get(name).is_some_and(|s| s.imported()) {
                    bail!("{name} is not an imported source");
                }
            }
            let build = books::build(&repo, &config, jiff::Zoned::now().date())?;
            let filter = konter_diagnostics::gaps::Filter { sources, skip, from, to };
            let gaps = konter_diagnostics::gaps::gaps(&config, &build.loaded, &build.generated, &filter);
            print!("{}", gaps.render(&config, &reference_categories(&repo), top));
            Ok(ExitCode::SUCCESS)
        }
        Command::Digest { json } => {
            let mut build = books::build(&repo, &config, jiff::Zoned::now().date())?;
            consent_warnings(&config, &mut build.findings);
            let digest = konter_diagnostics::digest::digest(&build);
            if json {
                println!("{}", serde_json::to_string_pretty(&digest.json())?);
            } else {
                print!("{}", digest.text());
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Coverage { source } => {
            let build = books::build(&repo, &config, jiff::Zoned::now().date())?;
            print!("{}", konter_diagnostics::coverage::coverage(&build, source.as_deref()));
            Ok(ExitCode::SUCCESS)
        }
        Command::Unmatched { source } => {
            let build = books::build(&repo, &config, jiff::Zoned::now().date())?;
            let (mut count, mut later) = (0, 0);
            for t in build.loaded.transactions() {
                if source.as_ref().is_some_and(|s| *s != t.source) {
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
                println!("# {}  {}  {}  {}", t.source, t.booked, fmt_num(t.amount), quoted_description(t));
                println!("[pin.\"{}\"]", t.id);
                println!("account = \"\"  # was {}\n", fallback_account(&t.source, t.amount));
            }
            match later {
                0 => eprintln!("{count} unmatched transactions"),
                _ => eprintln!("{count} unmatched transactions, and {later} more marked later (left out)"),
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::List { source } => {
            let mut findings = Findings::default();
            let loaded = load::all_sources(&repo, &config, &mut findings)?;
            for (name, data) in &loaded.sources {
                if source.as_ref().is_some_and(|s| s != name) {
                    continue;
                }
                for t in &data.txs {
                    println!(
                        "{}  {}  {:>12}  {:>12}  {}",
                        t.id,
                        t.booked,
                        fmt_num(t.amount),
                        t.balance_after.map(fmt_num).unwrap_or_default(),
                        name_instead(t)
                            .map_or_else(|| t.description.clone(), |name| format!("{} [{name}]", t.description))
                    );
                }
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Explain { id } => {
            let build = books::build(&repo, &config, jiff::Zoned::now().date())?;
            let t = build.loaded.find(&id).with_context(|| format!("no bank transaction with ID {id}"))?;
            print_transaction(t);
            let outcome = &build.generated.outcomes[&id];
            println!("\nrules:");
            if let Some(pin) = build.rules.pins.get(&id) {
                println!("  pin in {} (always wins)", pin.file);
            }
            if let Some(later) = build.rules.later.get(&id) {
                let note = later.note.as_deref().map(|n| format!(": {n}")).unwrap_or_default();
                println!("  marked later in {}{note}", later.file);
            }
            if outcome.candidates.is_empty() {
                println!("  no pattern rule matches");
            }
            for (name, priority) in &outcome.candidates {
                let file = build.rules.patterns.iter().find(|r| &r.name == name).map_or("?", |r| r.file.as_str());
                println!("  rule {name:?} in {file} matches (priority {priority})");
            }
            println!("  → {:?}", outcome.status);
            println!("\njournal output:");
            if outcome.ledger.is_empty() {
                println!("  (none: {})", outcome.ignored.as_deref().unwrap_or("ignored"));
            }
            for &i in &outcome.ledger {
                let mut text = String::new();
                render_transaction(&mut text, &build.generated.txs[i]);
                println!("{text}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::AddExport { file, source, name, balance, balance_date } => {
            let balance = balance.map(|b| konter_engine::money::parse_plain(&b)).transpose()?;
            let snapshot = balance.map(|b| (balance_date.unwrap_or_else(|| jiff::Zoned::now().date()), b));
            add_export(&repo, &config, &file, source, name, snapshot)
        }
        Command::Banks { country } => {
            let client = fetch::Client::new(&fetch::Settings::load()?)?;
            for a in client.aspsps(&country)? {
                let days = a.get("maximum_consent_validity").and_then(|v| v.as_u64()).map(|s| s / 86_400);
                println!(
                    "{:<40} max consent {} days",
                    a.get("name").and_then(|v| v.as_str()).unwrap_or("?"),
                    days.map(|d| d.to_string()).unwrap_or_else(|| "?".into())
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Link { bank, days } => link(&config, &bank, days),
        Command::Session { bank } => {
            let sessions = fetch::Sessions::load()?;
            let session = sessions.banks.get(&bank).with_context(|| format!("{bank} isn't linked"))?;
            let client = fetch::Client::new(&fetch::Settings::load()?)?;
            let mut info = client.get(&format!("/sessions/{}", session.session_id), &[])?;
            if let Some(obj) = info.as_object_mut() {
                obj.remove("session_id");
            }
            println!("{}", serde_json::to_string_pretty(&info)?);
            for account in &session.accounts {
                let details = client.get(&format!("/accounts/{}/details", account.uid), &[])?;
                let field = |k: &str| details.get(k).and_then(|v| v.as_str()).unwrap_or("-").to_string();
                println!(
                    "{:<20} name: {}  product: {}  details: {}",
                    account.iban.as_deref().unwrap_or("?"),
                    field("name"),
                    field("product"),
                    field("details")
                );
            }
            Ok(ExitCode::SUCCESS)
        }
        Command::Fetch { source, from } => fetch_all(&repo, &config, source, from),
    }
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
fn reference_categories(repo: &Repo) -> std::collections::BTreeMap<(jiff::civil::Date, Decimal), String> {
    load::reference_export(repo).map(|text| konter_diagnostics::suggest::reference_categories(&text)).unwrap_or_default()
}

fn exit_code(findings: &Findings, strict: bool) -> ExitCode {
    let failed = findings.has_errors() || (strict && findings.count(Severity::Warning) > 0);
    if failed { ExitCode::FAILURE } else { ExitCode::SUCCESS }
}

fn print_transaction(t: &BankTransaction) {
    println!("{}  ({})", t.id, t.source);
    println!("  booked        {}", t.booked);
    let opt = |label: &str, v: Option<String>| {
        if let Some(v) = v {
            println!("  {label:<13} {v}");
        }
    };
    opt("time", t.time.map(|x| x.strftime("%H:%M").to_string()));
    opt("value date", t.value_date.map(|d| d.to_string()));
    opt("purchased", t.transaction_date.map(|d| d.to_string()));
    println!("  amount        {} {}", fmt_num(t.amount), t.currency);
    opt(
        "original",
        t.original_amount.map(|a| format!("{} {}", fmt_num(a), t.original_currency.clone().unwrap_or_default())),
    );
    println!("  description   {:?}", t.description);
    opt("counterparty", t.counterparty.clone());
    opt("cp. name", t.counterparty_name.clone());
    opt("balance after", t.balance_after.map(fmt_num));
    opt("bank ref", t.bank_ref.clone());
    println!("  raw ({}):", t.format);
    for (k, v) in &t.raw {
        println!("    {k:<15} {v:?}");
    }
    println!("  found in:");
    for o in &t.origins {
        println!("    data/{}:{}", o.file, o.line);
    }
}

fn add_export(
    repo: &Repo,
    config: &Config,
    file: &Path,
    source: Option<String>,
    name: Option<String>,
    snapshot: Option<(jiff::civil::Date, rust_decimal::Decimal)>,
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
        println!("data/{rel} is already in place");
    } else {
        add_raw_file(repo, &rel, &bytes)?;
        println!("copied to data/{rel}");
    }
    manifest.entries.insert(rel.clone(), sha256_hex(&bytes));
    if let Some((sidecar, text)) = &sidecar {
        if sidecar_in_place {
            println!("data/{sidecar} is already in place");
        } else {
            add_raw_file(repo, sidecar, text.as_bytes())?;
            println!("wrote data/{sidecar}");
        }
        manifest.entries.insert(sidecar.clone(), sha256_hex(text.as_bytes()));
    }
    manifest.save(&repo.manifest())?;
    drop(lock);
    println!("recorded in data/MANIFEST\n");

    let mut findings = Findings::default();
    load::source(repo, config, &source_name, &mut findings)?;
    print!("{}", findings.render(false));
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
            findings.warn("consent", format!("Enable Banking consent for {bank} has expired; run `konter link {bank}`"));
        } else if days < 14 {
            findings.warn(
                "consent",
                format!("Enable Banking consent for {bank} expires in {days} days; run `konter link {bank}`"),
            );
        }
    }
}

fn link(config: &Config, bank: &str, days: u64) -> Result<ExitCode> {
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
    println!("1. Open this URL and log in with MitID:\n\n   {url}\n");
    println!("2. You'll end up on {} (the page may fail to load; that's fine).", settings.redirect_url);
    println!("   Paste the full URL from the address bar here and press Enter:");
    let mut pasted = String::new();
    std::io::stdin().read_line(&mut pasted)?;
    let code = fetch::parse_redirect(&pasted, &state)?;
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
            .unwrap_or(&valid_until)
            .to_string(),
        accounts: accounts.clone(),
    };
    let mut sessions = fetch::Sessions::load()?;
    println!("\nLinked {bank} until {}. Accounts:", stored.valid_until);
    sessions.banks.insert(bank.to_string(), stored);
    sessions.save()?;
    for a in &accounts {
        let iban = a.iban.as_deref().unwrap_or("?");
        let source = source_for_iban(config, bank, iban).unwrap_or("not in config/sources.toml; add it to import it");
        println!("  {iban:<24} {source}");
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

fn fetch_all(repo: &Repo, config: &Config, only: Option<String>, from: Option<jiff::civil::Date>) -> Result<ExitCode> {
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
        // Overlap the previous fetch by 30 days, so a re-fetch can be checked.
        let date_from = match from {
            Some(d) => d,
            None => {
                let mut quiet = Findings::default();
                let existing = load::source(repo, config, name, &mut quiet)?;
                // The last transaction in earlier fetches (their rows may be
                // represented by CSV copies after merging, so look at the files).
                existing
                    .files
                    .iter()
                    .filter(|f| f.rel.ends_with(".json"))
                    .filter_map(|f| f.last)
                    .max()
                    .map(|d| d.saturating_sub(jiff::Span::new().days(30)))
                    .unwrap_or_else(|| config.source_start(source))
            }
        };
        println!("{name}: fetching from {date_from} …");
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
        let mut quiet = Findings::default();
        let last_fetch = load::source(repo, config, name, &mut quiet)?
            .files
            .iter()
            .filter(|f| f.rel.ends_with(".json"))
            .filter_map(|f| f.exported)
            .max();
        let recent =
            last_fetch.is_some_and(|d| konter_engine::transfers::days_between(jiff::Zoned::now().date(), d) <= 30);
        if new == 0 && unreferenced == 0 && recent {
            println!("  nothing new");
            fetched_any = true;
            continue;
        }
        println!("  {} new transactions", new + unreferenced);
        let rel = format!("{dir}/eb-{}.json", now.strftime("%Y-%m-%dT%H-%M-%S"));
        let text = fetch::to_raw_json(&fetch::strip_nulls(doc));
        let lock = manifest::lock(repo)?;
        let mut manifest = Manifest::load(&repo.manifest())?;
        add_raw_file(repo, &rel, text.as_bytes())?;
        manifest.entries.insert(rel.clone(), sha256_hex(text.as_bytes()));
        manifest.save(&repo.manifest())?;
        drop(lock);
        println!("  wrote data/{rel}");
        load::source(repo, config, name, &mut findings)?;
        fetched_any = true;
    }
    if !fetched_any && !findings.has_errors() {
        findings.warn("fetch", "nothing to fetch: no source has \"enable-banking\" in its formats");
    }
    print!("\n{}", findings.render(false));
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
