# Rules

Rules turn bank transactions into ledger transactions. The design is in [spec.md](spec.md).

Every `*.toml` file under `config/rules/` is read (and `config/rules.toml`, if there is one), and they're all checked together as one set. Neither the order of the rules nor the file they're in changes the result, so files are purely for organization. Each set of books says in its own `config/rules/README.md` which file holds what.

## Writing rules

- A later mark leaves a transaction unmatched but acknowledged (see `docs/spec.md` §5). Remove it when the transaction gets a rule or pin; the build warns about stale marks.
- A pin (one transaction, by ID) always wins. Otherwise exactly one pattern rule may match, and `priority = n` breaks ties.
- A transaction's ID is the UUID the store of transactions gave it, as `konter unmatched` prints it: `[pin."3f2b8c1e-5d4a-4f6b-9c7d-0a1b2c3d4e5f"]`. Later marks use it too.
- Rule names, pin IDs, later marks, trip names and subscription names must be unique across all the files.
- Amounts are plain hledger posting amounts: positive debits the account.
- Every account used here must be declared in `config/accounts.toml`.
- `konter unmatched` prints a stub for every transaction no rule matches, and `konter explain <id>` shows which rules match a transaction, which file each is in, and why.
