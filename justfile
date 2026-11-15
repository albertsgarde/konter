# The recipes in daily use. Everything else: `just more` (more.just).

set positional-arguments

# The books: their own repo, in books/ unless KONTER_BOOKS says otherwise.
books := env("KONTER_BOOKS", "books")
# The tool's working copy, unless KONTER says otherwise (`just pinned` does).
konter := env("KONTER", "cargo run --quiet --release --manifest-path " + quote(justfile_directory() / "Cargo.toml") + " --") + " --root " + quote(books)

mod more
# Recipes that are only for one person's books, if their books have a justfile: `just books <recipe>`.
mod? books

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

# The working copy is for changing the tool; the server's timer uses this.
[doc("Any recipe, with the konter and hledger pinned in the books' pin/: `just pinned fetch`.")]
pinned *args: _check-setup
    #!/usr/bin/env bash
    set -euo pipefail
    pin="$(realpath {{quote(books)}})/pin"
    [ -f "$pin/flake.lock" ] || { echo "{{books}}/pin/ has no pin yet: run \`just pin\`." >&2; exit 1; }
    # A path: flake, so Nix copies only pin/ into its store, never the books.
    exec nix develop "path:$pin" --no-update-lock-file -c env KONTER=konter just "$@"

# It's a GitHub pin if the commit is there, and otherwise a local one, which
# only works on this machine: run `just pin <commit>` again once it's pushed.
# Commit the books' pin/ with the change that needs the new tool.
[doc("Pin the books to a commit of the tool: by default the last one (@-), or `main` (GitHub's).")]
pin rev="": _check-setup
    #!/usr/bin/env bash
    set -euo pipefail
    pin="$(realpath {{quote(books)}})/pin"
    if [ -d .jj ]; then
        jj git fetch --quiet
        rev="${1:-@-}"; [ "$rev" = main ] && rev=main@origin
        commits=$(jj log --no-graph -r "$rev" -T 'commit_id ++ "\n"')
    else
        git fetch --quiet origin
        rev="${1:-HEAD}"; [ "$rev" = main ] && rev=origin/main
        commits=$(git rev-parse --verify "$rev^{commit}")
    fi
    [ "$(printf '%s\n' "$commits" | wc -l)" = 1 ] || { echo "$rev isn't one commit" >&2; exit 1; }
    commit=$commits
    url=$(git remote get-url origin 2>/dev/null || true)
    case "$url" in
        git@github.com:*) repo=${url#git@github.com:} ;;
        ssh://git@github.com/*) repo=${url#ssh://git@github.com/} ;;
        https://github.com/*) repo=${url#https://github.com/} ;;
        *) repo= ;;
    esac
    repo=${repo%.git}
    if [ -n "$repo" ] && git branch -r --contains "$commit" --format='%(refname)' | grep -q '^refs/remotes/origin/'; then
        ref="github:$repo/$commit"
    else
        ref="git+file://$PWD?rev=$commit"
        echo "$commit isn't on GitHub, so this pin only works on this machine; \`just pin $commit\` again once it's pushed." >&2
    fi
    # Made in a scratch folder, so a pin that doesn't build replaces nothing.
    new=$(mktemp -d)
    trap 'rm -rf "$new"' EXIT
    cat > "$new/flake.nix" <<'EOF'
    # The konter (and hledger) that builds these books. Written by `just pin` in
    # konter: don't edit it, run that. flake.lock holds the exact commit and its
    # hash. It's a flake of its own, run as a path: flake by `just pinned`, so
    # Nix copies only this folder into its store, never the books' data.
    {
      inputs.konter.url = "@REF@";

      outputs =
        { konter, ... }:
        {
          devShells = builtins.mapAttrs (
            system: packages:
            let
              pkgs = konter.inputs.nixpkgs.legacyPackages.${system};
            in
            {
              default = pkgs.mkShell {
                packages = [
                  packages.konter
                  packages.hledger
                  pkgs.just
                ];
              };
            }
          ) konter.packages;
        };
    }
    EOF
    sed -i -e 's/^    //' -e "s|@REF@|$ref|" "$new/flake.nix"
    nix flake lock "path:$new"
    nix develop "path:$new" --no-update-lock-file -c konter --version
    mkdir -p "$pin"
    mv "$new/flake.nix" "$new/flake.lock" "$pin/"
    echo "Pinned {{books}}/pin/ to $ref."

_check-setup:
    @scripts/check-setup
