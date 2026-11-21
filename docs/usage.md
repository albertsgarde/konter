# Using konter

What the tool does and why is in [spec.md](spec.md), and getting bank data in [fetching.md](fetching.md). This page covers day-to-day use. `just --list` shows every recipe. Paths are relative to your books (`books/`).

## New books

`konter init <dir>` makes new, empty books that build as they are, and makes the folder a git repo first (with jj, run `jj git init --colocate` in it afterwards). `config/sources.toml` has a commented example of a bank account to start from. It never replaces anything: it refuses a folder that already has `config/` or `data/`, and keeps a README or `.gitignore` that's already there. `just setup` runs it when there are no books yet.

## The loop

1. **Get new data:** `just fetch` gets Nordea and Lunar through Enable Banking (after `just more konter link <bank>`) and then rebuilds, so step 2 is done too. CSV exports are the backup, and the only way for Nordea savings accounts: `just more konter add-export <file> [--source …]`. Both read the new file into the store of transactions, which the books are built from; a build doesn't read the raw files. If `verify` says raw files haven't been read into the store, `just more konter store` reads them.
2. **Rebuild** (after changing rules, or adding an export): `just more build`. This writes `ledger/generated.journal` and `ledger/forecast.journal`, runs `hledger check --strict`, and prints what needs attention.
3. **Map what's new:**
   - `just gaps` opens the gaps page (`just more gaps` prints the same as text). It shows how much of your income and spending is mapped (transfers and balance-sheet moves like buying a home left out), per account and in total, then the largest groups of similar unmapped transactions with the mapped share you reach once each is mapped, so you can see which few rules cover most of the money. The page lists every group, or with the Transactions switch every unmapped transaction, and sorts by any column (→ mapped then adds up in the order shown); the text can be narrowed with `just more gaps nordea:main`, `--skip nordea:spending`, `--from 2026-10-01` or `--top 0` (all groups).
   - `just more konter unmatched [source]` prints a pin stub for each unmatched transaction, leaving out those marked later.
   - `just more suggest [source]` proposes pattern rules for groups of similar ones.
   - `just more konter digest` prints what needs attention as a short message: the new transactions, numbered, and the build's errors and warnings (`--json` for a bot). Nothing when there's nothing to say.
   - Not now: mark a transaction `[later."<id>"]` (with an optional `note`) in its source's rule file. It stays unmapped, but stops counting against the source's policy and drops out of `konter unmatched`.
   - Going away: declare the trip in `config/rules/trips.toml` (`[trip.<name>]` with `account`, `from`, `to`), and the digest marks purchases made during it.
   - `just more konter explain <id>` shows a transaction as the store has it, where it was read from and whether it's settled, which rules match it and what it produced.
   - Edit the source's file in `config/rules/` (for example `nordea/main.toml`; your books' `config/rules/README.md` says which file holds what, and [rules.md](rules.md) how to write rules), declare new accounts in `config/accounts.toml`, and rebuild.
4. **Commit:** raw data, the store (`store/`), rules and the generated journals together. `just more verify` confirms that the journals match a fresh build and that hledger is happy, with the working copy and with the pinned tool (below). It also checks `data/` against `data/MANIFEST`, and that the store has read every raw file.

## The pinned tool

The recipes run the tool's working copy, so a change to the tool shows up in the books on the next run. The books also record which tool builds them, in `pin/`: a small Nix flake whose `flake.lock` holds the exact commit of `konter`, which in turn decides the hledger. `just pinned <recipe> [args]` runs any recipe with that `konter` and hledger instead (`just pinned fetch`), as the server's daily run does ([server.md](server.md)), so it never depends on half-finished work on the tool.

- `just pin [rev]` moves the pin to a commit of the tool: by default the last one (`@-`), or `main` for GitHub's newest. A commit that isn't on GitHub yet gives a local pin, which works only on this machine; run `just pin <commit>` again once it's pushed, and it becomes a GitHub pin with an identical build.
- Move the pin when the books start to depend on something new in the tool, and commit `pin/` with that change. Before moving it, build with the new tool and check that `jj diff ledger/` is empty, unless the change was meant to alter the books.
- `just more verify` fails if the pinned tool doesn't build the journals as they are, which catches rules that use something new without moving the pin. A local pin is fine while you work; `just more verify --push`, for before pushing the books, also fails on one, since the pushed books need a pin that works on any machine.
- `pin/` is a flake of its own rather than one at the books' root because Nix copies a flake's whole git repo into its store, which every user on the machine can read. As a `path:` flake, only `pin/` is copied.

## Reports

`konter build` also writes a small site in `build/`, to open in a browser: a front page (`index.html`) with how much money isn't mapped yet and the bank transactions left to settle in the store ([spec.md](spec.md) §4), the spending page (`spending.html`), the income page (`income.html`, the same page for income accounts) and the gaps page (`gaps.html`). `just web`, `just exp`, `just inc` and `just gaps` write it fresh and open one of them; `just more report` writes it without opening anything. The spending page shows spending per account and month. The account column and the month header stay in place while you scroll, accounts open and close, and clicking a number lists the transactions behind it. A transaction the store hasn't settled is marked "⚠ not settled" there, linking to the front page. Clicking a column heading sorts by it. Subscriptions paid yearly or quarterly can be spread over the months they cover, and money moved into accounts marked `set_aside = true` in `config/sources.toml` (a charity account, say) can count as money out, with what's later paid from them left out so nothing counts twice. It reads the whole books, so hand-written entries count too. The site isn't committed.

`just fetch`, and `just web`, `just exp`, `just inc` and `just gaps` (the site's pages) are in the `justfile`; the rest are in `more.just` and run as `just more <recipe>`: `is` (monthly income statement), `bs`, `exp` (hledger's expenses table), `reg <account>`, `subs`, `gaps` (as text), `unmapped`, `coverage`, `forecast`, `ui`, `web` (hledger-web), or any hledger command via `hl …`. On a server, `setup-server`, `daily`, `auto-start` and `auto-push` run the daily fetch and commit to the books' `auto` bookmark ([server.md](server.md)).

## Where things live

| What | Where |
|---|---|
| Accounts you own, start date | `config/sources.toml` |
| Rules, pins, subscriptions | `config/rules/<bank>/<account>.toml`, `config/rules/subscriptions.toml` |
| Account declarations | `config/accounts.toml` |
| Hand-written transactions | `config/transactions.toml` |
| The books, for hledger (generated; never edit) | `ledger/main.journal`, which includes `generated.journal` and `forecast.journal` |
| Raw bank data (never edit; read only by `fetch`, `add-export`, `store` and `verify`) | `data/<bank>/<account>/`, hashes in `data/MANIFEST` |
| The store of transactions (written by `fetch`, `add-export` and `store`; the books are built from it) | `store/bank-transactions.csv`, and `store/files.csv` for the raw files it has read |
| Enable Banking key and sessions, links begun with `konter link --start`, the relay's `relay.toml` | `~/.config/konter/` (outside the repo) |
| The relay's login, encryption store and outbox (on a server) | `~/.local/state/konter-relay/` |
