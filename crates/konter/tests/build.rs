//! End-to-end tests on the fixture repository in tests/fixtures/basic.

use std::path::Path;

use jiff::civil::date;
use konter_engine::config::Config;
use konter_engine::engine::{self, Status};
use konter_engine::findings::Findings;
use konter_engine::rules::Rules;
use konter_repo::repo::Repo;
use konter_repo::{books, load};

fn fixture(name: &str) -> (Repo, Config) {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name);
    let repo = Repo::discover(&root).unwrap();
    let config = load::config(&repo).unwrap();
    (repo, config)
}

#[test]
fn basic_build_snapshot() {
    let (repo, config) = fixture("basic");
    let build = books::build(&repo, &config, date(2026, 2, 15)).unwrap();
    insta::assert_snapshot!("basic_journal", build.journal);
    insta::assert_snapshot!("basic_report", build.findings.render(true));
    insta::assert_snapshot!("basic_forecast", build.forecast);
}

#[test]
fn subscription_state() {
    let (repo, config) = fixture("basic");
    let build = books::build(&repo, &config, date(2026, 2, 15)).unwrap();
    let state = &build.subscriptions[0];
    // Due 2026-01-02 and 2026-02-02, paid once.
    assert_eq!(state.credit, rust_decimal::Decimal::from(-10000));
    assert_eq!(state.oldest_unpaid, Some(date(2026, 2, 2)));
    assert_eq!(state.next_due, Some(date(2026, 3, 2)));
    assert_eq!(state.committed_until, Some(date(2026, 3, 15)));
    assert!(build.findings.render(true).contains("salary: 10000.00 DKK owed, unpaid since 2026-02-02"));
    // Within the grace period there's no deficit yet.
    let early = books::build(&repo, &config, date(2026, 2, 4)).unwrap();
    assert!(!early.findings.render(true).contains("deficit"));
}

#[test]
fn builds_are_deterministic() {
    let (repo, config) = fixture("basic");
    let a = books::build(&repo, &config, date(2026, 2, 15)).unwrap();
    let b = books::build(&repo, &config, date(2026, 2, 15)).unwrap();
    assert_eq!(a.journal, b.journal);
    assert_eq!(a.findings.render(true), b.findings.render(true));
}

/// Applies rules given as text to the fixture's bank data.
fn apply(rules: &str) -> (engine::Generated, Findings) {
    let (repo, config) = fixture("basic");
    let rules = Rules::load_str(rules, "test.toml", &config).unwrap();
    let mut findings = Findings::default();
    let loaded = load::all_sources(&repo, &config, &mut findings).unwrap();
    let generated = engine::generate(&config, &rules, &loaded, &mut findings);
    (generated, findings)
}

#[test]
fn ignoring_one_side_only_is_an_error() {
    let (_, findings) = apply(
        r#"
        [pin."ba-2026-01-15-64ffb9"]
        ignore = "only the charge"
        "#,
    );
    let text = findings.render(true);
    assert!(text.contains("ignored transactions must cancel out"), "{text}");
}

#[test]
fn split_that_doesnt_add_up_is_reported_and_falls_back() {
    let (generated, findings) = apply(
        r#"
        [pin."ba-2026-01-30-3d3806"]
        split = [{ account = "expenses:concerts", amount = 600 }]
        "#,
    );
    let outcome = &generated.outcomes["ba-2026-01-30-3d3806"];
    assert!(matches!(outcome.status, Status::Invalid { .. }), "{:?}", outcome.status);
    let tx = &generated.txs[outcome.ledger[0]];
    assert_eq!(tx.postings[1].account, "expenses:unmatched:bank:a");
    assert!(findings.render(true).contains("split amounts add up to 600.00, but must add up to 1200.00"));
}

#[test]
fn priority_resolves_overlap() {
    let (generated, findings) = apply(
        r#"
        [[rule]]
        name = "coffee"
        when = { description_re = '(?i)coffee' }
        account = "expenses:coffee"

        [[rule]]
        name = "coffee-lunar"
        when = { source = "lunar:b", description = "Coffee Shop" }
        priority = 1
        account = "expenses:coffee"
        "#,
    );
    assert_eq!(generated.outcomes["lb-2026-01-10-b23a52"].status, Status::Rule("coffee-lunar".into()));
    assert_eq!(generated.outcomes["ba-2026-01-02-ca8e9e"].status, Status::Rule("coffee".into()));
    assert!(!findings.render(true).contains("ambiguous"));
}

#[test]
fn malformed_rules_are_rejected_with_a_reason() {
    let (_, config) = fixture("basic");
    let cases = [
        ("[[rule]]\nname = \"x\"\nwhen = { descripton = \"a\" }\naccount = \"a\"", "descripton"),
        ("[[rule]]\nname = \"x\"\nwhen = {}\naccount = \"a\"", "no conditions"),
        ("[[rule]]\nname = \"x\"\nwhen = { description = \"a\" }\naccount = \"a\"\nunknown = \"b\"", "exactly one"),
        ("[[rule]]\nname = \"X Y\"\nwhen = { description = \"a\" }\naccount = \"a\"", "lowercase"),
        ("[pin.\"not-an-id\"]\naccount = \"a\"", "not a transaction ID"),
        ("[[rule]]\nname = \"x\"\nwhen = { source = \"nope:x\" }\naccount = \"a\"", "not an imported source"),
        ("[[rule]]\nname = \"x\"\nwhen = { description = \"a\" }\ntransfer = \"nope:x\"", "transfer target"),
        ("[[rules]]\nname = \"x\"", "rules"),
    ];
    for (text, expected) in cases {
        let err = format!("{:#}", Rules::load_str(text, "test.toml", &config).unwrap_err());
        assert!(err.contains(expected), "expected {expected:?} in error for:\n{text}\ngot: {err}");
    }
}

#[test]
fn rules_are_read_from_rules_toml_and_every_toml_under_rules() {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("rules-files");
    let _ = std::fs::remove_dir_all(&root);
    for file in [
        "config/sources.toml",
        "config/rules.toml",
        "config/rules/b.toml",
        "config/rules/a/c.toml",
        "config/rules/README.md",
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }
    let repo = Repo::discover(&root).unwrap();
    let files: Vec<String> = repo.rules_files().unwrap().iter().map(|p| repo.rel(p)).collect();
    assert_eq!(files, ["config/rules.toml", "config/rules/a/c.toml", "config/rules/b.toml"]);
}

#[test]
fn duplicates_across_rules_files_name_both_files() {
    let (_, config) = fixture("basic");
    let rule = "[[rule]]\nname = \"x\"\nwhen = { description = \"a\" }\naccount = \"a\"";
    let pin = "[pin.\"ba-2026-01-02-ca8e9e\"]\naccount = \"a\"";
    let sub = "[[subscription]]\nname = \"s\"\nkind = \"expense\"\naccount = \"a\"\nevery = \"1 month\"\nfirst = 2026-01-01\nprices = [{ from = 2026-01-01, amount = 1 }]";
    for text in [rule, pin, sub] {
        let err = format!("{:#}", Rules::from_files([("one.toml", text), ("two.toml", text)], &config).unwrap_err());
        assert!(err.contains("defined in both one.toml and two.toml"), "{err}");
        // A pin twice in one file is already a TOML error (the table is defined twice).
        if text != pin {
            let twice = format!("{text}\n\n{text}");
            let err = format!("{:#}", Rules::from_files([("one.toml", twice.as_str())], &config).unwrap_err());
            assert!(err.contains("defined twice in one.toml"), "{err}");
        }
    }
}

#[test]
fn enable_banking_with_cutover_and_snapshots() {
    let (repo, config) = fixture("eb");
    let build = books::build(&repo, &config, date(2026, 2, 5)).unwrap();
    let text = build.findings.render(true);
    assert!(!text.contains("overlap conflict"), "{text}");
    assert!(!text.contains("balance check"), "{text}");
    let a = &build.loaded.sources["bank:a"];
    // CSV before the cutover keeps the same IDs as with CSV only.
    assert_eq!(a.txs[0].id, "ba-2026-01-02-ca8e9e");
    assert_eq!(a.txs[0].format, konter_engine::config::Format::NordeaCsv);
    // From the cutover on, the API rows are used (pending ones skipped).
    let api: Vec<_> = a.txs.iter().filter(|t| t.format == konter_engine::config::Format::EnableBanking).collect();
    assert_eq!(api.len(), 4);
    assert!(api.iter().all(|t| t.booked >= date(2026, 1, 20)));
    assert_eq!(api[1].counterparty.as_deref(), Some("DK0000000000000555"));
    // Opening balances worked back from the bank's reported balance.
    assert_eq!(a.opening, Some(rust_decimal::Decimal::from(1050)));
    assert_eq!(build.loaded.sources["other:c"].opening, Some(rust_decimal::Decimal::from(500)));
    assert!(build.journal.contains("assets:bank:a                                 0.00 DKK = 17375.00 DKK"));
    // Foreign amounts stay in DKK, with the original kept as a tag.
    assert!(build.journal.contains("original:-8.05 EUR"), "{}", build.journal);
}

#[test]
fn mixing_formats_without_cutover_is_reported() {
    let (repo, mut config) = fixture("eb");
    config.sources.get_mut("bank:a").unwrap().cutover = None;
    let build = books::build(&repo, &config, date(2026, 2, 5)).unwrap();
    let text = build.findings.render(true);
    assert!(text.contains("overlap conflict") && text.contains("Set `cutover`"), "{text}");
}

#[test]
fn suggestions_are_valid_rules_once_filled_in() {
    let (repo, config) = fixture("eb");
    let build = books::build(&repo, &config, date(2026, 2, 5)).unwrap();
    let (text, groups) = konter_diagnostics::suggest::suggest(&build, &Default::default(), Some("bank:a"), 2);
    assert_eq!(groups, 1, "{text}");
    assert!(text.contains("name = \"salary\"") && text.contains("description = 'SALARY'"), "{text}");
    let filled = text.replace("account = \"\"", "account = \"income:salary\"");
    let rules = Rules::load_str(&filled, "suggested.toml", &config).unwrap();
    assert_eq!(rules.patterns.len(), 1);
}

/// Applies the fixture's rules with a modified config.
fn apply_with(config: &Config) -> (engine::Generated, Findings) {
    let (repo, _) = fixture("basic");
    let rules = load::rules(&repo, config).unwrap();
    let mut findings = Findings::default();
    let loaded = load::all_sources(&repo, config, &mut findings).unwrap();
    let generated = engine::generate(config, &rules, &loaded, &mut findings);
    (generated, findings)
}

#[test]
fn transfers_before_an_accounts_start_go_to_before_start() {
    let (_, mut config) = fixture("basic");
    // lunar:b only counts from February: both its transactions are left out.
    config.sources.get_mut("lunar:b").unwrap().start = Some(konter_engine::dates::ConfigDate(date(2026, 2, 1)));

    // Without before_start, the top-up to it on 2026-01-28 is an error.
    let (generated, findings) = apply_with(&config);
    let top_up = &generated.outcomes["ba-2026-01-28-2299c7"];
    assert!(matches!(top_up.status, Status::Invalid { .. }), "{:?}", top_up.status);
    assert!(findings.render(true).contains("set `before_start`"));

    // With it, the top-up is booked there and nothing is left unpaired.
    config.sources.get_mut("lunar:b").unwrap().before_start = Some("expenses:side-project".into());
    let (generated, findings) = apply_with(&config);
    let top_up = &generated.outcomes["ba-2026-01-28-2299c7"];
    assert_eq!(top_up.status, Status::Rule("top-up".into()));
    let tx = &generated.txs[top_up.ledger[0]];
    assert_eq!(tx.postings[1].account, "expenses:side-project");
    let text = findings.render(true);
    assert!(!text.contains("unpaired"), "{text}");
    assert!(
        generated.outcomes.keys().all(|id| !id.starts_with("lb-")),
        "lunar:b transactions before its start are left out"
    );
}

#[test]
fn gaps_rank_by_money_and_spot_own_transfers() {
    use konter_diagnostics::gaps::{self, Filter};
    let (repo, config) = fixture("basic");
    // Only coffee is mapped, so nearly everything shows up as a gap.
    let (generated, _) = apply(
        r#"
        [[rule]]
        name = "coffee"
        when = { description_re = '(?i)coffee' }
        account = "expenses:coffee"
        "#,
    );
    let loaded = load::all_sources(&repo, &config, &mut Findings::default()).unwrap();
    let all = gaps::gaps(&config, &loaded, &generated, &Filter::default());
    let order: Vec<(&str, &str, String)> =
        all.groups.iter().map(|g| (g.source, g.txs[0].description.as_str(), g.flow.gross().to_string())).collect();
    let expected = [
        ("bank:a", "SALARY", "10000.00"),
        ("bank:a", "Concert tickets", "1200.00"),
        ("bank:a", "Savings", "1000.00"),
        ("bank:a", "Insurance", "900.00"),
        ("bank:a", "Top up", "500.00"),
        ("lunar:b", "Overførsel", "500.00"),
        ("bank:a", "Charge", "100.00"),
        ("bank:a", "Refund", "100.00"),
        ("bank:a", "Mystery", "25.00"),
    ];
    let expected: Vec<(&str, &str, String)> = expected.iter().map(|&(s, d, a)| (s, d, a.to_string())).collect();
    assert_eq!(order, expected);
    // The top-up and its other side are unmapped on both sides: a likely transfer.
    // Charge and refund cancel out, but within one account, so they aren't.
    assert_eq!(all.groups[4].transfers.get("lunar:b"), Some(&1));
    assert_eq!(all.groups[5].transfers.get("bank:a"), Some(&1));
    assert!(all.groups[6].transfers.is_empty());
    assert_eq!(all.transfers.gross(), rust_decimal::Decimal::from(1000));
    let text = all.render(&config, &Default::default(), 3);
    assert!(text.contains("… and 6 smaller groups (6 transactions, 2,125 or 15% of the unmapped money)"), "{text}");
    // Only the two coffee purchases (80 of 14,405) are mapped; mapping the
    // salary next would bring it to 10,080 of 14,405.
    assert!(
        text.contains("0.5% of income and spending is mapped: 80 of 14,405. Unmapped: 14,325 in 9 of 11 transactions."),
        "{text}"
    );
    let salary = text.lines().find(|l| l.contains("SALARY")).unwrap();
    assert!(salary.contains("69.9%"), "{salary}");
    // The page has the same numbers, and every group rather than the top ones.
    let html = all.html(&config, &Default::default());
    assert!(html.contains("<strong>0.5%</strong> of income and spending is mapped: 80 of 14,405 DKK."), "{html}");
    assert_eq!(html.matches("<td class=\"id\">").count(), all.groups.len());
    let salary = html.lines().find(|l| l.contains("SALARY")).unwrap();
    assert!(salary.contains("69.9%"), "{salary}");

    // Filtering keeps the hint about the other side, which is filtered out.
    let filter = Filter { skip: vec!["bank:a".into()], ..Filter::default() };
    let lunar = gaps::gaps(&config, &loaded, &generated, &filter);
    assert_eq!(lunar.groups.len(), 1);
    assert!(lunar.render(&config, &Default::default(), 0).contains("↔ bank:a"));
    let filter = Filter { from: Some(date(2026, 1, 20)), ..Filter::default() };
    let late = gaps::gaps(&config, &loaded, &generated, &filter);
    assert!(late.groups.iter().all(|g| g.txs.iter().all(|t| t.booked >= date(2026, 1, 20))));
    assert_eq!(late.groups.len(), 4);
}

#[test]
fn gaps_count_income_and_spending_only() {
    use konter_diagnostics::gaps::{self, Filter};
    let (repo, config) = fixture("basic");
    let build = books::build(&repo, &config, date(2026, 2, 15)).unwrap();
    let findings = gaps::gaps(&config, &build.loaded, &build.generated, &Filter::default());
    let bank = findings.sources.iter().find(|s| s.name == "bank:a").unwrap();
    // Coffee 50, the salary subscription 10,000, the concert's own half 600
    // and the prepaid insurance once expensed 900. The transfers, the
    // receivable and the ignored charge and refund aren't income or spending.
    assert_eq!(bank.mapped.gross(), rust_decimal::Decimal::from(11550));
    assert_eq!(bank.unmapped.gross(), rust_decimal::Decimal::from(25));
    let text = findings.render(&config, &Default::default(), 0);
    // The ambiguous Lunar coffee (30) is unmapped too.
    assert!(
        text.contains(
            "99.5% of income and spending is mapped: 11,550 of 11,605. Unmapped: 55 in 2 of 11 transactions."
        ),
        "{text}"
    );
}

/// The fixture's rules plus `extra`, through the engine and the subscriptions.
fn subscriptions_with(
    extra: &str,
    as_of: jiff::civil::Date,
) -> Result<(Config, Rules, engine::Generated, Vec<konter_engine::subscriptions::State>, Findings), String> {
    let (repo, config) = fixture("basic");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/config/rules.toml");
    let text = format!("{}\n{extra}", std::fs::read_to_string(path).unwrap());
    let rules = Rules::load_str(&text, "test.toml", &config).map_err(|e| format!("{e:#}"))?;
    let mut findings = Findings::default();
    let loaded = load::all_sources(&repo, &config, &mut findings).unwrap();
    let mut generated = engine::generate(&config, &rules, &loaded, &mut findings);
    let states =
        konter_engine::subscriptions::generate(&config, &rules.subscriptions, as_of, &mut generated, &mut findings);
    Ok((config, rules, generated, states, findings))
}

/// The state of the subscription called `name`.
fn state<'a>(states: &'a [konter_engine::subscriptions::State], name: &str) -> &'a konter_engine::subscriptions::State {
    states.iter().find(|s| s.name == name).unwrap()
}

#[test]
fn transfer_subscriptions_are_paid_by_transfers_of_any_size() {
    // The fixture moves 500 to lunar:b on 2026-01-28 and 1000 to bank:savings
    // (which has no data) on 2026-01-05.
    let subs = r#"
[[subscription]]
name = "pocket-money"
kind = "transfer"
from = "bank:a"
to = "lunar:b"
every = "1 month"
first = 2026-01-15
prices = [{ from = 2026-01-01, amount = 100 }]

[[subscription]]
name = "saving"
kind = "transfer"
from = "bank:a"
to = "bank:savings"
every = "1 month"
first = 2026-01-31
prices = [{ from = 2026-01-01, amount = 1000 }]
"#;
    let as_of = date(2026, 2, 15);
    let (config, rules, generated, states, findings) = subscriptions_with(subs, as_of).unwrap();
    let pocket = states.iter().find(|s| s.name == "pocket-money").unwrap();
    // 500 pays the 100 due on 2026-01-15 and 2026-02-15, and three months ahead.
    assert_eq!(pocket.credit, rust_decimal::Decimal::from(300));
    assert_eq!(pocket.oldest_unpaid, None);
    assert_eq!(pocket.paid_until, Some(date(2026, 5, 15)));
    let saving = states.iter().find(|s| s.name == "saving").unwrap();
    assert_eq!(saving.credit, rust_decimal::Decimal::ZERO);
    assert_eq!(saving.paid_until, Some(date(2026, 1, 31)));

    // Paying a transfer ahead isn't a warning, and due dates book nothing.
    let text = findings.render(true);
    assert!(!text.contains("pocket-money") && !text.contains("saving:"), "{text}");
    let mine = |(k, v): &(String, String)| k == "subscription" && (v == "pocket-money" || v == "saving");
    assert!(!generated.txs.iter().any(|tx| tx.tags.iter().any(mine)));
    assert!(!generated.tool_accounts.iter().any(|a| a.ends_with("pocket-money")));

    // The paying side is tagged; the receiving side isn't.
    let tagged = |id: &str| {
        let tx = &generated.txs[generated.outcomes[id].ledger[0]];
        tx.tags.iter().filter(|(k, _)| k == "pays").map(|(_, v)| v.as_str()).collect::<Vec<_>>()
    };
    assert_eq!(tagged("ba-2026-01-28-2299c7"), ["pocket-money"]);
    let arrival = generated.outcomes.keys().find(|id| id.starts_with("lb-2026-01-29")).unwrap();
    assert!(tagged(arrival).is_empty());

    // The forecast moves the money from the first due date that isn't paid.
    let forecast = konter_engine::subscriptions::forecast(&config, &rules.subscriptions, &states, as_of);
    assert!(
        forecast.contains("~ every 1 months from 2026-06-15  pocket-money  ; pays:pocket-money\n    assets:bank:b   "),
        "{forecast}"
    );
    assert!(forecast.contains("~ every 1 months from 2026-02-28  saving  ; pays:saving"), "{forecast}");
}

#[test]
fn transfer_subscriptions_report_what_is_owed() {
    let subs = r#"
[[subscription]]
name = "pocket-money"
kind = "transfer"
from = "bank:a"
to = "lunar:b"
every = "1 month"
first = 2026-01-15
prices = [{ from = 2026-01-01, amount = 300 }]
"#;
    let (_, _, _, states, findings) = subscriptions_with(subs, date(2026, 2, 25)).unwrap();
    // Due 300 + 300, paid 500: the 2026-02-15 one is 100 short.
    assert_eq!(state(&states, "pocket-money").credit, rust_decimal::Decimal::from(-100));
    assert_eq!(state(&states, "pocket-money").paid_until, Some(date(2026, 1, 15)));
    let text = findings.render(true);
    assert!(text.contains("pocket-money: 100.00 DKK owed, unpaid since 2026-02-15"), "{text}");

    // A transfer subscription is paid by transfers, not by `subscription =`.
    let pin = "[pin.\"ba-2026-01-28-2299c7\"]\nsubscription = \"pocket-money\"\n";
    let error = subscriptions_with(&format!("{subs}\n{pin}"), date(2026, 2, 25)).err().unwrap();
    assert!(error.contains("is a transfer subscription"), "{error}");
    // Both ends must be sources.
    let error = subscriptions_with(&subs.replace("lunar:b", "lunar:nope"), date(2026, 2, 25)).err().unwrap();
    assert!(error.contains("\"lunar:nope\" is not a source"), "{error}");
}

#[test]
fn variable_subscriptions_book_what_was_paid() {
    // The fixture pays 50 on 2026-01-02 and 25 on 2026-01-20.
    let subs = r#"
[[subscription]]
name = "power"
kind = "expense"
account = "expenses:coffee"
payee = "Power bill"
every = "1 month"
first = 2026-01-01
variable = true
prices = [{ from = 2026-01-01, amount = 40 }]

[pin."ba-2026-01-02-ca8e9e"]
subscription = "power"

[pin."ba-2026-01-20-b7634b"]
subscription = "power"
"#;
    let due = |generated: &engine::Generated| {
        let mut dues: Vec<(jiff::civil::Date, rust_decimal::Decimal)> = generated
            .txs
            .iter()
            .filter(|tx| tx.description == "Power bill")
            .map(|tx| (tx.date, tx.postings[0].amount))
            .collect();
        dues.sort();
        dues
    };
    let n = rust_decimal::Decimal::from;

    // Each payment settles one due date, oldest first, and it books what was
    // paid. The second payment came before its due date.
    let (_, _, generated, states, findings) = subscriptions_with(subs, date(2026, 2, 15)).unwrap();
    assert_eq!(due(&generated), [(date(2026, 1, 1), n(50)), (date(2026, 2, 1), n(25))]);
    assert_eq!(state(&states, "power").credit, n(0));
    assert_eq!(state(&states, "power").paid_until, Some(date(2026, 2, 1)));
    assert_eq!(state(&states, "power").price, n(40));
    assert!(!findings.render(true).contains("power"), "{}", findings.render(true));

    // Paid one due date ahead: neither owed nor a warning.
    let (_, _, generated, states, findings) = subscriptions_with(subs, date(2026, 1, 25)).unwrap();
    assert_eq!(due(&generated), [(date(2026, 1, 1), n(50))]);
    assert_eq!(state(&states, "power").credit, n(25));
    assert_eq!(state(&states, "power").paid_until, Some(date(2026, 2, 1)));
    assert!(!findings.render(true).contains("power"), "{}", findings.render(true));

    // A due date nobody has paid books the estimate and is owed.
    let (_, _, generated, states, findings) = subscriptions_with(subs, date(2026, 3, 15)).unwrap();
    assert_eq!(due(&generated).last(), Some(&(date(2026, 3, 1), n(40))));
    assert_eq!(state(&states, "power").credit, n(-40));
    assert_eq!(state(&states, "power").oldest_unpaid, Some(date(2026, 3, 1)));
    let text = findings.render(true);
    assert!(text.contains("power: 40.00 DKK owed, unpaid since 2026-03-01"), "{text}");

    // Two due dates paid ahead is a possible double payment.
    let later = subs.replace("first = 2026-01-01", "first = 2026-01-25");
    let (_, _, _, _, findings) = subscriptions_with(&later, date(2026, 1, 24)).unwrap();
    let text = findings.render(true);
    assert!(text.contains("power: paid 75.00 ahead, more than one period"), "{text}");

    // Only for expenses and income, and without an opening balance.
    let error =
        subscriptions_with(&subs.replace("variable = true", "variable = true\nopening = 10"), date(2026, 2, 15));
    assert!(error.err().unwrap().contains("`opening` isn't supported"));
}

/// The fixture's own rules, plus more given as text.
fn apply_with_fixture_rules(more: &str) -> (engine::Generated, Findings) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/config/rules.toml");
    apply(&format!("{}\n{more}", std::fs::read_to_string(path).unwrap()))
}

#[test]
fn later_keeps_a_transaction_unmatched_without_counting_against_the_policy() {
    let (generated, findings) = apply_with_fixture_rules(
        r#"
        [later."ba-2026-01-20-b7634b"]
        note = "no idea yet"
        "#,
    );
    let outcome = &generated.outcomes["ba-2026-01-20-b7634b"];
    assert_eq!(outcome.status, Status::Later(Some("no idea yet".into())));
    let tx = &generated.txs[outcome.ledger[0]];
    assert_eq!(tx.postings[1].account, "expenses:unmatched:bank:a");
    assert!(tx.comments.iter().any(|c| c == "not mapped yet, marked later: no idea yet"), "{:?}", tx.comments);
    let text = findings.render(true);
    // bank:a's policy is "error"; its only unmatched transaction is marked later.
    assert!(text.contains("bank:a: all 9 transactions mapped or marked later (1 later)"), "{text}");
    assert!(!text.contains("bank:a: 1 of 9 not mapped"), "{text}");
}

#[test]
fn later_on_a_transaction_that_is_mapped_or_missing_is_reported() {
    let (generated, findings) = apply_with_fixture_rules(
        r#"
        [later."ba-2026-01-15-64ffb9"]
        [later."ba-2026-01-31-000001"]
        [later."lb-2026-01-10-b23a52"]
        "#,
    );
    let text = findings.render(true);
    // Pinned (ignored): the mark is stale, and the pin wins.
    assert_eq!(generated.outcomes["ba-2026-01-15-64ffb9"].status, Status::Pin);
    assert!(text.contains(r#"later "ba-2026-01-15-64ffb9" (test.toml): mapped by a pin; remove the mark"#), "{text}");
    // No such transaction.
    assert!(text.contains(r#"later "ba-2026-01-31-000001" (test.toml): no such transaction"#), "{text}");
    // Ambiguous stays an error: a mark can't hide a problem in the rules.
    assert!(matches!(generated.outcomes["lb-2026-01-10-b23a52"].status, Status::Ambiguous(_)));
    assert!(text.contains("1 transactions match several rules of equal priority"), "{text}");
}

#[test]
fn later_marks_are_unique_across_files() {
    let (_, config) = fixture("basic");
    let mark = r#"[later."ba-2026-01-20-b7634b"]"#;
    let err = Rules::from_files([("a.toml", mark), ("b.toml", mark)], &config).unwrap_err();
    assert!(format!("{err:#}").contains("defined in both a.toml and b.toml"), "{err:#}");
    let err = Rules::load_str(r#"[later."not-an-id"]"#, "a.toml", &config).unwrap_err();
    assert!(format!("{err:#}").contains("not a transaction ID"), "{err:#}");
}

/// A build of the fixture with its own rules plus more given as text.
fn build_with(more: &str) -> konter_engine::pipeline::Build {
    let (repo, config) = fixture("basic");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic/config/rules.toml");
    let text = format!("{}\n{more}", std::fs::read_to_string(path).unwrap());
    let rules = Rules::load_str(&text, "config/rules.toml", &config).unwrap();
    let mut findings = Findings::default();
    let loaded = load::all_sources(&repo, &config, &mut findings).unwrap();
    let manual = load::manual(&repo).unwrap();
    konter_engine::pipeline::assemble(&config, rules, loaded, &manual, date(2026, 2, 15), findings)
}

#[test]
fn digest_lists_new_transactions_and_what_needs_attention() {
    let build = build_with("");
    let digest = konter_diagnostics::digest::digest(&build);
    let ids: Vec<&str> = digest.items.iter().map(|i| i.tx.id.as_str()).collect();
    // The ambiguous Lunar coffee is an error in the rules, listed below instead.
    assert_eq!(ids, ["ba-2026-01-20-b7634b"]);
    let text = digest.text();
    assert!(text.starts_with("1 new transaction to map:\n1. 2026-01-20  -25.00 DKK  Mystery\n   bank:a\n"), "{text}");
    assert!(text.contains("Needs attention:\n✗ ambiguous: 1 transactions match several rules"), "{text}");
    assert!(text.contains("⚠ dead rule: 2 rules match nothing"), "{text}");
    // The unmapped transactions are the list above.
    assert!(!text.contains("not mapped"), "{text}");
    let json = digest.json();
    assert_eq!(json["transactions"][0]["number"], 1);
    assert_eq!(json["transactions"][0]["id"], "ba-2026-01-20-b7634b");
    assert_eq!(json["transactions"][0]["amount"], "-25.00");
    assert_eq!(json["findings"][0]["severity"], "error");
}

#[test]
fn digest_leaves_out_transactions_marked_later() {
    let build = build_with(r#"[later."ba-2026-01-20-b7634b"]"#);
    let digest = konter_diagnostics::digest::digest(&build);
    assert!(digest.items.is_empty());
    assert!(digest.text().starts_with("Needs attention:\n"), "{}", digest.text());
}

#[test]
fn digest_marks_purchases_during_a_declared_trip() {
    let build = build_with(
        r#"
        [trip.mystery-tour]
        account = "expenses:coffee"
        from = 2026-01-19
        to = 2026-01-20
        "#,
    );
    let digest = konter_diagnostics::digest::digest(&build);
    assert_eq!(digest.items[0].trip.map(|t| t.name.as_str()), Some("mystery-tour"));
    assert!(digest.text().contains("   bank:a · during the mystery-tour trip\n"), "{}", digest.text());
    assert_eq!(digest.json()["transactions"][0]["trip"]["from"], "2026-01-19");
}

#[test]
fn a_trip_needs_a_declared_account_and_dates_in_order() {
    let build = build_with(
        r#"
        [trip.nowhere]
        account = "expenses:travel:nowhere"
        from = 2026-01-01
        to = 2026-01-02
        "#,
    );
    let text = build.findings.render(true);
    assert!(text.contains(r#"expenses:travel:nowhere  (used by trip "nowhere")"#), "{text}");
    let (_, config) = fixture("basic");
    let backwards = r#"
        [trip.backwards]
        account = "expenses:coffee"
        from = 2026-01-02
        to = 2026-01-01
    "#;
    let err = Rules::load_str(backwards, "a.toml", &config).unwrap_err();
    assert!(format!("{err:#}").contains("`from` (2026-01-02) is after `to` (2026-01-01)"), "{err:#}");
}
