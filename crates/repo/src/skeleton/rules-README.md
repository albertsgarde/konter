# Rules

Every `*.toml` under this folder is a rules file. konter's `docs/rules.md` says how to write rules.

Split the rules by source, one file per bank account (`nordea/main.toml` for `nordea:main`), so that two people or agents mapping different accounts rarely share a file. Split big files by topic (`nordea/main-trips.toml`), not by date: pins that only make sense together, like a payment and its repayment, belong in one file. Subscriptions go in `subscriptions.toml`.

Note here what goes where in these books, as it settles.
