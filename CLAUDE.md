# rama-rs — for agents and contributors

Same conventions as genealogy-rs and diskmap.

## Layout

```
crates/graph  rama-graph: topo_order + layout → Rows (lane, colour, up/down edges). Pure, no git.
crates/repo   rama-repo: Repo::discover / history / changes / diff over gitoxide. No UI.
crates/i18n   rama-i18n: embedded translation catalogs, t!/tn!/tv! macros, i18n guard tests
crates/app    `rama` desktop app (GPUI + gpui-component)
```

## Rules

- **Read-only.** rama never writes to a repository. Any future write (checkout, commit, …) needs
  its own design and an explicit confirmation step.
- **i18n is mandatory.** No user-facing string literal in `app/src`, `repo/src` or `graph/src`;
  use `i18n::t!` / `tn!` and add the key to **every** `crates/i18n/locales/<lang>/` catalog in the
  same change. Untranslatable literals end their line with `// i18n-ok`.
- Every graph/repo change ships with a test (`crates/graph/src/lib.rs`, `crates/repo/tests/`).
  Repo tests build fixtures with the `git` CLI.
- Comments: sparse, only non-obvious why.
- GPUI pins: `Cargo.lock` pins the same zed / gpui-component revs as genealogy-rs. Building only
  `rama-repo` can prune them from the lock; restore it before building the app.

## Validate

```
cargo test -p rama-i18n -p rama-graph -p rama-repo -p rama-app
cargo clippy --workspace --all-targets
```
