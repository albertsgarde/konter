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

# The server's clone works on the books' `auto` bookmark, never on main, and
# never rewrites what it has pushed (docs/server.md). These recipes are the
# only way it commits and pushes, so its Claude needs no jj commands of its own.

# auto follows what's pushed after a fetch, so a rewrite of it on GitHub
# (merged into main, say) is built on, never pushed over. If it was rewritten
# here and there both, it's conflicted and this stops.
[doc("Server: start work on the books' auto bookmark as pushed. Stops if the books have uncommitted changes.")]
auto-start: _check-setup
    #!/usr/bin/env bash
    set -euo pipefail
    b={{quote(books)}}
    jj -R "$b" git fetch --quiet
    if [ "$(jj -R "$b" log --no-graph -r @ -T empty)" != true ]; then
        echo "{{books}}/ has uncommitted changes: commit them with \`just more auto-push\`, or restore them." >&2
        jj -R "$b" status >&2
        exit 1
    fi
    base=auto
    jj -R "$b" log --no-graph -r auto -T '' > /dev/null 2>&1 || base='trunk()'
    jj -R "$b" new --quiet "$base"

[doc("Server: commit the books' changes to their auto bookmark and push it.")]
auto-push message: _check-setup
    #!/usr/bin/env bash
    set -euo pipefail
    b={{quote(books)}}
    if [ "$(jj -R "$b" log --no-graph -r @ -T empty)" != true ]; then
        jj -R "$b" commit --quiet -m "$1"
        jj -R "$b" bookmark set --quiet auto -r @-
    fi
    if jj -R "$b" log --no-graph -r auto -T '' > /dev/null 2>&1; then
        jj -R "$b" git push --bookmark auto
    fi

# A failed fetch still builds, and the build's own errors are the digest's to
# report, so the data that came in is committed either way. Exits with the
# fetch's status, so the relay can say a bank failed.
[doc("Server: the daily run. Fetch and build on the books' auto bookmark, then commit and push. Run it pinned.")]
daily: auto-start
    #!/usr/bin/env bash
    set -uo pipefail
    status=0
    {{konter}} fetch || status=$?
    {{konter}} build > /dev/null
    just more auto-push ":card_file_box: Fetch $(date +%F)" || exit $?
    exit $status

[doc("Server: once per clone, after `just setup`. Pushed commits become immutable, and the books' server/ settings go in place.")]
setup-server: _check-setup
    #!/usr/bin/env bash
    set -euo pipefail
    immutable='builtin_immutable_heads() | remote_bookmarks()'
    jj config set --repo 'revset-aliases."immutable_heads()"' "$immutable"
    jj -R {{quote(books)}} config set --repo 'revset-aliases."immutable_heads()"' "$immutable"
    # The server's Claude: the books' own instructions, if they have any, and
    # their settings, or else server/claude-settings.json. Both paths are
    # ignored here.
    server="$(realpath {{quote(books)}})/server"
    if [ -f "$server/CLAUDE.md" ]; then ln -sfn "$server/CLAUDE.md" CLAUDE.local.md; fi
    settings="$PWD/server/claude-settings.json"
    if [ -f "$server/settings.json" ]; then settings="$server/settings.json"; fi
    mkdir -p .claude
    ln -sfn "$settings" .claude/settings.local.json
    echo "Set up for the server: Claude's settings are $settings."
