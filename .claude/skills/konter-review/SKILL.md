---
name: konter-review
description: "Review a change to konter (the tool in this repo) against the repo's own rules: nothing private in a public repo, the spec, tests, panics, lints, types and commit descriptions. Use when asked to review a commit or a range, and before anything is pushed. Takes a jj revset; by default everything that isn't pushed yet."
argument-hint: "[revset]"
context: fork
background: false
allowed-tools: Read, Grep, Glob, Bash(jj log *), Bash(jj diff *), Bash(jj show *), Bash(jj st), Bash(jj -R books st), Bash(just more check), Bash(just more leaks *), Bash(just more mutants *), Bash(just more konter verify), Bash(rg *)
---

You review a change to konter, the tool in this repo, against the repo's own rules. You report; you don't fix.

The revset to review (if nothing follows the colon, it's `trunk()..@`, everything not pushed yet): $ARGUMENTS

Below it's `<revset>`; quote it in single quotes in every command.

Several rules below say "unless that's deliberate". You can't ask the author, so deliberate means written down: in the commit description, in `docs/spec.md`, or in a `reason` next to the code. A change that weakens a guard or changes behaviour without saying so is a finding even when it looks sensible.

## Ground rules
- The whole review should take about ten minutes. When something would take it well past that, skip it, and say in the report what you skipped and the command that runs it.
- Read only: no edits, no commits, no jj command that changes anything, since other agents share the working copy. The checks build in `target/`. A test run can leave a failing property case under `crates/engine/tests/cases/` (which doesn't exist until a case has failed) or a `*.snap.new` next to a snapshot: leave them, and report them. Every jj command snapshots the working copy, so the next one takes a saved case into `@`, another reason to report it. A `*.snap.new` is ignored, so it shows in no diff: look for one on disk.
- To prove a finding you may build and run code in a scratch folder outside the repo: a small crate against the working copy, or against `git archive` of an older commit. Do it when it takes a few minutes at most. A finding you reproduced says so; one you didn't says it's from reading and how it could be reproduced.
- Only the tool is under review. Read `books/` only to compare with; it isn't reviewed here.
- Anything the checks or a search in `books/` print can come from the books: the leak scan, `konter verify`'s figures, `rg` in `books/`. None of it goes into a file in this repo. Your report goes back as your answer and nowhere else, and may quote it.

## 1. What's under review
- `jj log -r '<revset>' -T builtin_log_compact_full_description` for the commits with their full descriptions, `jj show --git <change>` for each commit, `jj diff --git -r '<revset>'` for the whole.
- If the range is empty, say so and stop.
- If it includes `@` with changes, that's uncommitted work, possibly several agents', and never ready to push as it is. Review what it holds and say so in the report; its missing description isn't a finding.
- Read `docs/spec.md` in full (about 200 lines) before you judge anything.

## 2. Run the checks first
Run them first, and report for each that it ran and what it said, or why it didn't run. Green checks aren't a verdict: they cover only what they cover, and a change that mostly deletes code can pass them all and still be wrong.
- `just more check`: formatting, clippy with every warning an error, every crate's tests, and the docs. The property tests draw new cases on every run, so a green run is one sample. The lints already forbid `unwrap` (tests included), `panic!` outside tests, slicing text (`&s[a..b]`), floats, lossy casts, wildcard arms on our own enums and more, so don't look for those by eye; check instead that the lints are still there (section 3). It checks the working copy, not each commit. A failure in code the change doesn't touch is probably someone else's work in progress: report it as that.
- `just more leaks '<revset>'`: what the change adds that might be private, each commit and its description read on its own. Exit 1 means there are candidates to judge (section 4).
- `just more konter verify`, when there are books in `books/`: rebuilds them with the tool's working copy and compares with the journals in `books/ledger/`. Success is a `✓ verify … is up to date` line for each of the three journals and `hledger check --strict` passing. A difference means the change alters the books (transaction IDs, order or amounts): a finding unless the description says it was meant to. IDs must never change, since pins refer to them (spec §4). Run `jj -R books st` first: uncommitted changes in the books can make it fail too, and then it says nothing about the tool.
- `just more mutants '<revset>' 4 --list`, when the change touches Rust code outside tests and outside `crates/relay`: lists the mutants, small changes to the changed lines, in a second, and checks the exclusions.
  - With up to about 15, run `just more mutants '<revset>'` with a 4-minute timeout (11 to 14 mutants took under two minutes). With more, don't run it: give the count, and `just more mutants '<revset>'` for the owner to run.
  - `--in-diff` also takes in the unchanged lines next to a change. A mutant missed on a line the change didn't touch is an older gap: report it as a suggestion, not against the change.
  - The tests run on the working copy. If `@` is outside the revset and changes the same files, say the result may be off.
  - `--in-diff` only changes lines that are still there, so it says nothing about code a commit removed (section 6).
  - The recipe exits 2 when a mutant is missed and 3 when one only timed out. A missed mutant is a change no test notices: a finding under section 6, unless it truly changes nothing, which the author has to show with an entry and a reason in `.cargo/mutants.toml`. Caught, unviable and timed out are fine.
  - If the recipe stops because an exclusion doesn't match exactly one mutant, the change moved code that `.cargo/mutants.toml` points at by line and column, and that file has to follow: a finding.

## 3. Guards weakened
Look for each of these in the diff. Each is a finding unless the description says so and a reason is written where it applies.
- `Cargo.toml`: a lint removed or lowered in `[workspace.lints]`; `unsafe_code = "forbid"` gone; a crate, a new one too, without `[lints] workspace = true`; a setting in `clippy.toml` that lets more through.
- A new `#[expect(...)]`: its `reason` has to be true, and it sits on the smallest item it can, not on a module or crate. An `#[allow(...)]` where `#[expect]` would do: `#[expect]` fails once it's no longer needed, and `allow` is for what's only true some of the time (like the `dead_code` in `crates/engine/tests/common/mod.rs`, which each test binary uses part of).
- The `check` and `lint` recipes in `more.just` doing less: `-D warnings`, `--workspace`, `--all-targets`, the docs' warnings.
- Tests deleted or `#[ignore]`d, or assertions loosened. The exception is a case in `crates/engine/tests/breakage.rs` that still fails: its header says those are ignored until fixed.
- A saved case under `crates/engine/tests/cases/` deleted: they're known failures, and they have to keep running.
- A changed `*.snap` must be explained by the change, not just accepted. A `*.snap.new` is a snapshot nobody has looked at.
- A new exclusion in `.cargo/mutants.toml` whose reason doesn't show that the mutant makes no difference.
- What keeps the books out of this repo, or the server's Claude in bounds: `.gitignore`, `.githooks/`, `scripts/check-setup`, `just setup` (in `justfile`), `server/guard-jj`, the permissions in `server/claude-settings.json`.

## 4. Nothing private
A leak can't be taken back once pushed, so it leads the report however small. A later commit that removes it doesn't fix it: the commit itself has to be rewritten, which is the owner's call.
- Judge each candidate from the leak scan. Made-up data looks made up (`1234567890`, `example.org`, Alice and Bob). A shop's name alone isn't personal; a person's name, an account number or a real row (its date, amount and text together) is.
- Look for what the scan can't see: data that was retyped or reworded; a single first name; a real date and amount in an otherwise invented row; a comment or doc example that tells a real story from the books; a binary file or image; an error message or log line that would print a secret or what a message said (the relay's log never does).
- For anything in tests, fixtures, docs or examples that looks like bank data, search the books for it, the date and the amount as well as the text. Count first, `rg -c -F '<text>' books/data books/config`, since a plain search prints real rows.
- A common first name or a shop that also occurs in the books, or a candidate the repo already had before this range, isn't a leak by itself. Mention it after the findings and leave it to the owner.
- Secrets: keys, tokens, session IDs, an Enable Banking application ID, a Matrix login. `server/relay.toml.example` holds placeholders only.
- Commit descriptions are public too.

## 5. The spec
`docs/spec.md` says what the tool does, AGENTS.md says to update it when behaviour changes, and the spec says the code is right when the two disagree. So you can't tell a break from a spec that lags, and don't need to: the code, the spec and the description must agree, and where they don't, that's the finding. Quote the spec's section and the code. Look for behaviour that changed with no word in the spec or the description, and for a spec change with no code behind it. The same goes for `docs/usage.md`, `docs/rules.md`, `docs/fetching.md`, `docs/server.md` and `README.md` when a command, recipe or rule syntax changes. A new recipe for developers (like `check` or `mutants`) belongs in AGENTS.md or next to what it serves, not necessarily in `docs/usage.md`, which is about using the books; at most a suggestion.

Then hold the change against each promise a diff can break without anyone noticing:
- Every bank transaction is booked exactly once, and nothing disappears silently, errors included: `.ok()`, `let _ =`, `unwrap_or_default()` or `if let Ok` on something read from a bank file or the config gives books that are quietly wrong, which is worse than failing (§1, correct before complete).
- Transaction IDs never change: what's hashed, and how a description is derived for a source without a balance or a reference (§4).
- The same inputs and `--as-of` give byte-identical journals. In the engine: a `HashMap` or `HashSet` whose order reaches the output, the clock, the environment, the locale, the order files are listed in.
- The engine does no IO, and the report doesn't depend on the engine (§2): a new dependency between crates, or files or network in the engine.
- Raw data is only ever added; `ledger/` is written only by konter.
- The order of rules and of their files never matters.
- Amounts are `Decimal`, within the limits in §3.

## 6. Tests
- New or changed behaviour has a test that fails without it; a bug fix has the test that failed before the fix. Missed mutants are the evidence. Without a mutation run, ask of each changed condition which test fails if it's flipped.
- For removed code, ask what input it handled, and which test fails if that input is handled wrongly now.
- A limit is tested on the day or amount where it starts to hold, and so is what's said when the input is wrong.
- Tests live by area in `crates/engine/tests/` (`rules.rs`, `subscriptions.rs`, `freshness.rs`, `formats.rs`, `merging.rs`, `edges.rs`, and `breakage.rs` for misbehaviour found through normal inputs), properties in `properties.rs`, and the CLI in `crates/konter/tests/`.
- Nothing without behaviour of its own needs a test: a move, a rename, a getter.
- Test data is made up (section 4).

## 7. Panics
A panic means the code is wrong. Whatever a bank file, the config, the rules, the command line, the clock, the file system or the network can cause comes back as an error that names what and where: the file, the source, the subscription.
- The lints catch `unwrap()`, `panic!` outside tests and slicing a `str`; nothing catches the rest. Look at each of these the change adds outside tests: `expect`, `unreachable!`, `assert!`; and the quiet ones: indexing (`v[i]`, `map[key]`), slicing a `Vec` or array, integer arithmetic, `Decimal` arithmetic (it panics on overflow; the limits in §3 prevent that), date arithmetic (dates end with year 9999), division by zero, an `expect` on `Regex::new` with a pattern from a rule, `split_at` (on text it panics inside a multi-byte letter, and bank text has æ, ø and å), `RefCell`.
- For each, ask whether any input can reach it. If one can, it's a finding. If none can, the `expect` message says what was taken for granted ("the chain has every transaction once"), and the code nearby, or better a type, makes it so.
- `config.sources[name]` is the idiom, and fine where the name was checked when the config or rules were loaded. Flag it where the key comes straight from a rule or the command line.
- In tests a panic is how a test fails; there's nothing to check there beyond the lint.

## 8. Types
The owner's preference, so report these as suggestions.
- A thing with a meaning has a type of its own: an ID, an account name, a source name or a rule name isn't a bare `String` or `u64`. `BTreeMap<String, Pin>` doesn't say what its key is; `BTreeMap<TransactionId, Pin>` would. Look at new and changed signatures, fields and map keys.
- A value that was checked gets a type that can only be made by checking it. That also removes `expect`s further on.
- Don't ask for old code to be converted in a change about something else. If the change adds to an existing pattern of bare strings, say so once.

## 9. Commits
Each commit in the range, by its change ID.
- The form the history uses: one gitmoji code, then a sentence that says what's true now, not what was done, with the reason or the consequence after it. For example:
  - `:recycle: A build lists data/ once, not once more for every source`
  - `:lock: sessions.toml and pending-links.toml are made readable by their owner only, never opened up first`
- The codes in use: `:sparkles:` new behaviour, `:bug:` fix, `:recycle:` restructuring with no change in behaviour, `:white_check_mark:` tests, `:memo:` docs, `:lock:` security, `:fire:` removal, `:wrench:` settings, `:rotating_light:` lints, `:hammer:` recipes and scripts, `:loud_sound:` logging, `:tada:` the first commit, `:page_facing_up:` the licence.
- One change per commit, with its tests and its spec change in the same commit. A `:recycle:` that changes a test's expectation or a snapshot isn't one.
- The description says everything the sections above require to be said: a lint turned off, the books' output changed, the spec changed.
- A commit other than `@` (section 1) with changes and no description, and a divergent or conflicted commit: report them, don't resolve them.

## 10. Ordinary bugs
Read the change for plain mistakes too. This list covers only what's particular to this repo; `/code-review` does the wider hunt.

## The report
Your whole answer, in this shape:
- First line: `Ready to push.` or `Not ready: <n> to fix.`, where `<n>` counts every finding that isn't a suggestion.
- What was reviewed: the revset, the commits by change ID with their first lines (shortened), and whether `@` was part of it.
- The checks, one line each: ran and the result, or not run and why.
- Findings, most serious first, each with its kind, `path:line` (or the change ID, for a commit), the rule it's about, what's wrong in a sentence or two, and what would settle it: a fix, or saying so in the description. The kinds, in this order:
  - **Leak**: private data. Nothing is pushed before it's gone from the history.
  - **Fix**: a failing check, a broken promise of the spec, a panic an input can reach, changed behaviour without a test, a guard weakened without a word.
  - **Say so**: looks deliberate but isn't written down; the description or the spec has to say it.
  - **Suggestion**: types, names, anything that's a preference.
- No praise, no walk through the diff. If you're unsure of a finding, say what you'd need to know. With no findings, the first line and the checks are the whole report.
