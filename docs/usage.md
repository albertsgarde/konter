# Using konter

What the tool does and why is in [spec.md](spec.md), and getting bank data in [fetching.md](fetching.md). This page covers day-to-day use. `just --list` shows every recipe. Paths are relative to your books (`books/`).

## The loop

1. **Get new data:** `just fetch` gets Nordea and Lunar through Enable Banking (after `just more konter link <bank>`) and then rebuilds, so step 2 is done too. CSV exports are the backup, and the only way for Nordea savings accounts: `just more konter add-export <file> [--source …]`.
2. **Rebuild** (after changing rules, or adding an export): `just more build`. This writes `ledger/generated.journal` and `ledger/forecast.journal`, runs `hledger check --strict`, and prints what needs attention.
3. **Map what's new:**
   - `just gaps` opens the gaps page (`just more gaps` prints the same as text). It shows how much of your income and spending is mapped (transfers and balance-sheet moves like buying a home left out), per account and in total, then the largest groups of similar unmapped transactions with the mapped share you reach once each is mapped, so you can see which few rules cover most of the money. The page lists every group, or with the Transactions switch every unmapped transaction, and sorts by any column (→ mapped then adds up in the order shown); the text can be narrowed with `just more gaps nordea:main`, `--skip nordea:spending`, `--from 2026-10-01` or `--top 0` (all groups).
   - `just more konter unmatched [source]` prints a pin stub for each unmatched transaction, leaving out those marked later.
   - `just more suggest [source]` proposes pattern rules for groups of similar ones.
   - `just more konter digest` prints what needs attention as a short message: the new transactions, numbered, and the build's errors and warnings (`--json` for a bot). Nothing when there's nothing to say.
   - Not now: mark a transaction `[later."<id>"]` (with an optional `note`) in its source's rule file. It stays unmapped, but stops counting against the source's policy and drops out of `konter unmatched`.
   - Going away: declare the trip in `config/rules/trips.toml` (`[trip.<name>]` with `account`, `from`, `to`), and the digest marks purchases made during it.
   - `just more konter explain <id>` shows a transaction's raw data, which rules match it and what it produced.
   - Edit the source's file in `config/rules/` (for example `nordea/main.toml`; your books' `config/rules/README.md` says which file holds what, and [rules.md](rules.md) how to write rules), declare new accounts in `config/accounts.toml`, and rebuild.
4. **Commit:** raw data, rules and the generated journals together. `just more check` confirms that the committed journals match a fresh build and that hledger is happy.

## Reports

`konter build` also writes a small site in `build/`, to open in a browser: a front page (`index.html`), the spending page (`spending.html`), the income page (`income.html`, the same page for income accounts) and the gaps page (`gaps.html`). `just web`, `just exp`, `just inc` and `just gaps` write it fresh and open one of them; `just more report` writes it without opening anything. The spending page shows spending per account and month. The account column and the month header stay in place while you scroll, accounts open and close, and clicking a number lists the transactions behind it. Clicking a column heading sorts by it. Subscriptions paid yearly or quarterly can be spread over the months they cover, and money moved into accounts marked `set_aside = true` in `config/sources.toml` (a charity account, say) can count as money out, with what's later paid from them left out so nothing counts twice. It reads the whole books, so hand-written entries count too. The site isn't committed.

`just fetch`, and `just web`, `just exp`, `just inc` and `just gaps` (the site's pages) are in the `justfile`; the rest are in `more.just` and run as `just more <recipe>`: `is` (monthly income statement), `bs`, `exp` (hledger's expenses table), `reg <account>`, `subs`, `gaps` (as text), `unmapped`, `coverage`, `forecast`, `ui`, `web` (hledger-web), or any hledger command via `hl …`.

## Where things live

| What | Where |
|---|---|
| Accounts you own, start date | `config/sources.toml` |
| Rules, pins, subscriptions | `config/rules/<bank>/<account>.toml`, `config/rules/subscriptions.toml` |
| Account declarations | `config/accounts.toml` |
| Hand-written transactions | `config/transactions.toml` |
| The books, for hledger (generated; never edit) | `ledger/main.journal`, which includes `generated.journal` and `forecast.journal` |
| Raw bank data (never edit) | `data/<bank>/<account>/`, hashes in `data/MANIFEST` |
| Enable Banking key and sessions | `~/.config/konter/` (outside the repo) |
