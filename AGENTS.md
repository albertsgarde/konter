## Start here
[docs/spec.md](docs/spec.md) describes what the system does and why: the principles behind the engine, how bank data becomes the books, and the conventions the rules follow. Read it before changing the engine or the rules, and update it when behaviour changes. [docs/rules.md](docs/rules.md) says how to write rules.

## Two repos, one project
This repo is the tool, `konter`, and it's public. The books it works on are a separate, private repo in `books/`, which this repo ignores:

```
./          the tool: crates/, docs/, justfile, more.just, server/
books/      the books: config/, data/, ledger/
```

- **Mapping work follows [books/AGENTS.md](books/AGENTS.md).** Read it before touching anything in `books/`.
- **Never let anything from `books/` or `human_notes/` into this repo**, not even in a commit message: this repo is public. `just setup` sets up the guards (ignore rules, jj's `snapshot.auto-track`, a git pre-commit hook), and the recipes refuse to run until it has.
- **jj acts on the nearest repo.** From this directory, use `jj -R books …` for the books and plain `jj` for the tool, rather than relying on the shell's current directory. With `-R`, paths are still relative to the current directory: `jj -R books diff books/ledger`, not `ledger`.
- **The recipes run on `books/`** (or `$KONTER_BOOKS`) with the tool's working copy: `just more build`, `just more konter <subcommand>`. A bare `cargo run -- <subcommand>` from here finds no books; add `--root books`.
- **`crates/relay` is the server's Matrix bot** ([docs/server.md](docs/server.md)), with `server/` holding its systemd units, the Claude settings and the push guard. It isn't a default member, so plain `cargo build` and `cargo test` skip it and its many dependencies: use `-p konter-relay`, or `--workspace`.
- A change to the tool shows up in the books on the next run. Commit the tool change here and the books' change in `books/`, each with its own message.
- **The books are pinned to a tool commit** in `books/pin/`. `just pinned <recipe>` runs with that tool, and `just pin` moves the pin ([docs/usage.md](docs/usage.md)). When the books start to need a tool change, move the pin and commit `books/pin/` with the books' change.
- Plans and design history live with the books too, if they have any (mine: `books/PLAN.md` and `books/docs/plans/`).

## Version control
I use JJ for version control with a git backend, in both repos.
You can and should create new revisions on top of the one I asked you to work on (by splitting, see below) and add descriptions to those, but never change existing commits other than the one I tell you to work on, unless that is very explicitly requested.
The exception is a commit you just created.
You can assume others don't touch it (because you won't touch their's), but only change it by squashing things from the current commit.
_NEVER_ push anything to git, though you are welcome to fetch using `jj git fetch`.


### Other agents work here at the same time
Several agents often run in this project at once, in the same working copies (later maybe in separate jj workspaces). Respect each other's work, and expect things to change around you. All of this applies in both repos:

- **Files change under you.** If a file differs from what you last read, another agent changed it: read it again and build on it, and never revert changes you didn't make. If a build or test fails in code you didn't touch, it's probably someone else's work in progress: tell me rather than fixing it.
- **The working copy isn't yours alone.** `@` holds everyone's uncommitted changes. Never move it or rewrite it as a whole: no `jj new`, `jj edit`, `jj describe` or `jj abandon` on `@`, and no `jj undo` or `jj op restore`. Each of these either changes files on disk or puts other agents' work into your commit. Read-only commands (`jj st`, `jj log`, `jj diff`) are fine.
- **`jj restore <files>` is just a write**, so it's fine for specific files. It replaces the whole file, though: check with `jj diff <file>` that every change in it is yours, or you'll revert someone else's work. Never run it without paths.
- **Commit by splitting out your own files.** `jj split -m "<message>" <your files>` puts just those files into a commit below the working copy and leaves everything else, including the files on disk, as it was. If another agent has changed one of the same files, a split by path can't separate your changes from theirs: ask me.
- **Revisions move.** Don't assume the commit you just made is still `@-`: another agent may have split theirs in since. Refer to your commits by change ID, and check `jj log` right before acting on one (e.g. `jj squash --into <change-id> <your files>` to add to it).
- **Divergence happens.** jj commands running at the same time can leave a change divergent (two copies with one change ID). Don't abandon or resolve the copies yourself; tell me what you see.
