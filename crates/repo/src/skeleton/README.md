# Books

Books kept with [konter](https://github.com/albertsgarde/konter): bank data in `data/`, how to book it in `config/`, and the hledger journals konter builds from them in `ledger/`.

- `config/sources.toml`: when the books start and which bank accounts feed them.
- `config/accounts.toml`: every account the books use.
- `config/rules/`: how bank transactions are booked.
- `config/transactions.toml`, if needed: the few transactions with no bank row.
- `data/`: raw bank data, never edited, with its hashes in `data/MANIFEST`.
- `ledger/`: written by `konter build`; never edit it.

These books are private. Keep them in a private repo.
