# rama-rs — for agents and contributors

Same conventions as genealogy-rs and diskmap.

## Layout

```
crates/graph  rama-graph: topo_order + layout → Rows (lane, colour, up/down edges). Pure, no git.
crates/repo   rama-repo: Repo::discover / history / changes / diff over gitoxide; status, stage,
              commit, stash, branches, fetch/pull/push via the git CLI (work.rs, git.rs);
              worktrees, base branches, disk usage (worktree.rs); merged / gone / squash
              detection for cleanup (cleanup.rs); disk reclaim (reclaim.rs: pure `plan` over
              sizes + `Repo::reclaim`, cargo target-dir detection). No UI.
crates/i18n   rama-i18n: embedded translation catalogs, t!/tn!/tv! macros, i18n guard tests
crates/app    `rama` desktop app (GPUI + gpui-component). map.rs / map_view.rs: worktree map;
              cleanup.rs: branch cleanup dialog; launch.rs: external terminals and agent CLIs;
              agents.rs: AI agents at work per worktree (processes via sysinfo, Claude session
              files; pure `scan` / `classify` / `Tracker`, tested with fakes); agents_view.rs:
              poll, animation, badge, panel rows, legend; robot.rs: the pixel sprites;
              transcript.rs: session log → conversation items; convo_view.rs: Conversation tab;
              otherwise.rs: otherwise-link (links in, About's other apps, Today feed of agents
              that need you); storage.rs: `<data root>/rama/` + one-time migration from the
              old config folder; vcity.rs: Vcity invitation (About, Help); reclaim.rs:
              "Free up space…" dialog, Today entry > 10 GB, Open in diskmap
```

## Rules

- **Writes only on explicit user actions.** Nothing writes to a repository in the background
  (refreshes use `git --no-optional-locks status` and gitoxide reads). Destructive actions
  (discard, drop stash, delete branch, delete remote branch, `--no-verify`) go through a
  confirmation modal; untracked files are moved to the Trash, never deleted. Nothing is ever
  force-pushed (`--force`, `--force-with-lease`, `reset --hard` are off limits). Forced local
  writes happen only after a second, separate confirm: `git branch -D` for an unmerged branch
  (also squash-merged / gone branches in cleanup) and `git worktree remove --force` for a worktree
  with changes (listed in the dialog; `git::run` allows `--force` only there). Locked worktrees
  are never removed. Deleting a remote branch makes the user type its name (bulk cleanup: the
  number of branches). `git gc --prune=now` asks first. Background reads include
  `git worktree list`, `for-each-ref`, `status --no-optional-locks` per worktree, `log -p | patch-id`
  for squash detection and a file walk for sizes; nothing there writes.
- **Free up space** (`repo/src/reclaim.rs`): the plan is pure and deterministic (kind, then path)
  and never includes a worktree with changes / unreadable status / partial size, a locked one,
  the open one, a protected branch's, or a path outside its worktree (or outside the shared
  cargo folder). Merged = every commit on the default branch's remote-tracking ref and on its
  upstream. `Repo::reclaim` re-checks before each item (still clean, still merged, real dir not
  a symlink, build-folder name, inside the owner after canonicalising, not tracked by git) and
  removes worktrees with plain `git worktree remove`, branches with `git branch -d`. Build
  folders (`target/`, `node_modules/`, `.next/`, `incremental/`) are the only thing deleted
  outright instead of moved to the Trash: they're regenerable and the dialog says so. A shared
  cargo folder goes whole only if it carries `CACHEDIR.TAG` and neither its users nor the folder
  itself changed for the configured days. Tests never touch real folders or the Trash.
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
  (`GIT_CONFIG_GLOBAL`, `commit.gpgsign=false`) and never touches the real Trash; `tests/worktrees.rs`
  does the same for worktrees and cleanup. `examples/worktrees.rs` times the map and cleanup scans
  read-only (safe on a real repository).
- Agent badges read, never write: process list (sysinfo), `~/.claude/sessions/<pid>.json` and
  the session logs. For badges only mtime and the last entries' `type` / `stop_reason` / pending
  tool name / error kind / trailing "?" / title are read. The conversation viewer reads a log only
  while the user has its tab open (tail first, capped); its text is shown there and nowhere else:
  never written, sent, cached on disk or logged. Polling happens only while the window is on
  screen and the graph or map shows; the 6 fps redraw only while an agent works or waves and
  motion isn't reduced.
- The Today feed (`otherwise.rs`) carries only agent kind, worktree folder and state, comes from
  the badge poll (no extra polling), expires 30 min after the last poll and is cleared on quit.
  otherwise-link is pinned by git tag; it must only add its own crates to Cargo.lock. Tests use
  temp dirs (`Root::at`, explicit `Locations`). The reclaim entry (repo folder name + size,
  over 10 GB) is republished when its title or expiry day changes, expires after a day, and is
  cleared on quit.
  `<data root>/rama/worktrees.txt` (worktree paths rama listed) is read by diskmap.
- External terminals get the exact command line shown before its first run (remembered per
  terminal + command in settings); commands run through the login shell (`$SHELL -lc`).
- Comments: sparse, only non-obvious why.
- GPUI pins: `Cargo.lock` pins the same zed / gpui-component revs as genealogy-rs. Building only
  `rama-repo` can prune them from the lock; restore it before building the app.

## Validate

Never run write operations against a real repository while developing; use temp repos.

```
cargo test -p rama-i18n -p rama-graph -p rama-repo -p rama-app
cargo clippy --workspace --all-targets
```
