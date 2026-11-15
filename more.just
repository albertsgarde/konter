# Recipes not used day to day. Run them as `just more <recipe>`.

set positional-arguments

# hledger version the books are checked with (major.minor).
hledger_version := "1.51"
# The books: their own repo, in books/ unless KONTER_BOOKS says otherwise.
books := env("KONTER_BOOKS", "books")
journal := books / "ledger/main.journal"
konter := "cargo run --quiet --release -- --root " + quote(books)

[private]
default:
    @just --list more

# Rebuild the books (ledger/generated.journal) and the site in build/.
build *args: _check-setup
    {{konter}} build "$@"

# Write the site in build/ (spending, gaps) without opening it.
report: _check-setup
    {{konter}} report

# Everything consistent: committed journal up to date, and hledger agrees the
# books are sound (konter verify runs `hledger check --strict`).
check: _hledger-version _check-setup
    {{konter}} verify

test:
    cargo test

# Run any konter subcommand.
konter *args: _check-setup
    {{konter}} "$@"

# Run hledger on the books.
hl *args: _check-setup
    hledger -f {{journal}} "$@"

# Monthly income statement.
is *args: _check-setup
    hledger -f {{journal}} is -M "$@"

# Balance sheet.
bs *args: _check-setup
    hledger -f {{journal}} bs "$@"

# Monthly expenses, two levels deep (hledger's table).
exp *args: _check-setup
    hledger -f {{journal}} bal '^expenses' -M --depth 2 --tree "$@"

# Unmapped and won't-map amounts per month.
unmapped *args: _check-setup
    hledger -f {{journal}} bal expenses:unmatched income:unmatched expenses:unknown income:unknown -M "$@"

# Monthly balance changes including future subscription due dates.
forecast *args: _check-setup
    hledger -f {{journal}} bal -M --forecast "$@"

# Subscriptions: balances, deficits, commitments.
subs *args: _check-setup
    {{konter}} subscriptions "$@"

# Rule suggestions for groups of unmatched transactions.
suggest *args: _check-setup
    {{konter}} suggest "$@"

# Where the unmapped money is, as text: per account, then the largest groups by amount.
gaps *args: _check-setup
    {{konter}} gaps "$@"

# Per month, how much of each account is mapped.
coverage *args: _check-setup
    {{konter}} coverage "$@"

reg account *args: _check-setup
    hledger -f {{journal}} reg "$@"

ui *args: _check-setup
    hledger-ui -f {{journal}} "$@"

# hledger's own web interface.
web *args: _check-setup
    hledger-web -f {{journal}} "$@"

_hledger-version:
    @hledger --version | grep -q '^hledger {{hledger_version}}\.' \
      || (echo "expected hledger {{hledger_version}}.x, found: $(hledger --version)" >&2; exit 1)

_check-setup:
    @scripts/check-setup
