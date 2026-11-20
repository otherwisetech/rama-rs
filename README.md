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
- *Git output* panel with the running command, live output from git and its hooks, and Cancel.
- Refreshes on its own when files change on disk or the window regains focus.
- Keyboard: ↑/↓ or j/k to move, ⌘F to search, Esc to close a diff or clear the search, ⌘C copies the commit SHA, ⌘O opens a repo,
  ⌘R reloads.
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
or notarized**. rama makes no network requests of its own; fetch, pull and push run `git`.

## Build and run

```sh
cargo run -p rama-app -- path/to/repo        # or no argument for the start screen
cargo run --release -p rama-repo --example log -- path/to/repo   # load history and time it
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
