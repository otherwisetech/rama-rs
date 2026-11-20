//! Worktrees, branch cleanup and disk usage against throwaway repositories.
use repo::{Cancel, NewWorktree, Reason, Repo, shell_env};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("rama-wt-test-home-{}", std::process::id()));
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

fn commit(dir: &Path, file: &str, body: &str, msg: &str) {
    write(dir, file, body);
    git(dir, &["add", "--", file]);
    git(dir, &["commit", "-q", "-m", msg]);
}

/// A temp dir holding `main/` (one commit on main); worktrees go next to it.
fn base() -> (tempfile::TempDir, PathBuf) {
    setup();
    let d = tempfile::tempdir().unwrap();
    let p = d.path().join("main");
    std::fs::create_dir(&p).unwrap();
    git(&p, &["init", "-q", "-b", "main"]);
    commit(&p, "a.txt", "one\n", "base");
    (d, p)
}

fn run(r: &Repo, args: Vec<String>) -> repo::Run {
    r.run_args(&args, &Cancel::default(), &mut |_, _| {}).unwrap()
}

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[test]
fn porcelain_parsing() {
    let raw = b"worktree /src/app\0HEAD 1111111111111111111111111111111111111111\0branch refs/heads/main\0\0\
worktree /src/app-feature\0HEAD 2222222222222222222222222222222222222222\0branch refs/heads/feature/x\0locked needs the usb disk\0\0\
worktree /src/app-review\0HEAD 3333333333333333333333333333333333333333\0detached\0\0\
worktree /tmp/gone\0HEAD 4444444444444444444444444444444444444444\0detached\0prunable gitdir file points to non-existent location\0\0";
    let w = repo::parse_worktree_list(raw);
    assert_eq!(w.len(), 4);
    assert!(w[0].main && !w[1].main);
    assert_eq!(w[0].branch.as_deref(), Some("main"));
    assert_eq!(w[1].branch.as_deref(), Some("feature/x"));
    assert_eq!(w[1].locked.as_deref(), Some("needs the usb disk"));
    assert!(w[2].detached && w[2].branch.is_none() && w[2].locked.is_none());
    assert_eq!(w[3].prunable.as_deref(), Some("gitdir file points to non-existent location"));
    assert_eq!(w[3].path, PathBuf::from("/tmp/gone"));

    // newline form, a bare main repository, and a lock without reason
    let raw = b"worktree /srv/r.git\nbare\n\nworktree /srv/r-wt\nHEAD 5555555555555555555555555555555555555555\nbranch refs/heads/dev\nlocked\n\n";
    let w = repo::parse_worktree_list(raw);
    assert!(w[0].bare && w[0].head.is_none() && w[0].main);
    assert_eq!(w[1].locked.as_deref(), Some(""));
}

#[test]
fn helpers() {
    assert_eq!(repo::parse_track("ahead 3, behind 14"), (3, 14, false));
    assert_eq!(repo::parse_track("behind 2"), (0, 2, false));
    assert_eq!(repo::parse_track("gone"), (0, 0, true));
    assert_eq!(repo::parse_track(""), (0, 0, false));
    assert!(repo::glob_match("release*", "release/1.2"));
    assert!(repo::glob_match("*-stable", "v2-stable"));
    assert!(repo::glob_match("a*b*c", "a-x-b-y-c"));
    assert!(!repo::glob_match("release*", "pre-release"));
    assert!(!repo::glob_match("main", "main2"));
    let prot: Vec<String> = repo::PROTECTED.iter().map(|s| s.to_string()).collect();
    assert!(repo::is_protected("develop", &prot) && repo::is_protected("release-2026", &prot) && !repo::is_protected("feature/x", &prot));
    assert_eq!(repo::default_worktree_path(Path::new("/src/app"), None, "feature/login"), PathBuf::from("/src/app-feature-login"));
    assert_eq!(repo::default_worktree_path(Path::new("/src/app"), Some(Path::new("/wt")), "fix"), PathBuf::from("/wt/app-fix"));
    let p = Path::new("/w/x");
    assert_eq!(repo::worktree_add_args(p, &NewWorktree::NewBranch { name: "f".into(), start: "main".into() }), ["worktree", "add", "-b", "f", "/w/x", "main"]);
    assert_eq!(repo::worktree_add_args(p, &NewWorktree::Commit("abc".into())), ["worktree", "add", "--detach", "/w/x", "abc"]);
    assert_eq!(Repo::worktree_remove_args(p, true), ["worktree", "remove", "--force", "/w/x"]);
    assert_eq!(Repo::worktree_lock_args(p, " usb "), ["worktree", "lock", "--reason", "usb", "/w/x"]);
}

#[test]
fn add_list_status_lock_remove_and_prune() {
    let (d, main) = base();
    let r = Repo::discover(&main).unwrap();
    let feat = d.path().join("main-feat");
    assert!(run(&r, repo::worktree_add_args(&feat, &NewWorktree::NewBranch { name: "feat".into(), start: "main".into() })).success);
    let review = d.path().join("review");
    assert!(run(&r, repo::worktree_add_args(&review, &NewWorktree::Commit("main".into()))).success);
    commit(&feat, "f.txt", "f\n", "feature work");

    let bases = r.base_branches(None);
    assert_eq!(bases[0].name, "main");
    assert_eq!(bases[0].rev, "refs/heads/main");
    let all = r.worktree_overview(&bases).unwrap();
    assert_eq!(all.len(), 3);
    assert!(all[0].wt.main);
    let f = all.iter().find(|e| e.wt.branch.as_deref() == Some("feat")).unwrap();
    assert_eq!(canon(&f.wt.path), canon(&feat));
    assert_eq!(f.info.vs_default(), Some((1, 0)));
    assert!(!f.info.merged);
    assert_eq!(f.info.subject.as_deref(), Some("feature work"));
    assert_eq!(f.info.changed(), 0);
    let rv = all.iter().find(|e| e.wt.detached).unwrap();
    assert!(rv.info.merged && rv.info.vs_default() == Some((0, 0)));
    assert_eq!(rv.info.subject.as_deref(), Some("base"));

    // dirty: tracked change + untracked file; plain remove is refused, force removes it
    write(&feat, "a.txt", "changed\n");
    write(&feat, "new.txt", "u\n");
    let f = r.worktree_overview(&bases).unwrap().into_iter().find(|e| e.wt.branch.as_deref() == Some("feat")).unwrap();
    assert_eq!(f.info.changed(), 2);
    assert!(!run(&r, Repo::worktree_remove_args(&feat, false)).success);
    assert!(feat.exists());

    // locked: listed as locked, and even --force refuses
    assert!(run(&r, Repo::worktree_lock_args(&feat, "on usb")).success);
    let w = r.worktrees().unwrap();
    assert_eq!(w.iter().find(|w| w.branch.as_deref() == Some("feat")).unwrap().locked.as_deref(), Some("on usb"));
    assert!(!run(&r, Repo::worktree_remove_args(&feat, true)).success);
    assert!(run(&r, Repo::worktree_unlock_args(&feat)).success);
    assert!(run(&r, Repo::worktree_remove_args(&feat, true)).success);
    assert!(!feat.exists());
    // the branch stays
    git(&main, &["rev-parse", "--verify", "feat"]);

    // a worktree whose folder vanished is prunable; prune previews then removes it
    std::fs::remove_dir_all(&review).unwrap();
    let w = r.worktrees().unwrap();
    assert!(w.iter().any(|w| w.prunable.is_some()));
    let preview = r.worktree_prune_preview().unwrap();
    assert_eq!(preview.len(), 1, "{preview:?}");
    assert!(r.worktrees().unwrap().iter().any(|w| w.prunable.is_some()), "dry run changes nothing");
    assert!(run(&r, vec!["worktree".into(), "prune".into(), "--verbose".into()]).success);
    assert_eq!(r.worktrees().unwrap().len(), 1);
}

/// `main/` cloned from a bare `origin.git` that has `main` as HEAD.
fn with_remote() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (d, seed) = base();
    let origin = d.path().join("origin.git");
    git(d.path(), &["clone", "-q", "--bare", seed.to_str().unwrap(), origin.to_str().unwrap()]);
    let main = d.path().join("clone");
    git(d.path(), &["clone", "-q", origin.to_str().unwrap(), main.to_str().unwrap()]);
    (d, main, origin)
}

#[test]
fn default_branch_from_origin_head_and_preference() {
    let (_d, main, _) = with_remote();
    git(&main, &["branch", "dev"]);
    git(&main, &["push", "-q", "origin", "dev"]);
    let r = Repo::discover(&main).unwrap();
    let b = r.base_branches(None);
    assert_eq!(b[0].name, "main");
    assert_eq!(b[0].rev, "refs/remotes/origin/main");
    assert_eq!(b[0].local.as_deref(), Some("refs/heads/main"));
    assert_eq!(b[0].short_rev(), "origin/main");
    assert!(b.iter().any(|x| x.name == "dev"));
    let b = r.base_branches(Some("dev"));
    assert_eq!(b[0].name, "dev");
    // a preference that doesn't exist falls back
    assert_eq!(r.base_branches(Some("nope"))[0].name, "main");
}

#[test]
fn cleanup_finds_merged_gone_and_squashed_branches() {
    let (d, main, _) = with_remote();
    let r = Repo::discover(&main).unwrap();
    // merged: fast-forwarded into main and pushed
    git(&main, &["switch", "-q", "-c", "merged"]);
    commit(&main, "m.txt", "m\n", "merged work");
    git(&main, &["switch", "-q", "main"]);
    git(&main, &["merge", "-q", "--ff-only", "merged"]);
    git(&main, &["push", "-q", "origin", "main"]);
    // squashed: two commits, squash-merged into main as one
    git(&main, &["switch", "-q", "-c", "squashed"]);
    commit(&main, "s.txt", "s1\n", "squash 1");
    commit(&main, "s.txt", "s1\ns2\n", "squash 2");
    git(&main, &["switch", "-q", "main"]);
    git(&main, &["merge", "-q", "--squash", "squashed"]);
    git(&main, &["commit", "-q", "-m", "Squashed feature (#12)"]);
    git(&main, &["push", "-q", "origin", "main"]);
    // gone: pushed, deleted on the remote, pruned; it has unmerged work
    git(&main, &["switch", "-q", "-c", "gone"]);
    commit(&main, "g.txt", "g\n", "gone work");
    git(&main, &["push", "-q", "-u", "origin", "gone"]);
    git(&main, &["push", "-q", "origin", "--delete", "gone"]);
    git(&main, &["fetch", "-q", "--prune"]);
    git(&main, &["switch", "-q", "main"]);
    // live work: never a candidate
    git(&main, &["switch", "-q", "-c", "wip"]);
    commit(&main, "w.txt", "w\n", "wip");
    git(&main, &["switch", "-q", "main"]);
    // protected and merged: excluded by pattern
    git(&main, &["branch", "release-1"]);
    git(&main, &["branch", "develop"]);
    // merged and checked out in another worktree: listed with it
    git(&main, &["branch", "in-wt"]);
    let wt = d.path().join("wt");
    git(&main, &["worktree", "add", "-q", wt.to_str().unwrap(), "in-wt"]);

    let default = r.base_branches(None).remove(0);
    let protected: Vec<String> = repo::PROTECTED.iter().map(|s| s.to_string()).collect();
    let progress = |_: usize, _: usize| {};
    let c = r.cleanup_candidates(&default, &protected, true, &Cancel::default(), &progress).unwrap();
    let names: Vec<&str> = {
        let mut v: Vec<&str> = c.iter().map(|c| c.name.as_str()).collect();
        v.sort();
        v
    };
    assert_eq!(names, ["gone", "in-wt", "merged", "squashed"]);
    let get = |n: &str| c.iter().find(|c| c.name == n).unwrap();
    assert_eq!(get("merged").reason, Reason::Merged("origin/main".into()));
    assert!(!get("merged").needs_force && get("merged").suggested());
    assert_eq!(get("squashed").reason, Reason::Squashed { confident: true });
    assert!(get("squashed").needs_force && get("squashed").suggested());
    let g = get("gone");
    assert!(g.gone && g.reason == Reason::Gone && g.needs_force);
    assert_eq!(canon(get("in-wt").worktree.as_ref().unwrap()), canon(&wt));
    assert!(get("merged").worktree.is_none());

    // the quick pass skips the squash check: the squashed branch isn't listed, gone still is
    let quick = r.cleanup_candidates(&default, &protected, false, &Cancel::default(), &progress).unwrap();
    assert!(quick.iter().any(|c| c.name == "gone") && !quick.iter().any(|c| c.name == "squashed"));

    // the current branch is never a candidate, even when merged
    git(&main, &["switch", "-q", "merged"]);
    let c = r.cleanup_candidates(&default, &protected, true, &Cancel::default(), &progress).unwrap();
    assert!(!c.iter().any(|c| c.name == "merged"));
    assert!(!c.iter().any(|c| c.name == "main" || c.name == "develop" || c.name == "release-1"));
}

#[test]
fn squash_heuristic_without_exact_patch() {
    let (_d, main) = base();
    let r = Repo::discover(&main).unwrap();
    git(&main, &["switch", "-q", "-c", "topic"]);
    commit(&main, "t.txt", "a\n", "t1");
    commit(&main, "t.txt", "a\nb\n", "t2");
    git(&main, &["switch", "-q", "main"]);
    // same final content landed on main mixed with other edits: no exact patch match
    write(&main, "t.txt", "a\nb\n");
    write(&main, "other.txt", "x\n");
    git(&main, &["add", "-A"]);
    git(&main, &["commit", "-q", "-m", "topic + other"]);
    let default = r.base_branches(None).remove(0);
    let c = r.cleanup_candidates(&default, &[], true, &Cancel::default(), &|_, _| {}).unwrap();
    let t = c.iter().find(|c| c.name == "topic").unwrap();
    assert_eq!(t.reason, Reason::Squashed { confident: false });
    assert!(!t.suggested());
}

#[test]
fn delete_then_restore_from_undo_log() {
    let (_d, main) = base();
    let r = Repo::discover(&main).unwrap();
    git(&main, &["branch", "old"]);
    let sha = git(&main, &["rev-parse", "old"]).trim().to_string();
    let oid = repo::Oid::from_hex(sha.as_bytes()).unwrap();
    r.delete_branch("old").unwrap();
    assert!(Command::new("git").args(["rev-parse", "--verify", "-q", "old"]).current_dir(&main).output().unwrap().stdout.is_empty());
    r.restore_branch("old", oid).unwrap();
    assert_eq!(git(&main, &["rev-parse", "old"]).trim(), sha);
    // refuses to overwrite a branch that exists again
    assert!(r.restore_branch("old", oid).is_err());
}

#[test]
fn gc_and_git_dir_size() {
    let (_d, main) = base();
    let r = Repo::discover(&main).unwrap();
    for i in 0..20 {
        commit(&main, "a.txt", &format!("{i}\n").repeat(500), &format!("c{i}"));
    }
    let before = r.git_dir_usage();
    assert!(before.total > 0 && before.files > 0);
    assert!(run(&r, vec!["gc".into(), "--quiet".into()]).success);
    let after = r.git_dir_usage();
    assert!(after.total <= before.total, "{} > {}", after.total, before.total);
    assert_eq!(canon(&r.common_dir()), canon(&main.join(".git")));
}

#[test]
fn disk_usage_breakdown_skips_nested_worktrees() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    write(p, "src/main.rs", &"x".repeat(10_000));
    write(p, "node_modules/a/index.js", &"y".repeat(50_000));
    write(p, "web/node_modules/b/index.js", &"y".repeat(20_000));
    write(p, ".worktrees/other/big.bin", &"z".repeat(200_000));
    let all = repo::disk_usage(p, &[], &Cancel::default());
    let skipped = repo::disk_usage(p, &[p.join(".worktrees/other")], &Cancel::default());
    assert!(all.total >= skipped.total + 200_000, "{} vs {}", all.total, skipped.total);
    let nm = skipped.parts.iter().find(|(n, _)| n == "node_modules").unwrap().1;
    assert!(nm >= 70_000 && nm < skipped.total);
    assert_eq!(skipped.files, 3);
    #[cfg(unix)]
    {
        // a hard link (pnpm's store) counts once
        std::fs::hard_link(p.join("node_modules/a/index.js"), p.join("src/linked.js")).unwrap();
        let again = repo::disk_usage(p, &[p.join(".worktrees/other")], &Cancel::default());
        assert!(again.total < skipped.total + 8192, "the linked copy was counted again");
    }
    let c = Cancel::default();
    c.cancel();
    assert!(repo::disk_usage(p, &[], &c).partial);
}

#[cfg(unix)]
#[test]
fn which_finds_executables_on_a_path() {
    use std::os::unix::fs::PermissionsExt;
    let d = tempfile::tempdir().unwrap();
    let (a, b) = (d.path().join("a"), d.path().join("b"));
    std::fs::create_dir_all(&a).unwrap();
    std::fs::create_dir_all(&b).unwrap();
    std::fs::write(a.join("claude"), "").unwrap(); // not executable: skipped
    std::fs::write(b.join("claude"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(b.join("claude"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", a.display(), b.display());
    assert_eq!(shell_env::find_in(&path, "claude"), Some(b.join("claude")));
    assert_eq!(shell_env::find_in(&path, "codex"), None);
}
