# rama

<img src="crates/app/assets/logo/rama-icon-512.png" width="128" alt="rama icon">

Explore git repositories: every branch and merge as a coloured commit graph, the full history at
a glance, and each commit's changes down to the line. *Rama* is Spanish for *branch*.

Native desktop app in Rust: [gitoxide](https://github.com/GitoxideLabs/gitoxide) reads the
history (no libgit2), the `git` command does every write, and [GPUI](https://www.gpui.rs/) with
[gpui-component](https://github.com/longbridge/gpui-component) draws it. macOS, Windows and
Linux. UI in Spanish, English, Portuguese and French.

## Features

- Commit graph with coloured lanes, curved branch and merge lines, author avatars on each commit,
  branch / remote / tag labels, and the checked-out branch highlighted.
- Fast on big repositories: 60 000 commits and 20 000 refs load in under a second; lists are
  virtualised.
- Sidebar with local branches (the checked-out one pinned first, sorted by name or by latest
  commit), remote branches, tags and stashes; click one to jump to its commit. A filter field on
  top (⌘⇧F, or `/` from the list; Esc clears) narrows all of them as you type, ignoring case and
  matching word starts too (`fe/lo` finds `feature/user-login`), with the matches highlighted and
  counted per section. Instant with thousands of remote branches and tags.
- Commit details: full message, author, date, parents (clickable), refs, and changed files with
  added / removed line counts and rename detection.
- Search (⌘F) across commit messages, authors, emails, branch and tag names and SHA prefixes,
  ignoring case and accents; non-matching commits fade, Enter / Shift+Enter jump between matches.
- Unified diff per file with line numbers.
- Working copy, as in VS Code: a *Working copy* row always pinned at the top of the graph (it
  says *No local changes* when clean) and a *Changes* button with the changed-file count open the
  staged and unstaged files (untracked, renamed and conflicted included) with stage / unstage /
  discard per file or all at once, their diffs, and a commit box (⌘↩, *Amend last commit*).
  Discarding asks first, and untracked files go to the Trash, never straight to deletion.
- Git hooks run exactly as on the command line (see below); a rejected commit keeps your
  message and shows the hook's output. *Commit without hooks (--no-verify)* exists only as an
  explicit, confirmed second step.
- Stashes in the sidebar: stash (with or without untracked files), apply, pop, drop, and the
  files each one holds.
- Branches: double-click (or right-click → *Check out*) to switch; right-click a local branch to
  pull it (fast-forward only, even when it is not checked out), push it, rename it, copy its
  name, delete it, or delete its branch on the remote. Deleting an unmerged branch needs a second
  confirmation (`git branch -D`); deleting a remote branch shows the exact
  `git push <remote> --delete <branch>` and asks you to type the branch name. Right-click a remote
  branch to check it out as a local tracking branch or delete it on the remote.
- *Fetch*, *Pull* and *Push* buttons in the top bar, with the current branch's ahead / behind
  counts and a spinner while they run. Push becomes *Publish* for a branch without an upstream
  and shows the `--set-upstream` command before running it. Nothing is ever force-pushed; a
  failure shows git's and the hook's output above the graph.
- **Worktree map** (*Map*, ⌘M, next to the graph): the main worktree in the centre and every
  worktree linked to it, radially or grouped by the base branch each is closest to. Each card shows
  its path, branch (or detached SHA), ahead / behind against its upstream and the default branch,
  uncommitted changes, last commit, size on disk with a `node_modules` (or `.git`, `target`…)
  breakdown, and flags: this window, locked, prunable (folder gone), merged, upstream gone. Merged
  ones are dimmed; edges use the branch's lane colour. Drag or scroll to pan; ⌘+scroll, pinch or
  ⌘+/− to zoom (⌘0 resets). A list mode sorts by path, size, age or merged for many worktrees.
  Sizes are measured in the background one worktree at a time (nested worktrees count only on
  their own card, hard links once) and cached; cargo target folders kept outside the repository
  (`CARGO_TARGET_DIR` in your login shell, or `build.target-dir` in a `.cargo/config.toml`) are
  measured too and shown in the repository's totals.
- **Free up space…** (map toolbar, totals panel, a worktree's panel) offers, with every path and
  size listed before anything happens: worktrees whose branch is merged into the default branch
  with nothing uncommitted or unpushed (`git worktree remove`, optionally `git branch -d`);
  cargo's `target/**/incremental` caches; the whole `target/` and the `node_modules/` / `.next/`
  of worktrees untouched for 14 days (configurable). Build folders are deleted directly (they are
  regenerated, and the Trash would keep the space); the dialog says so. Worktrees with
  uncommitted changes, locked ones, the one open in the window and protected branches are never
  touched, nothing outside the listed worktree or target folders is deleted, a committed
  `node_modules` is kept, and every rule is checked again right before each delete.
  *Open in diskmap* shows the folder in [diskmap](https://github.com/otherwisetech/diskmap-rs)
  when it's installed.
- Worktree actions (side panel and right-click): open in the file manager, open in a new rama
  window, copy the path, open your terminal there, **start an AI agent there** (`claude`,
  `codex`, `opencode`, `pi`, `gemini`, `aider` when found on your login-shell PATH, or your own
  command list), lock / unlock, and remove (asks first; a worktree with changes or untracked files
  lists them and needs a second *Force remove*; optionally also `git branch -d` its merged branch).
  *Prune…* previews `git worktree prune` before running it. *New worktree…* checks out an existing
  branch, a new branch or a commit into `../<repo>-<branch>` (or a folder you choose) and can start
  an agent once it exists.
- **AI agents at work**: a tiny pixel-art robot on a worktree's map card (and list row), on
  the graph's *Working copy* row and on the ref pill of a branch checked out in a worktree where
  `claude`, `codex`, `opencode`, `pi`, `gemini` or `aider` is running. It types while the agent
  works, **raises a hand** with an amber "!" when the agent needs you (a permission prompt, its
  question tool, or a last answer that ends in a question), rests when it's your turn or the
  agent is idle, and turns **red with a broken face** on an error (rate limit, overload,
  connection, API or sign-in error, or the process stopping mid-work). A legend sits under the
  map; agents that need you sort first; an optional macOS notification tells you once each time
  one starts needing you. Hover for the agent, its state, the session title, how many
  processes serve it, when it started and changed state; click to bring its terminal app forward
  (macOS) or open a terminal in the worktree. Each session is listed once.
  Found every 3 s, in the background and only while a rama window is on screen, from agent
  processes whose working directory is in the worktree (the innermost one, so
  `.claude/worktrees/x` inside the main checkout counts only for `x`) and Claude Code's own
  records: `~/.claude/sessions/<pid>.json` (session id and status) and the session log
  `~/.claude/projects/<path with every non-alphanumeric character as ->/<id>.jsonl`, of which
  the badge reads only the modification time and the last entries' type, stop reason, pending
  tool, error kind, trailing "?" and title. Other agents show working / idle from CPU only.
  Windows: session files only. Turn it off, stop the animation (*Reduce motion*) or turn on
  notifications in the worktree settings.
- **Agent conversations**: in the map's side panel each agent is a row; click it for the
  *Conversation* tab: the session's messages, read-only, newest at the bottom and following new
  ones while you're at the bottom, tool calls folded to one line ("Edited src/x.rs",
  "Ran `cargo test`") that open to show their input and output, a find field, *Load earlier
  messages* (the last 2 MB are read first, up to 64 MB) and *Open in terminal*. The log is read
  only while the tab is open, only from this computer, and never written, sent or logged. Codex
  and the other agents: transcript not available.
- Terminals: Terminal.app, iTerm2, Ghostty, Warp, WezTerm, kitty and Alacritty on macOS;
  `$TERMINAL`, GNOME Terminal, Konsole, kitty, Alacritty and WezTerm on Linux; Windows Terminal,
  PowerShell and cmd on Windows. The exact command line is shown the first time each terminal +
  command pair is used. Warp can't be handed a command, so it opens at the folder and the command is
  copied for you to paste. There is no embedded terminal.
- **Clean up branches…** (Local branches header ⋯ menu, or the map): lists local branches merged
  into the default branch (`origin/HEAD`, else main / master / dev / develop, or the one you set),
  with their upstream gone, or squash- / rebase-merged (every patch, or the branch's whole diff, is
  already on the default branch by `git patch-id`; "maybe" when only the touched files match, left
  unchecked). Never listed: the checked-out branch, the default branch and protected patterns
  (`main`, `master`, `dev`, `develop`, `release*`, `trunk` by default). Branches checked out in a
  worktree are only deleted if you also tick *remove the worktree* (and it has no changes).
  `git branch -d` where it works; the rest need `-D` behind a second confirmation; deleting their
  remote branches is a separate, default-off option that shows the `git push --delete` commands and
  asks you to type the count. *Fetch & prune first* refreshes what is gone. Deleted branches can be
  restored for the rest of the session (`git branch <name> <sha>`).
- Disk space, honestly: deleting branches frees almost nothing; the space is in worktrees (sizes
  on the map) and unreachable objects. *Run git gc* shows `.git` before and after;
  `--prune=now` asks first.
- *Git output* panel with the running command, live output from git and its hooks, and Cancel.
- Refreshes on its own when files change on disk or the window regains focus.
- Keyboard: ↑/↓ or j/k to move, ⌘F to search, Esc to close a diff or clear the search, ⌘C copies the commit SHA, ⌘O opens a repo,
  ⌘R reloads, ⌘M switches between the graph and the worktree map.
- Writes only when you ask: every change to a repository is an explicit action, the destructive
  ones ask for confirmation, and nothing is ever force-pushed.

## Git hooks and PATH

Commit, stage, stash, checkout, fetch, pull and push run the `git` you have installed, so
`pre-commit`, `commit-msg`, `prepare-commit-msg`, `post-commit` and `pre-push` hooks,
`core.hooksPath` (husky v9's `.husky/_`, and with it `~/.config/husky/init.sh`), commit signing,
Git LFS, credential helpers and your config behave exactly as in a terminal. Viewing history
does not need `git`; if it is missing, rama says so and the write actions stay disabled.

Apps opened from Finder, the Dock or a desktop launcher don't get the PATH your shell sets up,
which is why hooks that call `node`, `npx` or `pnpm` fail with "command not found" in many GUI
git clients. At startup rama asks your login shell for its environment once, in the background
(`$SHELL -l -i -c env`, with a timeout; zsh, bash and fish), and uses it for every git command,
so nvm, fnm, volta, asdf, mise and Homebrew installs are found. If that fails it falls back to
the inherited PATH plus the usual install folders. On Windows the inherited environment is used
as is. To see the PATH rama will use: `cargo run -p rama-repo --example shell_path`.

git runs without a terminal: password prompts are off (`GIT_TERMINAL_PROMPT=0`), so use a
credential helper or an ssh agent for remotes that need a login.

## Download

Ready-made builds for macOS (Apple silicon and Intel), Linux and Windows are attached to each
[GitHub Release](https://github.com/otherwisetech/rama-rs/releases).

- **macOS:** unzip and drag `rama.app` to Applications. The app is not notarized yet, so the
  first time right-click it → *Open* (or run `xattr -dr com.apple.quarantine /Applications/rama.app`).
- **Linux:** extract the `.tar.gz` and run `./rama`; copy `rama.desktop` and the icon to
  `~/.local/share/applications` / `~/.local/share/icons` for a menu entry. Needs Vulkan.
- **Windows:** unzip and run `rama.exe` (SmartScreen may ask to confirm, since it is unsigned).

The downloads are built by GitHub Actions from the tagged source, but they are **not code-signed
or notarized**. Fetch, pull and push run `git`; the only request rama makes itself is reading the
project's star count (`api.github.com/repos/otherwisetech/rama-rs`, no login, nothing about you or
your repositories) at most once a day, shown next to *Star on GitHub*. Turn it off in the globe
menu (*Show GitHub star count*).

## Works with other Otherwise apps

rama uses [otherwise-link](https://github.com/otherwisetech/otherwise-link), the small layer
Otherwise Technologies' apps share. Everything stays on your computer.

- **Links.** rama opens `otherwise-rama://` links (also `otherwise://rama/…` through
  [Otherwise Apps](https://github.com/otherwisetech/otherwise-apps)); paths are absolute and
  percent-encoded:

  | Link | Does |
  |------|------|
  | `otherwise://rama` | brings rama to the front |
  | `otherwise://rama/repo?path=/Users/me/code/app` | shows that repository (the window that has it, else a new one) |
  | `otherwise://rama/agent?worktree=/Users/me/code/app-wt&session=<id>` | opens the worktree map and that agent's Conversation tab |
  | `otherwise://rama/reclaim?path=/Users/me/code/app` | shows that repository's worktree map and *Free up space…* |

  rama opens `otherwise://diskmap/scan?path=…` for *Open in diskmap*.

  macOS registers the scheme from the app's `Info.plist`; Windows registers it for your user
  (`HKCU\Software\Classes\otherwise-rama`) when rama starts; on Linux `rama.desktop` declares
  `x-scheme-handler/otherwise-rama` and rama makes itself the handler when that file is in
  `~/.local/share/applications`.
- **Today.** Agents that wait for you (a question or a permission prompt, the raised hand) or lost
  their connection show up in Otherwise Apps' *Today* view, linking to their conversation. Only
  the agent's name, the worktree's folder and what it waits for are written
  (`today/rama.json` in the shared Otherwise folder), never titles or conversation text. The
  list comes from the same poll as the badges, so it is only refreshed while rama is on screen;
  entries expire 30 minutes after the last poll and are removed when rama quits. Turn it off in
  the worktree map's settings. A repository open in rama that could free more than 10 GB gets an
  entry too (its folder name and the size only), linking to *Free up space…*; it expires after a
  day, is removed on quit and has its own switch in the same settings.
- **About** lists the other Otherwise apps installed on this computer; click one to open it.

## Where files live

rama works on your repositories in place and keeps no documents of its own. Its settings, recent
repositories and small caches (folder sizes in `sizes.json`, star count) are in the Otherwise
data folder, plus `worktrees.txt`, the worktree folders rama has listed (one per line, so diskmap
can mark them), all
`~/.otherwise/rama/` (or wherever you moved the data root in Otherwise Apps). Version 0.4 moves
them there once from the old folder (`~/Library/Application Support/rama`, `%APPDATA%\rama`,
`~/.config/rama`), never overwriting anything, and leaves a `.migrated-to-otherwise` note.
*Show rama's settings folder* is in the app menu and the globe menu; worktrees have *Show in
Finder* on the map. `OTHERWISE_DATA_DIR` points the data root elsewhere.

**Help → Crea tu tienda en Vcity** (also in About) is a short invitation to
[Vcity](https://vcity.io), online stores for businesses; the link only carries
`utm_source=rama`, nothing about you.

## Build and run

```sh
cargo run -p rama-app -- path/to/repo        # or no argument for the start screen
cargo run --release -p rama-repo --example log -- path/to/repo   # load history and time it
cargo run --release -p rama-repo --example worktrees -- path/to/repo   # time the map and cleanup scans (read-only)
script/bundle-macos.sh                       # macOS: target/release/rama.app with the icon
```

## Layout

```
crates/graph  rama-graph: topological order and lane layout — no git, no UI
crates/repo   rama-repo: history, refs, changed files and diffs through gitoxide; working-copy
              status and every write through the git CLI; login-shell PATH — no UI
crates/i18n   rama-i18n: embedded catalogs (es, en, pt, fr) and i18n guard tests
crates/app    `rama` desktop app
```

## License

MIT or Apache-2.0, at your option.

Developed by [Otherwise Technologies LLC](https://otherwisetech.com), Wyoming, USA.
