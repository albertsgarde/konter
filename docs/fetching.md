# Fetching bank data

This page explains how raw bank data gets into `data/`. **Enable Banking is the main way:** `just fetch` gets new transactions from Nordea and Lunar, then rebuilds. **CSV exports are the backup**, for accounts Enable Banking can't reach (Nordea savings accounts) or when the API doesn't work.

| Bank | Accounts | Main way | Backup |
|---|---|---|---|
| Nordea | current accounts | Enable Banking | CSV export from netbank |
| Nordea | savings accounts (Opsparing) | CSV export from netbank (savings accounts aren't on Enable Banking) | – |
| Lunar | all | Enable Banking | CSV export from the app or web bank |

Nordea only keeps 540 days of history, both in the API and in netbank. Anything older exists only in files already in `data/`, so an export kept from earlier can reach further back than netbank does today. `konter build` warns a year before the window passes a Nordea account's last download.

Everything under `data/<bank>/` is raw data exactly as the bank delivered it. Never edit those files: `data/MANIFEST` records a SHA-256 for each file, and `konter build` refuses files that don't match.

## Enable Banking (Nordea and Lunar)

### One-time setup

1. Log in to the Enable Banking Control Panel and register an application under *API applications*:
   - Environment: **Production**. Key: let the browser generate it.
   - Fill in the required fields (name, description, data protection email, privacy policy and terms URLs).
   - Redirect URL: any URL works, e.g. `https://localhost/konter`. Nothing needs to answer there: after logging in at the bank you copy the URL from the address bar.
   - The private key is saved to your downloads as `<application id>.pem`.
2. Move the key out of Downloads and lock it down:
   ```sh
   mkdir -p ~/.config/konter && mv ~/Downloads/<application id>.pem ~/.config/konter/ && chmod 600 ~/.config/konter/*.pem
   ```
3. Create `~/.config/konter/enable-banking.toml` (outside the repo, never committed):
   ```toml
   application_id = "<application id>"
   private_key = "~/.config/konter/<application id>.pem"
   redirect_url = "https://localhost/konter"   # exactly as registered
   ```
4. Activate the application in restricted mode by linking your own accounts, as the Control Panel describes. Restricted mode is free, and only works for accounts you've linked yourself.
5. Check the banks' names: `just more konter banks` should list `Nordea` and `Lunar`. If a name differs, change `[banks.<bank>]` in `config/sources.toml`.

### Linking (every ≤ 180 days)

```sh
just more konter link nordea
just more konter link lunar
```

Each prints a URL. Open it, log in with MitID, then paste the URL you land on back into the terminal. The session is stored in `~/.config/konter/sessions.toml`, and the command lists which linked accounts match which sources. `konter build` warns two weeks before a consent expires.

Without a terminal to paste into (on a server, through the relay bot in [server.md](server.md)), linking takes two steps:

```sh
just more konter link nordea --start                      # prints only the URL
just more konter link nordea --finish '<the URL you land on>'
```

`--start` keeps what the redirect has to match in `~/.config/konter/pending-links.toml` until `--finish` uses it. The URL you land on holds a single-use code that, until it's used, links your accounts: send it only over an encrypted channel.

### Fetching

```sh
just fetch                         # all sources with "enable-banking" in their formats, then a build
just fetch --source nordea:spending  # one source
```

Each fetch gets transactions from 30 days before the last fetched transaction (or from the source's start the first time), plus the current balance. A fetch with no new transactions isn't stored, unless the last stored fetch is over 30 days old (so quiet accounts still record how far their data reaches). Fetching, `add-export` and `build` never change or remove existing data, so they're safe to run any time. The raw responses are written to `data/<bank>/<account>/eb-<timestamp>.json` and recorded in the MANIFEST. `null` fields are dropped (the API lists every field and fills in few), and each transaction is on its own line, which keeps files about a third of their raw size. Pending transactions are ignored when reading.

**What we found on the first fetch (2026-11-08):** Nordea's API has running balances, so its rows match the CSV exports exactly. Nordea only goes back 540 days. Lunar's API has no running balances, but its `entry_reference` is the CSV export's "Transaction ID", so its rows match the CSV by that instead. Neither bank needs `cutover`.

For a new bank, check `just more konter build` after the first fetch:
- If the API data has running balances, the report still says "running balance unbroken". API rows then get the same IDs as CSV rows, and the two formats check each other.
- If it reports overlap conflicts (no running balances and no shared bank reference), set `cutover = <date of the first fetched transaction>` for the source in `config/sources.toml`. CSV data is then used before that date and API data from it on. Pick it once: moving it changes the IDs of transactions in between (dead-pin warnings would show which pins are affected).

## CSV exports (backup)

Use these for Nordea savings accounts, which Enable Banking can't reach, and for any other account when fetching doesn't work (an expired consent you can't renew right away, an API outage). CSV and API data for the same account match up, so mixing them is fine.

After getting a file, add it with:

```sh
just more konter add-export <file> [--source <source>]
```

This copies the file into the account's directory, records its hash in `data/MANIFEST` and checks it against the existing data straight away. Then run `just more build`.

The tool accepts any number of files covering any date ranges. Overlapping the previous file by a few weeks is recommended: the overlap is how the tool checks that a re-export gives the same transactions as before. Where the bank gives a running balance (both Nordea and Lunar CSVs do), gaps between files are caught by the balance chain even without overlap.

For an export without a running-balance column, add `--balance <amount>` (and `--balance-date <date>` if it isn't today's balance). That records the balance the bank showed, in a `.balance.toml` file next to the export, and it becomes a balance check. Neither Nordea's nor Lunar's CSV needs it.

To import an account for the first time, give it `number`, `id_prefix` and `formats` in `config/sources.toml` (see the placeholders there) before running `add-export`.

### Nordea: netbank

<!-- TODO: fill in the exact menu path in netbank. -->
1. Log in to Nordea netbank and open the account.
2. Choose to export/download transactions as CSV, and pick the date range.
3. Any range works, and one export can cover everything netbank has. Netbank only goes back 540 days, so there's nothing older to get.

Facts about the file (checked against real exports):

- Filename: `Konto <account number> - <YYYY-MM-DD HH.MM.SS>.csv` (savings accounts: `Opsparing <account number> - …`). The account number and export time are in the name, so the file is stored under that name. `add-export` reads the account number from it, so `--source` isn't needed.
- UTF-8 with BOM, `;`-separated, comma decimals, newest transaction first.
- Columns: `Bogføringsdato;Beløb;Afsender;Modtager;Navn;Beskrivelse;Saldo;Valuta;Afstemt`.
  - `Bogføringsdato` is `YYYY/MM/DD`.
  - `Afsender`/`Modtager` contain the own account number on one side and sometimes the counterparty's IBAN on the other.
  - `Saldo` is the running balance after the transaction.
- Pending transactions: if a row's date column says `Reserveret`, it is skipped.

### Nordea savings accounts (Opsparing)

Savings accounts aren't available through Enable Banking (Nordea doesn't expose them there), so netbank CSV exports are their only source. Export it as above.

When to export again (`konter build` reminds you):
- **Before the 540-day window passes the last export.** Netbank only goes 540 days back, so a later export can't fill a gap older than that. `konter build` warns ("download soon") a year before the deadline, i.e. once the last export is about 6 months old.
- **When it's 90 days old** ("stale data"), so transfers to and from your other accounts keep pairing.
- **After interest is credited around New Year**, so the year's interest is in the books.

### Lunar: app or web bank

1. In the Lunar app (or web bank), open the account.
2. *More → Details → Export Statement*, choose CSV and the date range.
3. Add it with `just more konter add-export <file> --source lunar:<account>`. The file doesn't say which account it's from, so `--source` is required.

Facts about the file (checked against a real export):

- UTF-8 with BOM, `,`-separated, newest transaction first.
- Columns: `Date,Time,Title,Amount,Balance,Transaction ID`.
  - `Date` is `DD.MM.YYYY` and `Time` is `HH.MM`.
  - Amounts and balances are quoted, in Danish format (`"1.234,56"`).
  - `Balance` is the running balance after the transaction.
  - `Transaction ID` is a UUID.
- Pending transactions are not included.
- Rows with the same timestamp aren't reliably in order. The tool orders each day by the running balance instead.

## Reference data

`data/reference/transactions.csv` can hold an older categorized export, such as one from Nordea's categorized export option. It is **not** an input; it's kept as reference material, for example to seed rule suggestions.
