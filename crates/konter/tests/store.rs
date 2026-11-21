//! The store of transactions as `fetch` and `add-export` keep it, run as the
//! `konter` binary on a copy of tests/fixtures/basic: a table in
//! store/bank-transactions.csv where a transaction keeps the ID it was first
//! given (docs/spec.md §4). Nothing builds from it yet.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const TABLE: &str = "store/bank-transactions.csv";
const NORDEA_DIR: &str = "data/nordea/1234567890";
const LUNAR: &str = "data/lunar/555/statement.csv";

/// A fresh copy of the basic fixture, without its store: the raw files are
/// in data/ and nothing has read them yet.
fn scratch(name: &str) -> PathBuf {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).expect("the folder is made");
        for entry in std::fs::read_dir(from).expect("the fixture's folder is read") {
            let path = entry.expect("the folder's entry is read").path();
            let target = to.join(path.file_name().expect("the entry has a name"));
            if path.is_dir() {
                copy(&path, &target)
            } else {
                std::fs::copy(&path, &target).map(|_| ()).expect("the file is copied")
            }
        }
    }
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join("store").join(name);
    let _ = std::fs::remove_dir_all(&root);
    copy(&Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/basic"), &root);
    std::fs::remove_dir_all(root.join("store")).expect("the fixture's store is removed");
    root
}

fn text(output: &Output) -> String {
    format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr))
}

/// `konter add-export` of a file, which may be a raw file where it lies:
/// then nothing is added, and the store is still brought up to date.
fn add(root: &Path, file: &Path, source: Option<&str>) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_konter"));
    command.arg("--root").arg(root).arg("add-export").arg(file);
    if let Some(source) = source {
        command.args(["--source", source]);
    }
    command.output().expect("konter runs")
}

/// `konter build`.
fn build(root: &Path) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_konter"));
    command.arg("--root").arg(root).args(["build", "--as-of", "2026-02-15"]).output().expect("konter runs")
}

/// The site's front page after a build.
fn front_page(root: &Path) -> String {
    build(root);
    std::fs::read_to_string(root.join("build/index.html")).expect("the front page is read")
}

fn table(root: &Path) -> String {
    std::fs::read_to_string(root.join(TABLE)).expect("the table is read")
}

/// Each row's ID, by its account, date, amount and text.
fn ids(table: &str) -> BTreeMap<String, String> {
    let rows = table.lines().skip(1).map(|line| {
        let cells: Vec<&str> = line.split(',').collect();
        (format!("{} {} {} {}", cells[1], cells[2], cells[3], cells[5]), cells[0].to_owned())
    });
    rows.collect()
}

/// The fixture's Nordea export a week on: one more transaction, and the rest as it was.
fn later_export(root: &Path) -> PathBuf {
    let first = root.join(NORDEA_DIR).join("Konto 1234567890 - 2026-01-31 10.00.00.csv");
    let text = std::fs::read_to_string(first).expect("the export is read");
    let (heading, rows) = text.split_once('\n').expect("a heading and rows");
    let later = root.join("Konto 1234567890 - 2026-02-05 10.00.00.csv");
    std::fs::write(&later, format!("{heading}\n2026/02/02;-75,00;1234567890;;;Books;7300,00;DKK;;\n{rows}"))
        .expect("the export is written");
    later
}

/// The first command that adds a file reads every raw file into the table.
/// After that a file's rows that the table has keep their IDs, new ones get
/// one, and a run with nothing new leaves the table's file alone.
#[test]
fn the_table_follows_the_raw_files_and_ids_stay() {
    let root = scratch("follows");
    let first = add(&root, &root.join(LUNAR), Some("lunar:b"));
    assert!(first.status.success(), "{}", text(&first));
    assert!(
        text(&first).contains("store/bank-transactions.csv: 11 transactions, 11 of them new, from 2 files"),
        "{}",
        text(&first)
    );
    let before = table(&root);
    assert!(before.starts_with("id,account,date,amount,currency,text,"), "{before}");
    let given = ids(&before);
    assert_eq!(given.len(), 11);
    assert!(given.contains_key("bank:a 2026-01-30 -1200 Concert tickets"), "{given:?}");
    // Every ID is a UUID of its own.
    let distinct: std::collections::BTreeSet<&String> = given.values().collect();
    assert_eq!(distinct.len(), 11);
    assert!(given.values().all(|id| id.len() == 36 && id.chars().nth(14) == Some('4')), "{given:?}");

    // A file that was only dropped in data/ isn't in the manifest, and isn't read.
    std::fs::write(
        root.join("data/lunar/555/stray.csv"),
        "Date,Time,Title,Amount,Balance,Transaction ID\n30.01.2026,09.00,Stray,\"-1,00\",\"519,00\",stray-1\n",
    )
    .expect("the file is written");
    let second = add(&root, &later_export(&root), None);
    assert!(
        text(&second).contains("store/bank-transactions.csv: 12 transactions, 1 of them new, from 1 files"),
        "{}",
        text(&second)
    );
    let after = table(&root);
    let now = ids(&after);
    assert_eq!(now.len(), 12);
    for (transaction, id) in &given {
        assert_eq!(now.get(transaction), Some(id), "{transaction}");
    }
    assert!(now.contains_key("bank:a 2026-02-02 -75 Books"), "{now:?}");
    assert!(!after.contains("Stray"), "{after}");
    // The ones in both exports name both.
    let both = "nordea/1234567890/Konto 1234567890 - 2026-01-31 10.00.00.csv:2|\
                nordea/1234567890/Konto 1234567890 - 2026-02-05 10.00.00.csv:3";
    assert!(after.contains(both), "{after}");

    // Nothing new: the table is as it was, and no temporary file is left.
    let third = add(&root, &root.join(LUNAR), Some("lunar:b"));
    assert!(text(&third).contains("store/bank-transactions.csv: 12 transactions\n"), "{}", text(&third));
    assert_eq!(table(&root), after);
    let files: Vec<String> = std::fs::read_dir(root.join("store"))
        .expect("the folder is read")
        .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
        .collect();
    let mut files = files;
    files.sort();
    assert_eq!(files, ["bank-transactions.csv", "files.csv"]);
    // The files' table has each raw file that was read, once.
    let read = std::fs::read_to_string(root.join("store/files.csv")).expect("the files' table is read");
    assert_eq!(
        read,
        "file,account,exported,balance_date,balance\n\
         lunar/555/statement.csv,lunar:b,,,\n\
         nordea/1234567890/Konto 1234567890 - 2026-01-31 10.00.00.csv,bank:a,2026-01-31,,\n\
         nordea/1234567890/Konto 1234567890 - 2026-02-05 10.00.00.csv,bank:a,2026-02-05,,\n"
    );

    // The front page says how much money is unmapped and that the table has
    // nothing to settle. The fixture's pins name the IDs of the fixture's
    // own store, and this copy made its table anew, so what they map is
    // unmapped here too: 2,430 in all.
    let page = front_page(&root);
    assert!(page.contains("Unmapped: <a href=\"gaps.html\"><strong>2,430 DKK</strong></a>"), "{page}");
    assert!(page.contains("<h2>Bank transactions to settle</h2>\n<p class=\"muted\">Nothing to settle.</p>"), "{page}");
}

/// What the reader isn't sure of doesn't stop a file from being read: it's
/// marked in the table and warned of. Here a second Lunar export has one
/// transaction a day later than the table has it, and words the other
/// differently.
#[test]
fn what_the_reader_isnt_sure_of_is_marked_and_warned_of() {
    let root = scratch("unsure");
    add(&root, &root.join(LUNAR), Some("lunar:b"));
    let given = ids(&table(&root));
    let (coffee, transfer) =
        (&given["lunar:b 2026-01-10 -30 Coffee Shop"], &given["lunar:b 2026-01-29 500 Overførsel"]);

    let later = root.join("later.csv");
    std::fs::write(
        &later,
        "Date,Time,Title,Amount,Balance,Transaction ID\n\
         29.01.2026,09.00,Transfer,\"500,00\",\"520,00\",00000000-0000-0000-0000-000000000002\n\
         11.01.2026,12.00,Coffee Shop,\"-30,00\",\"20,00\",00000000-0000-0000-0000-000000000001\n",
    )
    .expect("the export is written");
    let said = text(&add(&root, &later, Some("lunar:b")));
    let after = table(&root);
    let moved = &ids(&after)["lunar:b 2026-01-11 -30 Coffee Shop"];
    for line in [
        "store/bank-transactions.csv: 12 transactions, 1 of them new, from 1 files".to_owned(),
        "rows added that may be transactions store/bank-transactions.csv already had: 1".to_owned(),
        format!(
            "{moved} lunar:b 2026-01-11 -30 \"Coffee Shop\" may be {coffee} lunar:b 2026-01-10 -30 \"Coffee Shop\""
        ),
        "→ each was added as a row of its own, with the other's ID in `maybe_same_as`".to_owned(),
        "differences between a copy and what store/bank-transactions.csv has: 1".to_owned(),
        "lunar:b 2026-01-29 500 \"Overførsel\": text is \"Overførsel\" in the table and \"Transfer\" in \
         lunar/555/later.csv:2"
            .to_owned(),
        "→ the table kept what it had; the transactions have the copies in `conflicts`".to_owned(),
    ] {
        assert!(said.contains(&line), "{line}\n{said}");
    }
    let row = |id: &str| after.lines().find(|line| line.starts_with(id)).expect("the row is there").to_owned();
    assert!(row(moved).ends_with(&format!(",{coffee},,lunar/555/later.csv:3")), "{}", row(moved));
    assert!(
        row(transfer).contains(",Overførsel,")
            && row(transfer).contains(r#"[{""at"":""lunar/555/later.csv:2"",""says"":{""text"":""Transfer""}}]"#),
        "{}",
        row(transfer)
    );

    // The front page has both: the two rows that may be one transaction, and
    // what the table has beside what the copy says.
    let page = front_page(&root);
    for line in [
        "<h3>May be a transaction the table already had (1)</h3>".to_owned(),
        format!(
            "<tr class=\"first\"><td class=\"muted\">added</td><td>lunar:b</td><td>2026-01-11</td>\
             <td class=\"num\">-30.00</td><td class=\"desc\" title=\"Coffee Shop\">Coffee Shop</td><td></td>\
             <td class=\"num\">20.00</td><td>00000000-0000-0000-0000-000000000001</td>\
             <td class=\"id\">{moved}</td><td>lunar/555/later.csv:3</td></tr>"
        ),
        format!(
            "<tr><td class=\"muted\">may be</td><td>lunar:b</td><td>2026-01-10</td><td class=\"num\">-30.00</td>\
             <td class=\"desc\" title=\"Coffee Shop\">Coffee Shop</td><td></td><td class=\"num\">20.00</td>\
             <td>00000000-0000-0000-0000-000000000001</td><td class=\"id\">{coffee}</td>\
             <td>lunar/555/statement.csv:3</td></tr>"
        ),
        "<h3>Copies that say something else (1)</h3>".to_owned(),
        format!(
            "<td class=\"desc\" title=\"Overførsel\">Overførsel</td><td>text</td><td>Overførsel</td>\
             <td>Transfer</td><td>lunar/555/later.csv:2</td><td class=\"id\">{transfer}</td></tr>"
        ),
    ] {
        assert!(page.contains(&line), "{line}\n{page}");
    }
    assert!(!page.contains("Nothing to settle"), "{page}");
    // The page comes from the store alone: the copy's file can go, and it
    // says the same.
    std::fs::write(root.join("data/lunar/555/later.csv"), "no longer an export\n").expect("the file is written");
    assert_eq!(front_page(&root), page);

    // The transfer is in the books with what it had, marked as not settled:
    // in the journal, and so wherever it's shown.
    let journal = std::fs::read_to_string(root.join("ledger/generated.journal")).expect("the journal is read");
    let entry = journal.lines().find(|line| line.contains(&format!("bank-id:{transfer}"))).expect("the transfer");
    assert!(entry.contains("unsettled:conflicts"), "{entry}");
    let entry = journal.lines().find(|line| line.contains(&format!("bank-id:{coffee}"))).expect("the coffee");
    assert!(entry.contains("unsettled:maybe-same-as"), "{entry}");
    assert!(!journal.contains(&format!("bank-id:{moved}")), "{journal}");
    let mut command = Command::new(env!("CARGO_BIN_EXE_konter"));
    let listed = command.arg("--root").arg(&root).arg("list").output().expect("konter runs");
    let listed = text(&listed);
    let line = listed.lines().find(|line| line.starts_with(coffee.as_str())).expect("the coffee");
    assert!(line.ends_with("Coffee Shop  ⚠ not settled (maybe-same-as)"), "{line}");
    assert_eq!(listed.matches("not settled").count(), 2, "{listed}");
    // The gaps report notes it on the coffee, which no rule maps, and on
    // nothing else; the page does too.
    let mut command = Command::new(env!("CARGO_BIN_EXE_konter"));
    let gaps = text(&command.arg("--root").arg(&root).arg("gaps").output().expect("konter runs"));
    let noted: Vec<&str> = gaps.lines().filter(|line| line.contains("not settled")).collect();
    assert!(matches!(noted.as_slice(), [line] if line.contains("Coffee Shop")), "{gaps}");
    let page = std::fs::read_to_string(root.join("build/gaps.html")).expect("the gaps page is read");
    let noted: Vec<&str> = page.lines().filter(|line| line.contains("not settled")).collect();
    assert!(noted.iter().all(|line| line.contains("Coffee Shop")) && !noted.is_empty(), "{noted:?}");
    let mut command = Command::new(env!("CARGO_BIN_EXE_konter"));
    let explained = command.arg("--root").arg(&root).args(["explain", moved]).output().expect("konter runs");
    let explained = text(&explained);
    assert!(explained.starts_with(&format!("{coffee}  (lunar:b)")), "{explained}");
    assert!(explained.contains("not settled   maybe-same-as (the site's front page has what)"), "{explained}");
}

/// The raw file is safe in data/ before the table is looked at, so a table
/// that can't be read is an error to report, with the file added all the
/// same; and the table isn't replaced by one made from scratch, which would
/// give every transaction a new ID. The books can't be built without it.
#[test]
fn a_table_that_cant_be_read_is_reported_and_left_alone() {
    let root = scratch("broken");
    std::fs::create_dir_all(root.join("store")).expect("the folder is made");
    std::fs::write(root.join(TABLE), "not,a,table\n").expect("the file is written");
    let run = add(&root, &later_export(&root), None);
    assert_eq!(run.status.code(), Some(1), "{}", text(&run));
    assert!(
        text(&run).contains(
            "the store wasn't brought up to date: reading store/bank-transactions.csv: the table's first line: \
             columns that this konter doesn't know: [\"not\", \"a\", \"table\"]"
        ),
        "{}",
        text(&run)
    );
    assert_eq!(table(&root), "not,a,table\n");
    assert!(root.join(NORDEA_DIR).join("Konto 1234567890 - 2026-02-05 10.00.00.csv").is_file());
    let built = build(&root);
    assert_eq!(built.status.code(), Some(2), "{}", text(&built));
    assert!(
        text(&built).contains("error: reading store/bank-transactions.csv: the table's first line: columns that"),
        "{}",
        text(&built)
    );
    assert!(!root.join("ledger").exists());
}

/// A raw file that can't be parsed is left out and named, and the others are read.
#[test]
fn a_raw_file_that_cant_be_parsed_is_left_out_of_the_table() {
    let root = scratch("unparsed");
    std::fs::write(root.join(LUNAR), "Date,Time,Title,Amount,Balance,Transaction ID\nsoon,,,,,\n")
        .expect("the file is written");
    let run = add(&root, &root.join(NORDEA_DIR).join("Konto 1234567890 - 2026-01-31 10.00.00.csv"), None);
    let said = text(&run);
    assert!(said.contains("store/bank-transactions.csv: 9 transactions, 9 of them new, from 1 files"), "{said}");
    assert!(said.contains("raw files that couldn't be read into the store: 1"), "{said}");
    assert!(said.contains("lunar/555/statement.csv"), "{said}");
    assert!(!table(&root).contains("lunar:b"));
}

/// One kind of open case without the other is still something to settle.
#[test]
fn a_row_we_arent_sure_of_alone_is_listed() {
    let root = scratch("only-unsure");
    add(&root, &root.join(LUNAR), Some("lunar:b"));
    let later = root.join("later.csv");
    std::fs::write(
        &later,
        "Date,Time,Title,Amount,Balance,Transaction ID\n\
         11.01.2026,12.00,Coffee Shop,\"-30,00\",\"20,00\",00000000-0000-0000-0000-000000000001\n",
    )
    .expect("the export is written");
    add(&root, &later, Some("lunar:b"));
    let page = front_page(&root);
    assert!(page.contains("<h3>May be a transaction the table already had (1)</h3>"), "{page}");
    assert!(!page.contains("Nothing to settle") && !page.contains("Copies that say something else"), "{page}");
}

/// The books are built from the store alone, so a build says nothing about
/// a raw file the store hasn't read: `konter verify` does, and `konter store`
/// reads it.
#[test]
fn verify_names_the_raw_files_the_store_hasnt_read() {
    let root = scratch("unread");
    let konter = |args: &[&str]| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_konter"));
        text(&command.arg("--root").arg(&root).args(args).output().expect("konter runs"))
    };
    let built = text(&build(&root));
    assert!(built.contains("bank:a: no raw data yet") && !built.contains("haven't been read"), "{built}");
    let verified = konter(&["verify"]);
    for line in [
        "2 raw files haven't been read into the store",
        "data/nordea/1234567890/Konto 1234567890 - 2026-01-31 10.00.00.csv",
        "data/lunar/555/statement.csv",
        "→ the books are built without their rows; `konter store` reads them",
        "2 raw files match data/MANIFEST",
    ] {
        assert!(verified.contains(line), "{line}\n{verified}");
    }
    let stored = konter(&["store"]);
    assert!(stored.contains("store/bank-transactions.csv: 11 transactions, 11 of them new, from 2 files"), "{stored}");
    assert!(stored.contains("2 raw files match data/MANIFEST"), "{stored}");
    let built = text(&build(&root));
    assert!(built.contains("bank:a: 9 transactions 2026-01-02 … 2026-01-30 from 1 files"), "{built}");
    assert!(!konter(&["verify"]).contains("haven't been read"));

    // A balance written down beside an export later is read too, and a file
    // in a folder that is no source's is named.
    std::fs::write(root.join("data/lunar/555/statement.csv.balance.toml"), "date = 2026-01-29\nbalance = \"520.00\"\n")
        .expect("the balance is written");
    let manifest = root.join("data/MANIFEST");
    let listed = std::fs::read_to_string(&manifest).expect("the manifest is read");
    let hash = "9e3b7b0c0f6e8e4f0d5f3f0e8b7f6a5d4c3b2a190817263544536271809a0b1c";
    std::fs::write(&manifest, format!("{hash}  lunar/555/statement.csv.balance.toml\n{listed}")).expect("it's written");
    std::fs::create_dir_all(root.join("data/elsewhere")).expect("the folder is made");
    std::fs::write(root.join("data/elsewhere/a.csv"), "x\n").expect("the file is written");
    let verified = konter(&["verify"]);
    assert!(verified.contains("1 raw files haven't been read into the store"), "{verified}");
    assert!(verified.contains("raw files in a directory that belongs to no imported source"), "{verified}");
    konter(&["store"]);
    let read = std::fs::read_to_string(root.join("store/files.csv")).expect("the files' table is read");
    assert!(read.contains("lunar/555/statement.csv.balance.toml,lunar:b,,2026-01-29,520\n"), "{read}");
    assert!(!konter(&["verify"]).contains("haven't been read"));
}
