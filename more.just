# Recipes not used day to day. Run them as `just more <recipe>`.

set positional-arguments

# The books: their own repo, in books/ unless KONTER_BOOKS says otherwise.
books := env("KONTER_BOOKS", "books")
journal := books / "ledger/main.journal"
# The tool's working copy, unless KONTER says otherwise (`just pinned` does).
konter := env("KONTER", "cargo run --quiet --release --manifest-path " + quote(justfile_directory() / "Cargo.toml") + " --") + " --root " + quote(books)

[private]
default:
    @just --list more

# Rebuild the books (ledger/generated.journal) and the site in build/.
build *args: _check-setup
    {{konter}} build "$@"

# Write the site in build/ (spending, gaps) without opening it.
report: _check-setup
    {{konter}} report

# konter verify runs `hledger check --strict`. With --push, the pinned konter
# has to be on GitHub too, so the pin works on any machine: use it before
# pushing the books.
[doc("Everything consistent: the journals are up to date and sound, with the working copy and with the pin.")]
check *flags: _check-setup && (_check-pin flags)
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

# The pin is what the books promise to build with, so it has to build the
# journals as they are. That catches rules that use something new in the tool
# without moving the pin. A local pin is fine while working; the books' pushed
# history needs one that works on any machine, hence --push.
_check-pin flags="":
    #!/usr/bin/env bash
    set -euo pipefail
    case "$1" in
        "") push=false ;;
        --push) push=true ;;
        *) echo "check takes only --push, not $1" >&2; exit 1 ;;
    esac
    pin="$(realpath {{quote(books)}})/pin"
    if [ ! -f "$pin/flake.lock" ]; then
        echo "{{books}}/pin/ has no pin, so the books build with whatever konter runs them (\`just pin\` pins one)."
        exit 0
    fi
    if $push && jq -e '.nodes.konter.locked.type != "github"' "$pin/flake.lock" > /dev/null; then
        echo "{{books}}/pin/ pins a konter that isn't on GitHub: push it, then \`just pin $(jq -r .nodes.konter.locked.rev "$pin/flake.lock")\`." >&2
        exit 1
    fi
    echo "With the pinned konter ($(jq -r .nodes.konter.locked.rev "$pin/flake.lock")):"
    nix develop "path:$pin" --no-update-lock-file -c konter --root {{quote(books)}} verify

_check-setup:
    @scripts/check-setup
