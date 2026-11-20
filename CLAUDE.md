# rama-rs — for agents and contributors

Same conventions as genealogy-rs and diskmap.

## Layout

```
crates/graph  rama-graph: topo_order + layout → Rows (lane, colour, up/down edges). Pure, no git.
crates/repo   rama-repo: Repo::discover / history / changes / diff over gitoxide; status, stage,
              commit, stash, branches, fetch/pull/push via the git CLI (work.rs, git.rs). No UI.
crates/i18n   rama-i18n: embedded translation catalogs, t!/tn!/tv! macros, i18n guard tests
crates/app    `rama` desktop app (GPUI + gpui-component)
```

## Rules

- **Writes only on explicit user actions.** Nothing writes to a repository in the background
  (refreshes use `git --no-optional-locks status` and gitoxide reads). Destructive actions
  (discard, drop stash, delete branch, `--no-verify`) go through a confirmation modal; untracked
  files are moved to the Trash, never deleted. Nothing is ever force-pushed (`--force`,
  `--force-with-lease`, `-D`, `reset --hard` are off limits).
- **Writes go through the `git` CLI** (`crates/repo/src/git.rs`), never gitoxide, so hooks,
  `core.hooksPath`, signing, LFS and credential helpers behave as in a terminal. Every git
  subprocess gets the login-shell environment from `shell_env` (GUI apps lack the shell PATH).
  Long commands run off the UI thread through `git::run` (streamed output, cancel kills the
  process group); a failing hook is identified from git's trace2 events.
- **i18n is mandatory.** No user-facing string literal in `app/src`, `repo/src` or `graph/src`;
  use `i18n::t!` / `tn!` and add the key to **every** `crates/i18n/locales/<lang>/` catalog in the
  same change. Untranslatable literals end their line with `// i18n-ok`.
- Every graph/repo change ships with a test (`crates/graph/src/lib.rs`, `crates/repo/tests/`).
  Repo tests build fixtures with the `git` CLI; `tests/work.rs` isolates global/system git config
  (`GIT_CONFIG_GLOBAL`, `commit.gpgsign=false`) and never touches the real Trash.
- Comments: sparse, only non-obvious why.
- GPUI pins: `Cargo.lock` pins the same zed / gpui-component revs as genealogy-rs. Building only
  `rama-repo` can prune them from the lock; restore it before building the app.

## Validate

Never run write operations against a real repository while developing; use temp repos.

```
cargo test -p rama-i18n -p rama-graph -p rama-repo -p rama-app
cargo clippy --workspace --all-targets
```
