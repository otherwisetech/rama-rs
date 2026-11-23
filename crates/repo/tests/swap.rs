//! Change branch and worktree swaps against throwaway repositories: changes travel with their
//! branch in tagged stashes, failures roll back, conflicts keep the stash.
use repo::shell_env;
use repo::swap::{self, Blocker, Ledger, SwapError, Target};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("rama-swap-test-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join(".test-gitconfig");
        std::fs::write(&config, "[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n[tag]\n\tgpgsign = false\n").unwrap();
        // SAFETY: runs once, before any test in this binary spawns a process
        unsafe {
            std::env::set_var("GIT_CONFIG_GLOBAL", &config);
            std::env::set_var("GIT_CONFIG_NOSYSTEM", "1");
            std::env::set_var("LC_ALL", "C");
            for k in ["GIT_AUTHOR_NAME", "GIT_COMMITTER_NAME"] {
                std::env::set_var(k, "Ana Test");
            }
            for k in ["GIT_AUTHOR_EMAIL", "GIT_COMMITTER_EMAIL"] {
                std::env::set_var(k, "ana@example.com");
            }
        }
        shell_env::use_inherited();
    });
}

fn git(dir: &Path, args: &[&str]) -> String {
    setup();
    let out = Command::new("git").args(args).current_dir(dir).output().expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(dir: &Path, file: &str, body: &str) {
    let p = dir.join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
}

fn read(dir: &Path, file: &str) -> String {
    std::fs::read_to_string(dir.join(file)).unwrap_or_default()
}

fn commit(dir: &Path, file: &str, body: &str, msg: &str) {
    write(dir, file, body);
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-q", "-m", msg]);
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap()
}

/// `main/` on main (a.txt) with branch `dev` (b.txt) and a worktree `wt/` on `feat` (c.txt).
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    setup();
    let d = tempfile::tempdir().unwrap();
    let m = d.path().join("main");
    std::fs::create_dir(&m).unwrap();
    git(&m, &["init", "-q", "-b", "main"]);
    commit(&m, "a.txt", "one\n", "base");
    git(&m, &["branch", "dev"]);
    git(&m, &["switch", "-q", "dev"]);
    commit(&m, "b.txt", "dev\n", "dev work");
    git(&m, &["switch", "-q", "main"]);
    let w = d.path().join("wt");
    git(&m, &["worktree", "add", "-q", "-b", "feat", w.to_str().unwrap()]);
    commit(&w, "c.txt", "feat\n", "feat work");
    (d, canon(&m), canon(&w))
}

fn branch(dir: &Path) -> String {
    git(dir, &["symbolic-ref", "--short", "HEAD"]).trim().to_string()
}

fn stashes(dir: &Path) -> Vec<String> {
    git(dir, &["stash", "list", "--format=%gs"]).lines().map(str::to_string).collect()
}

fn noop() -> impl FnMut(&str) {
    |_: &str| {}
}

#[test]
fn change_branch_carries_changes_and_keeps_staging() {
    let (_d, m, _w) = fixture();
    write(&m, "a.txt", "one\nmine\n");
    git(&m, &["add", "a.txt"]);
    write(&m, "notes.txt", "untracked\n");
    git(&m, &["stash", "push", "-q", "-m", "someone else's stash"]);
    write(&m, "a.txt", "one\nmine\n");
    git(&m, &["add", "a.txt"]);
    write(&m, "notes.txt", "untracked\n");
    let p = swap::preview_change(&m, &Target::Local("dev".into())).unwrap();
    assert_eq!((p.from.as_deref(), p.to.as_str(), p.changed), (Some("main"), "dev", 2));
    let moved = swap::change_branch(&m, &Target::Local("dev".into()), "t1", &mut noop()).unwrap();
    assert_eq!(moved.carried.0, 2);
    assert_eq!(branch(&m), "dev");
    assert_eq!(read(&m, "a.txt"), "one\nmine\n");
    assert_eq!(read(&m, "b.txt"), "dev\n");
    assert_eq!(read(&m, "notes.txt"), "untracked\n");
    assert_eq!(git(&m, &["diff", "--cached", "--name-only"]).trim(), "a.txt", "staged stays staged");
    assert_eq!(stashes(&m), vec!["On main: someone else's stash"], "our stash is dropped, the other one is untouched");
}

#[test]
fn change_branch_to_a_remote_branch_creates_a_tracking_branch() {
    let (d, m, _w) = fixture();
    let remote = d.path().join("remote.git");
    git(d.path(), &["init", "-q", "--bare", remote.to_str().unwrap()]);
    git(&m, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&m, &["push", "-q", "origin", "dev:release/1"]);
    git(&m, &["fetch", "-q", "origin"]);
    write(&m, "a.txt", "one\nlocal\n");
    swap::change_branch(&m, &Target::Remote("origin/release/1".into()), "t2", &mut noop()).unwrap();
    assert_eq!(branch(&m), "release/1");
    assert_eq!(git(&m, &["rev-parse", "--abbrev-ref", "release/1@{upstream}"]).trim(), "origin/release/1");
    assert_eq!(read(&m, "a.txt"), "one\nlocal\n");
    assert!(stashes(&m).is_empty());
}

#[test]
fn branch_checked_out_elsewhere_is_refused_and_names_the_worktree() {
    let (_d, m, w) = fixture();
    write(&m, "a.txt", "dirty\n");
    let err = swap::change_branch(&m, &Target::Local("feat".into()), "t3", &mut noop()).unwrap_err();
    match err {
        SwapError::Blocked(Blocker::Elsewhere { branch, worktree }) => {
            assert_eq!(branch, "feat");
            assert_eq!(canon(&worktree), w);
        }
        e => panic!("unexpected {e:?}"),
    }
    assert_eq!(branch(&m), "main");
    assert_eq!(read(&m, "a.txt"), "dirty\n", "nothing was touched");
    assert!(stashes(&m).is_empty());
}

#[test]
fn swap_and_swap_back_with_changes_on_both_sides() {
    let (d, m, w) = fixture();
    let ledger_file = d.path().join("state").join("swaps.json");
    write(&m, "y.txt", "y\n");
    git(&m, &["stash", "push", "-q", "--include-untracked", "-m", "rama-swap:decoy:main"]);
    // a stash from another tool whose message looks like ours, and a plain one
    write(&m, "x.txt", "x\n");
    git(&m, &["stash", "push", "-q", "--include-untracked", "-m", "rama-swap:other:feat"]);
    write(&m, "a.txt", "one\nmain wip\n");
    write(&m, "main-new.txt", "new on main\n");
    write(&w, "c.txt", "feat\nwt wip\n");
    git(&w, &["add", "c.txt"]);
    write(&w, "wt-new.txt", "new in wt\n");
    write(&w, "d.txt", "d\n");
    let before = stashes(&m);

    let mut ledger = Ledger::load(&ledger_file);
    let p = swap::preview_swap(&m, &w, &ledger).unwrap();
    assert_eq!((p.a_branch.as_str(), p.b_branch.as_str(), p.a_changed, p.b_changed, p.back), ("main", "feat", 2, 3, false));
    let mut log = Vec::new();
    let (p, moved) = swap::swap(&m, &w, "s1", &ledger, &mut |l| log.push(l.to_string())).unwrap();
    assert_eq!(moved.carried, (2, 3));
    assert!(log.iter().any(|l| l.contains("switch --detach")), "{log:?}");
    ledger.after_swap("s1", &m, &w, &p.a_branch, &p.b_branch, 1);
    ledger.save(&ledger_file).unwrap();

    assert_eq!(branch(&m), "feat");
    assert_eq!(branch(&w), "main");
    // feat's work and its uncommitted changes are now in main/, main's are in wt/
    assert_eq!(read(&m, "c.txt"), "feat\nwt wip\n");
    assert_eq!(read(&m, "wt-new.txt"), "new in wt\n");
    assert_eq!(git(&m, &["diff", "--cached", "--name-only"]).trim(), "c.txt");
    assert_eq!(read(&w, "a.txt"), "one\nmain wip\n");
    assert_eq!(read(&w, "main-new.txt"), "new on main\n");
    assert!(!m.join("main-new.txt").exists() && !w.join("wt-new.txt").exists());
    assert_eq!(stashes(&m), before, "only our own stashes come and go");

    let mut ledger = Ledger::load(&ledger_file);
    let rec = ledger.find(&w).cloned().expect("swap recorded");
    assert_eq!(canon(rec.partner(&w).unwrap()), m);
    assert!(rec.holds(&swap::branch_at));
    let back = swap::preview_swap(&m, &w, &ledger).unwrap();
    assert!(back.back);
    let (p, _) = swap::swap(&m, &w, "s2", &ledger, &mut noop()).unwrap();
    ledger.after_swap("s2", &m, &w, &p.a_branch, &p.b_branch, 2);
    assert!(ledger.swaps.is_empty(), "swapping back clears the record");
    assert_eq!(branch(&m), "main");
    assert_eq!(branch(&w), "feat");
    assert_eq!(read(&m, "a.txt"), "one\nmain wip\n");
    assert_eq!(read(&w, "c.txt"), "feat\nwt wip\n");
    assert_eq!(read(&w, "d.txt"), "d\n");
    assert_eq!(stashes(&m), before);
}

#[test]
fn a_conflict_keeps_the_stash_and_the_changes() {
    let (_d, m, _w) = fixture();
    git(&m, &["switch", "-q", "dev"]);
    commit(&m, "a.txt", "dev version\n", "dev edits a");
    git(&m, &["switch", "-q", "main"]);
    write(&m, "a.txt", "my version\n");
    let err = swap::change_branch(&m, &Target::Local("dev".into()), "c1", &mut noop()).unwrap_err();
    let SwapError::Conflict { kept, .. } = err else { panic!("expected a conflict, got {err:?}") };
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].tag, "rama-swap:c1:main");
    assert_eq!(branch(&m), "dev");
    assert_eq!(stashes(&m), vec!["On main: rama-swap:c1:main"], "the stash is never dropped after a conflict");
    assert!(read(&m, "a.txt").contains("<<<<<<<"));
    let kept_sha = git(&m, &["rev-parse", "stash@{0}"]);
    assert_eq!(kept_sha.trim(), kept[0].sha);
    assert!(git(&m, &["show", &format!("{}:a.txt", kept[0].sha)]).contains("my version"), "the user's version is in the kept stash");
}

#[test]
fn a_failing_step_rolls_everything_back() {
    let (_d, m, w) = fixture();
    write(&m, "a.txt", "one\nmain wip\n");
    write(&w, "c.txt", "feat\nwt wip\n");
    write(&w, "u.txt", "untracked\n");
    let wt_git = PathBuf::from(git(&w, &["rev-parse", "--absolute-git-dir"]).trim());
    // main switching to feat locks wt's index, so wt can't take main; switching main back
    // unlocks it, so the undo can finish
    let hook = format!(
        "#!/bin/sh\n[ \"$1\" = committed ] || {{ cat >/dev/null; exit 0; }}\nin=$(cat)\nhere=$(pwd -P)\n[ \"$here\" = \"{m}\" ] || exit 0\ncase \"$in\" in *\"ref:refs/heads/feat HEAD\"*) touch \"{l}\";; esac\ncase \"$in\" in *\"ref:refs/heads/main HEAD\"*) rm -f \"{l}\";; esac\nexit 0\n",
        m = m.display(),
        l = wt_git.join("index.lock").display()
    );
    let hooks = m.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("reference-transaction"), hook).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(hooks.join("reference-transaction"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let err = swap::swap(&m, &w, "r1", &Ledger::default(), &mut noop()).unwrap_err();
    assert!(matches!(err, SwapError::RolledBack { .. }), "{err:?}");
    assert_eq!(branch(&m), "main");
    assert_eq!(branch(&w), "feat");
    assert_eq!(read(&m, "a.txt"), "one\nmain wip\n");
    assert_eq!(read(&w, "c.txt"), "feat\nwt wip\n");
    assert_eq!(read(&w, "u.txt"), "untracked\n");
    assert!(stashes(&m).is_empty());
}

#[test]
fn a_failing_stash_puts_the_other_side_back() {
    let (_d, m, w) = fixture();
    write(&m, "a.txt", "one\nmain wip\n");
    write(&w, "c.txt", "feat\nwt wip\n");
    let lock = PathBuf::from(git(&w, &["rev-parse", "--absolute-git-dir"]).trim()).join("index.lock");
    std::fs::write(&lock, "").unwrap();
    let err = swap::swap(&m, &w, "r2", &Ledger::default(), &mut noop()).unwrap_err();
    std::fs::remove_file(&lock).unwrap();
    assert!(matches!(err, SwapError::RolledBack { .. }), "{err:?}");
    assert_eq!((branch(&m), branch(&w)), ("main".to_string(), "feat".to_string()));
    assert_eq!(read(&m, "a.txt"), "one\nmain wip\n");
    assert_eq!(read(&w, "c.txt"), "feat\nwt wip\n");
    assert!(stashes(&m).is_empty());
}

#[test]
fn a_failing_post_checkout_hook_is_a_warning_not_a_failure() {
    let (_d, m, w) = fixture();
    let hooks = m.join(".git").join("hooks");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::write(hooks.join("post-checkout"), "#!/bin/sh\nexit 3\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(hooks.join("post-checkout"), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    let (_, moved) = swap::swap(&m, &w, "h1", &Ledger::default(), &mut noop()).unwrap();
    assert!(!moved.hook_warnings.is_empty());
    assert_eq!((branch(&m), branch(&w)), ("feat".to_string(), "main".to_string()));
}

#[test]
fn refusals() {
    let (_d, m, w) = fixture();
    // a merge in progress in the worktree
    git(&w, &["switch", "-q", "--detach"]);
    git(&w, &["switch", "-q", "feat"]);
    git(&m, &["switch", "-q", "dev"]);
    commit(&m, "c.txt", "dev c\n", "dev c");
    git(&m, &["switch", "-q", "main"]);
    let out = Command::new("git").args(["merge", "dev"]).current_dir(&w).output().unwrap();
    assert!(!out.status.success());
    match swap::preview_swap(&m, &w, &Ledger::default()) {
        Err(Blocker::InProgress { what: "merge", .. }) => {}
        r => panic!("unexpected {r:?}"),
    }
    git(&w, &["merge", "--abort"]);
    git(&w, &["switch", "-q", "--detach"]);
    assert!(matches!(swap::preview_swap(&m, &w, &Ledger::default()), Err(Blocker::Detached(_))));
    git(&w, &["switch", "-q", "feat"]);
    // swapped with a third checkout
    let mut ledger = Ledger::default();
    ledger.after_swap("x", &m, Path::new("/elsewhere"), "main", "other", 0);
    assert!(matches!(swap::preview_swap(&m, &w, &ledger), Err(Blocker::AlreadySwapped { .. })));
    assert!(matches!(swap::preview_change(&m, &Target::Local("main".into())), Err(Blocker::Nothing)));
    // LFS attributes without git-lfs installed are refused only when git lfs is missing
    write(&m, ".gitattributes", "*.bin filter=lfs diff=lfs merge=lfs -text\n");
    let lfs = Command::new("git").args(["lfs", "version"]).output().is_ok_and(|o| o.status.success());
    assert_eq!(matches!(swap::preflight(&m), Err(Blocker::LfsMissing(_))), !lfs);
}

#[test]
fn ledger_forgets_swaps_undone_by_hand() {
    let mut l = Ledger::default();
    l.after_swap("a", Path::new("/r"), Path::new("/r-wt"), "main", "feat", 1);
    assert_eq!(l.swaps.len(), 1);
    let now = |p: &Path| Some(if p == Path::new("/r") { "feat".to_string() } else { "main".to_string() });
    assert!(!l.retain_live(&now));
    let by_hand = |_: &Path| Some("main".to_string());
    assert!(l.retain_live(&by_hand));
    assert!(l.swaps.is_empty());
}

#[test]
fn ledger_survives_a_round_trip_through_its_file() {
    let d = tempfile::tempdir().unwrap();
    let f = d.path().join("rama").join("swaps.tsv");
    let mut l = Ledger::default();
    l.after_swap("id1", Path::new("/src/app"), Path::new("/src/app-feat x"), "main", "feat/x", 42);
    l.save(&f).unwrap();
    assert_eq!(Ledger::load(&f), l);
    assert_eq!(Ledger::load(&d.path().join("missing")), Ledger::default());
}
