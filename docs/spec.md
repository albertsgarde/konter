# How the books are made: current spec

What `konter` does today and why, for comparing future changes against: what the code does now and the reasons behind it, some of which live only in the code. If this file and the code disagree, the code is right and this file needs updating.

As of 2026-11-15.

## 1. Principles

These decide most questions not answered below.

- **Every bank transaction is booked exactly once.** It's mapped by a rule or pin, explicitly ignored, marked won't-map, or it falls into a per-source fallback account (whether or not it's marked to be mapped later). Nothing disappears silently.
- **The bank is ground truth.** Every bank posting carries a balance assertion, and every build checks them all (§7). If the books disagree with the bank, the build fails. `konter verify` also runs `hledger check --strict` on the written journals: a second opinion from a program that shares none of konter's code.
- **Correct before complete.** Incomplete mapping is normal and reported. A wrong mapping is worse than none. Every booking must stay correct once *everything* is mapped: never rely on something staying unmapped, and book each expense once, where it was paid.
- **Your spending is what you consumed.** Money paid for others, or paid back by them, nets out on a receivable or reduces the expense it covered. It's never counted as your spending or your income.
- **Deterministic and reproducible.** The same inputs and `--as-of` give byte-identical journals. `konter verify` rebuilds with the as-of recorded in the journal header and compares.
- **Order-independent rules.** At most one pattern rule may match a transaction (priority breaks ties), so the order of rules and the files they're in never matter. That's what lets several agents edit rules at once.
- **Traceable.** Tags (`bank-id`, `rule`, `subscription`, `pays`, `transfer`, `transfer-pair`, `purchased`, `original`) and `konter explain <id>` lead from any journal line back to the raw data and the rule that made it.
- **Everything is committed** except secrets: raw data, rules and the generated journals. `jj diff` shows how a change affected the books.
- **Three kinds of files, in three places.** `data/` is fetched or exported from the banks and never edited. `config/` is everything written by hand. `ledger/` is written only by konter; konter, hledger and the report read it, and nothing edits it.

## 2. Structure and data flow

| Crate | Role | IO |
|---|---|---|
| `konter-engine` | config and rules parsing, bank formats, merging, rule engine, transfers, subscriptions, journal text, the whole books and their checks, findings | none |
| `konter-repo` | the repository on disk: `load` (inputs), `books` (build, write, verify, `hledger check`), manifest, Enable Banking `fetch` | yes |
| `konter-diagnostics` | `gaps`, `suggest`, `coverage` on a finished build, as text (and the gaps page's body as HTML) | none |
| `konter-report` | the site in `build/`: its shared frame and navigation, and the spending and income pages, read from the books | writes `build/`; doesn't depend on the engine |
| `konter` | the CLI; connects the others | yes |
| `konter-relay` | the server's Matrix relay ([server.md](server.md)): leaves the daily digest in an outbox, posts it, and hands replies to Claude in the server's clone. Not part of `konter`, and not in its build | yes |

The engine has no IO so that everything it does can be tested from text, and so that reading, writing and fetching are all in one place. The report depends only on the books, so it can't come to depend on how they're made: it reads them as its own `Transaction` type, which `konter` fills from the build. Code that makes or checks the books goes in the engine (or in `konter-repo` if it touches disk or network). Code that reads the books goes in the report. The gaps page needs the build, not just the books, so its body is made in `konter-diagnostics` and only put in the site's frame by `konter-report`.

Flow: `load` reads config (including the accounts and the hand-written transactions), rules and raw files. `pipeline::assemble` then does these steps:
1. Merges each source.
2. Checks freshness and that every account used is declared.
3. Applies rules and pairs transfers.
4. Books subscriptions.
5. Assembles the whole books in memory and checks them (§7).
6. Renders `generated.journal` from the whole books, and `forecast.journal`.

Finally `books` writes everything in `ledger/`: those two and `main.journal`, hledger's entry point, which includes them. Any other file there is warned about, since nothing reads it. Everything that reads the books (the report, the diagnostics) reads them from memory, never from the journal files; hledger isn't needed to build. `konter verify` runs `hledger check --strict` when hledger is installed, and warns when it isn't.

## 3. Inputs

**`config/sources.toml`**
- `[books]`: `start`, `currency`, `transfer_window_days` (5).
- `[sources."<bank>:<account>"]`: one per account you own.
  - `ledger`: the hledger account it books to.
  - `id_prefix` (lowercase letters, since pins expect them), `formats`.
  - `unmatched`: error, warn or allow (see §5).
  - `start` / `before_start`: an account that wasn't yours before `start` (say, a friend's account that you took over on 2026-09-16). Transfers to or from it before then go to `before_start` (e.g. `assets:receivable:sam`), whether it's imported or not. An imported account with a `start` needs `before_start` if there are any; for one that isn't imported, transfers without it go to its own account.
  - `cutover`: for mixing CSV and API data.
  - `set_aside`: read only by the report.
- `[banks.<name>]`: Enable Banking settings and `history_days` (540 for Nordea).

**`data/`**
- Raw files are never edited. `data/MANIFEST` records their hashes, and a mismatch is an error.
- Raw files are only ever added, never overwritten, and a path is recorded once: `add-export` refuses a file that has changed since it was added (even when given that file itself) and a name already taken by other content. `fetch` and `add-export` lock `data/` while they add to it, so several can run at once, and the manifest is replaced in one step, so it's never seen half-written.
- Any number of overlapping files is fine; merging sorts them out (§4).
- A `.balance.toml` sidecar (from `add-export --balance`) gives a balance assertion for exports without a running balance. Like a raw file it's recorded once: a different balance for the same export is refused.
- `data/reference/transactions.csv` is an old categorized export. It's only a hint for `suggest` and `gaps`, never an input to the books.

**Rules:** every `*.toml` under `config/rules/`. Files are split by source (`nordea/spending.toml`), and big groups by topic (`spending-trips.toml`, `spending-shared.toml`). Split by topic, not by date: pins that only make sense together (a payment and its repayment) must stay in one file. `config/rules/README.md` says what goes where.

**`config/accounts.toml`**: `accounts = [...]`, every account the rules and the hand-written transactions use.
- Declared by hand so a typo in a rule fails the build. Comments say what an account is for.
- Accounts the tool invents (fallbacks, `assets:transfers`, subscription balances, opening balances) aren't listed; konter declares them itself.
- The books have one commodity, `currency` in `config/sources.toml`.

**`config/transactions.toml`**: the few transactions that have no bank row and aren't subscriptions, such as writing off a loan that won't be repaid, as `[[transaction]]` with `date`, `payee`, optional `note` and `tags`, and `postings`. Amounts are in the books' currency. Each posting has one of:
- `amount`;
- `rest = true`: what balances the transaction (one per transaction);
- `balance`: a balance assignment. The posting gets the amount that leaves the account with that balance at the end of the day, so it stays right when earlier entries change.

A posting with both `amount` and `balance` asserts the balance. Hand-written transactions never post to bank accounts: the bank's assertions would fail.

## 4. From raw files to bank transactions

- **Parsers:** Nordea CSV, Lunar CSV, and Enable Banking JSON (both banks).
  - Enable Banking's description is the first non-empty line of the remittance text.
  - The counterparty name comes from the creditor or debtor.
  - The purchase date is kept (`purchased:` tag), but **booking date** is the transaction date throughout.
- **Merging per source:**
  - Deduplicate overlapping files; a disagreement between files is an error. Copies in the same format must agree on everything, booking date and amount included: rows told apart by the bank's reference alone would otherwise let a reference the bank used twice swallow a transaction.
  - Rows that look alike (same day, amount and running balance, say) are told apart by counting them oldest first within each file. Every export of a day starts at its beginning, while one taken during the day ends partway, so the n-th of a kind is the same row in each.
  - Report rows missing from a file that covers their date.
  - Order rows so the running balance chains; a break is an error. A day with no balance before it (the first, or the one after a break) is put in an order that ends where the next day starts. Where rows could go either way, their order comes from the export with the most rows that day (positions in different files can't be compared), and rows that look alike keep the order they were counted in, so their IDs don't depend on which exports there are.
  - `cutover` picks CSV before a date and API data from it on. Not needed today: the two agree.
- **IDs** (`<prefix>-<date>-<6 hex>`) must never change, because pins refer to them. They're hashed from:
  - booking date, amount and running balance, when there is one;
  - else the bank's reference;
  - else date, amount and description.

  So **don't change how descriptions are derived** for sources without balances or references: it would orphan their pins. That's why showing the merchant name instead of a bare reference number is done only in the output (§7), never on the transaction.
- **Opening balances** are derived from the first running balance (or a snapshot) and booked at `books.start` (or the source's `start`) against `equity:opening-balances`. Moving `start` re-derives them.
- **Freshness warnings:**
  - Fetched data older than 35 days, or exported data older than 90.
  - A year before the bank's history window passes the last download: the books can go a year without being looked at, and history lost from Nordea can't be fetched again.
  - Likewise a year before it passes history that was never downloaded: from a source's start to where its data begins (when that's more than a week and the opening balance isn't zero), or all of it for a source without data, imported or not. The way out, besides downloading, is a later `start`. A beginning the bank has already forgotten is left to the opening balance remark.

Data quirks worth knowing:
- From November 2025, Nordea card payments through Enable Banking have a bare reference number as description; the merchant is only in the counterparty name. Rules match both (`when.any = [{ description_re = … }, { counterparty_name_re = … }]`).
- MobilePay transfers read `MobilePay <full name>`.

## 5. Rules

- **Pins** (`[pin."<id>"]`) map one transaction and always win. **Pattern rules** (`[[rule]]`) match on conditions:
  - `source`;
  - `description` / `description_re`;
  - `counterparty` / `counterparty_name` / `counterparty_name_re`;
  - `direction`, `amount`, `amount_range`;
  - `date`, `date_from`, `date_to`;
  - `raw.<field>`;
  - `any`, `not`.

  Two pattern rules matching the same transaction is an error unless `priority` separates them. Rules or pins that match nothing are reported as dead.
- **Prefer a rule when the merchant always means the same thing,** a pin when it's a one-off or the meaning varies. Payments to people are pinned one by one, because a person isn't a category. A rule can be limited by dates when a counterparty meant one thing for a while (a landlord's late fees for a few months, say).
- **Outputs:**
  - `account`.
  - `split`: fixed amounts plus `rest`.
  - `transfer = "<source>"` (§6).
  - `subscription = "<name>"` (§8).
  - `unknown = "<reason>"`: won't map, booked to `expenses:unknown` / `income:unknown` and listed.
  - `ignore = "<reason>"`: for pairs that cancel out, both sides.
  - `transactions`: the general form, with `@bank` and `@amount`, and dates that may differ from the booking, e.g. prepaying and then expensing later.

  Each output can also set `payee`, `note` and `tags`, except the tags konter writes itself (`bank-id`, `rule`, `purchased`, `original`, `transfer`, `transfer-pair`, `subscription`, `pays`, `source`). Amounts are plain hledger posting amounts: positive debits the account. A rule can't be named `pin`, which is what pins are reported as.
- **Unmatched** transactions go to `expenses:unmatched:<source>` or `income:unmatched:<source>`. The source's `unmatched` policy decides how loudly: `error` suits a main account where everything should be mapped, `allow` a spending account full of small purchases that are mapped as far as is worthwhile.
- **Later marks** (`[later."<id>"]`, with an optional `note`) say "known about, not now". The transaction stays booked to the unmatched account and still counts as unmapped in the gaps report, where it's marked "later" with its note. But it's acknowledged: it doesn't count against the source's `unmatched` policy, and `konter unmatched` leaves it out. A mark isn't an output, so it's not a final answer like `unknown`: once a rule or pin maps the transaction, the mark is reported as stale and should go. A mark can't hide an ambiguous or invalid match, and a mark for no transaction is dead, like a pin.
- **Trips** (`[trip.<name>]` with `account`, `from`, `to` and an optional `note`; `trips.toml`) declare a trip's dates, so purchases during it can be recognised as they come in, before any of them are mapped. Like later marks, a trip isn't an output: it maps nothing by itself. Its account must be declared.
- **Comments in rule files record why**, often knowledge only the user has ("the meal boxes were for Sam, who paid them back"). Write what was said and don't guess beyond it; when someone's pronouns aren't known, write around them.

## 6. Transfers between your own accounts

- **Both accounts have data:** `transfer = "<source>"` books each side against `assets:transfers`, and the two sides are paired. They pair when the amounts are opposite and the dates are at most `transfer_window_days` apart, closest first, and get `transfer-pair` tags. A side that closest-first leaves without a partner still gets one if others can move to their next closest to free one, so as many sides as possible are paired. An unpaired side is an error inside both accounts' covered dates, and a warning at the edges.
- **The other account has no data:** the side is booked straight to that account's ledger account.
- **A labelled transfer stays a plain transfer.** A transfer like "To Spending: Lisbon trip" doesn't carry the expense. The purchases it paid for, in the spending account, are mapped to the trip. Booking the expense on the transfer would count it twice once those purchases are mapped.
- A typical layout: spending accounts topped up from a main account, an account holding money committed to charity until it's given away (`set_aside`, §10), and savings.

## 7. Output: journals and findings

- **`generated.journal`:**
  - The header records `--as-of`.
  - Then the commodity and every account: those in `config/accounts.toml` and the tool's own.
  - Then the whole books (below) in booking order, so that the balance assertions hold. Hand-written transactions are written with every amount filled in, keeping their `balance` as an assertion, so hledger checks what konter worked out.
- **Payee:** the rule's `payee`; else the counterparty name, when the description is only a reference number; else the bank's description. When the payee differs from the bank's text, the text is kept as a `; bank: "…"` comment, quoted as JSON, which is how the report reads it back.
- **Foreign-currency purchases** are booked in the account's currency, with the original amount as a tag (`original:-800.00 SEK`). Cost notation would mix currencies in every report unless hledger is run with `-B`.
- **`forecast.journal`** holds periodic transactions for future subscription due dates, for `hledger … --forecast`.
- **The whole books** (`Build.books`) are what `generated.journal` holds, in memory: the generated transactions and the hand-written ones, not the forecast. They're in journal order: by date, and within a date openings, subscription due dates, bank transactions in chain order, hand-written transactions in file order, then balance snapshots. Every amount is filled in. Every build checks what `hledger check --strict` would: every transaction balances, every balance assertion holds in that order, and every account and commodity is declared.
- **Findings** (`findings.rs`, printed by every build) are what needs attention:
  - **Errors:** ambiguous matches, broken chains, file conflicts, manifest mismatches, undeclared accounts or commodities, unbalanced transactions, failed balance assertions, unpaired transfers, unmatched under an `error` policy, and hledger failures (`konter verify`).
  - **Warnings:** deficits, overpayments, dead rules, stale data, expiring consents.
  - **Info and ok:** everything else.

  The journal is written even with errors, unless inputs can't be read at all. Errors make the exit code non-zero, and `--strict` treats warnings as errors too.

## 8. Subscriptions

They describe what *should* happen on a schedule, so a missing payment shows up.

- **Booking:**
  - Each due date up to `--as-of` books the price to the subscription's expense or income account. Due dates on the same day, and the forecast, come in subscription name order, whatever files they're in.
  - The other side goes to `liabilities:payable:<name>` (or `assets:receivable:<name>`).
  - Payments (`subscription = "<name>"`) settle it, so the balance is 0 when all is paid.
  - Fields: `every` (at most 100 years), `first`, `end` (last due date, inclusive), `prices` (history), `grace` (7 days), `notice`, `opening`.
- **Checks:**
  - Deficit: the oldest unpaid due date is more than the grace period old.
  - More than one period paid ahead: paid twice?
  - Paid after `end`. A phone plan that keeps charging after you cancelled it shows up this way, as an overcharge to chase.
- **`variable = true`** (electricity, prices in foreign currency):
  - Prices are estimates.
  - Each payment settles one due date, oldest first, and that due date books what was paid. An unpaid one books the estimate.
  - So refunds go straight to the expense account, and `opening` isn't allowed. A refund mapped to the subscription is a warning: it would settle a due date as a negative bill and move every later payment a period on.
- **Transfer subscriptions** (`from`/`to` sources, e.g. monthly donations):
  - They expect money to move between your own accounts, so due dates book nothing.
  - The transfers are counted against what was due and tagged `pays:<name>`. Any amount at any time is fine.
- **A paused subscription** is two subscriptions with end dates (`streaming-2025`, `streaming-2026`), since a schedule can't pause.
- **Due day:** the day the bank charges (the purchase date, which is often a few days before booking).
- **Overview:** `konter subscriptions` shows each one's balance, what it's paid until and the commitment from its `notice`.

## 9. Money with other people

Not engine features, but conventions for the rules (kept together in a file such as `nordea/spending-shared.toml`):

- **Paid for someone, paid back:** both sides go to `assets:receivable:<person>` and net to 0. This covers a dinner paid for a friend who sends their share by MobilePay, meal boxes ordered for a flatmate, or a phone bought for a sibling. A partial one uses a `split` pin, e.g. one of two concert tickets.
- **Paid in advance by someone:** `liabilities:owed:<who>`. The same goes for money collected to pass on (`liabilities:owed:club-dinner`).
- **A friend's share of a shared meal or shopping:** credits the expense account it reduces (`expenses:eating-out`, `expenses:groceries:supermarket`).
- **Gifts:** `income:gifts:<who>`, only when it really is a gift (a birthday transfer, a parent's monthly contribution to savings).
- **Trips** get one account each (`expenses:travel:lisbon-2026`, `expenses:conferences:devconf-2026`). Purchases abroad during a trip, meals included, are pinned to it. A trip declared with its dates (§5) gets its purchases marked in the digest.

## 10. Diagnostics and the report

- **Diagnostics** help with mapping and never change the books.
  - `gaps`: where the unmapped money is, by amount.
  - `suggest`: rule stubs for groups of similar unmatched transactions.
  - `coverage`: mapped share per month.
  - `unmatched` (in the CLI): pin stubs.
  - `digest`: a short message of what needs attention, for reading on a phone: the new transactions (unmatched, not marked later), numbered, oldest first, with purchase date, foreign amount and the declared trip they fall in; then the build's errors and warnings, except "not mapped", which the list covers. Prints nothing when there's nothing to say. `--json` adds the transaction IDs behind the numbers and each finding's details, for whoever acts on a reply.
- **The spending report** shows accounts by month, sortable, with the transactions behind each number. It reads the whole books in memory, so hand-written entries count too.
  - Subscriptions billed less often than monthly can be spread as monthly cost.
  - Sorting and hiding empty rows always use the amounts as booked, so turning spreading on or off never moves or hides a row.
  - Accounts with `set_aside = true` (a charity account, say) count as money out when money moves in, and what's later paid from them is left out, so nothing counts twice.
  - It's regenerated by every build and isn't committed.
- **The site** (`build/`): a front page leading to the spending page, the income page and the gaps page.
  - The income page is the spending page for income accounts, with income as positive amounts. Money set aside is part of spending only.
  - The gaps page shows the `gaps` text as tables, every group included. The groups, or the single unmapped transactions, sort by any column. → mapped adds up down the table, so the page works it out again for the order shown.
