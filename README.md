# konter

A personal accounting tool built on double-entry bookkeeping. *Kontere* is Danish for posting a transaction to its account.
The idea is to get data from the banks (Nordea and Lunar so far), then generate transactions from those and generate reports from the resulting double-entry bookkeeping data.
The hard part is figuring out how to turn bank transactions into double-entry bookkeeping transactions, which most of the time means categorizing them into the right accounts, but not always.
Sometimes a transaction can be split, sometimes it is best modeled as first one transaction and then another, and sometimes two transactions can be merged or even ignored.
My idea is to build a set of _rules_ that each take a bank transaction and generate 0 or more double-entry bookkeeping transactions from it.
Each bank transaction has a set of attributes, like amount, date (often multiple dates), description, from, to, and so on.
The rules will have these to go on.
The system should also have a way to clearly tell me what bank transactions failed to get matched by any rules, or anything else the system could not handle so I can go in and fix it manually.
I expect there to be quite a few rules, and especially a lot of them will be specific to a single transaction.

## Reproducibility
The system should be totally reproducable.
If I give it the same set of rules (ideally defined in a single file (or directory I guess if we need that kind of structure)) and same set of bank transactions, it should always produce the same set of double-entry bookkeeping transactions.
Moreover, the way bank transactions are fetched (which sadly may involve me doing manual steps) should be clearly documented, and reproducable so refetching the data should give the exact same set of bank transactions, except for new transactions that have happened since the last fetch.

## Code
If custom code is needed, I strongly prefer it to be written in Rust, except if the code can be made much simpler by using another language that has a better ecosystem for the task at hand.
In general, using good existing libraries is preferred over writing custom code if it can do the same job.
E.g. `hledger` seems like a good fit.

## Subscriptions
One of the beauties of double-entry bookkeeping is how it tracks the difference between what should have happened and what actually happened.
One tool for describing what should have happened that I really like is "subscriptions".
Basically a subscription is a recurring transaction that happens at a regular interval, e.g. every month.
It will not touch the bank account account directly, but will create a deficit somewhere if the bank transaction does not happen.
Examples are rent, electricity, and Spotify.

## Uncategorized
The system should be robust to incomplete mapping.
I expect mapping of specifically an everyday spending account to stay incomplete forever, and the system should be able to handle that gracefully.
On top of that, I should also be able to mark a specific transaction as one I won't map, e.g. because I don't know how to.
Whether that is it's own thing or is modelled in the double-entry bookkeeping data is up to you.

## Data
I think it's best if all the data is tracked in VCS.
We probably wanna use Enable Banking for getting the data if they support Nordea and Lunar.
## Setup
The tool and the books are two repos. This one is the tool; the books (bank data, config, rules and the generated ledger) are a private repo of their own in `books/`, which this repo ignores.

1. Clone this repo, and clone your books into `books/` inside it (or set `KONTER_BOOKS` to wherever they are). If you have none yet, step 3 makes new ones.
2. With Nix, `nix develop` (or direnv, through `.envrc`) brings the Rust toolchain, hledger, `just` and `cargo-insta`. Without it, install those yourself: Rust 1.85 or newer, and hledger for `konter verify`.
3. Run `just setup` once. It puts in place the guards that keep `books/` out of this repo's history: `.git/info/exclude`, jj's `snapshot.auto-track`, and a git pre-commit hook. The recipes refuse to run until it has. If there's no `books/`, it then makes new, empty books there with `konter init`.
4. `just` lists the daily recipes and `just more` the rest. They run the tool's working copy on the books.

[docs/fetching.md](docs/fetching.md) says how to get bank data, [docs/usage.md](docs/usage.md) how to use the tool, and [docs/rules.md](docs/rules.md) how to write rules.
