# The recipes in daily use. Everything else: `just more` (more.just).

set positional-arguments

# The books: their own repo, in books/ unless KONTER_BOOKS says otherwise.
books := env("KONTER_BOOKS", "books")
konter := "cargo run --quiet --release -- --root " + quote(books)

mod more

[private]
default:
    @just --list

# Run once per clone: guards that keep books/ and human_notes/ out of this repo.
setup:
    #!/usr/bin/env bash
    set -euo pipefail
    for path in /books/ /human_notes/; do
        grep -qxF "$path" .git/info/exclude 2>/dev/null || echo "$path" >> .git/info/exclude
    done
    git config core.hooksPath .githooks
    if [ -d .jj ]; then
        jj config set --repo snapshot.auto-track '~(root:"books" | root:"human_notes")'
    fi
    if [ ! -f "{{books}}/config/sources.toml" ]; then
        echo "No books in {{books}}/ yet: clone your books repo there."
    fi
    echo "Set up."

# Open the site's front page, which leads to the others.
web: (_open "index.html")

# Open the spending page: expenses per account and month.
exp: (_open "spending.html")

# Open the gaps page: where the unmapped money is.
gaps: (_open "gaps.html")

# Open the income page: income per account and month.
inc: (_open "income.html")

# The build runs even if a bank fails, so the others' new data still shows up.
# Takes konter fetch's options, e.g. --source nordea:spending.

# Get new transactions from the banks, then rebuild the books and the site.
fetch *args: _check-setup
    -{{konter}} fetch "$@"
    {{konter}} build

# Write the site in the books' build/ fresh from the books, then open one of its pages.
_open page: _check-setup
    {{konter}} report
    xdg-open "{{books}}/build/{{page}}"

_check-setup:
    @scripts/check-setup
