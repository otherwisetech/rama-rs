//! Disk reclaim: sizes of build folders, the cleanup plan's safety rules and order, and
//! running it — all against throwaway folders and repositories.
use repo::reclaim::{self, PlanInput, ReclaimKind, SharedTarget, Skip, Suggestion};
use repo::{Cancel, DiskUsage, FileState, Repo, WorkFile, WorkStatus, Worktree, WorktreeEntry, WorktreeInfo, shell_env};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Once;

const DAY: i64 = 86_400;
const NOW: i64 = 1_800_000_000;

fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("rama-reclaim-test-home-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = dir.join(".test-gitconfig");
        std::fs::write(&config, "[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n").unwrap();
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

fn tmp() -> (tempfile::TempDir, PathBuf) {
    setup();
    let d = tempfile::tempdir().unwrap();
    let p = std::fs::canonicalize(d.path()).unwrap();
    (d, p)
}

/// A cargo-like project: sources, `target/` with incremental caches, `node_modules/`.
fn rust_project(p: &Path) {
    write(p, "Cargo.toml", "[package]\nname = \"x\"\n");
    write(p, "src/main.rs", &"x".repeat(5_000));
    write(p, "target/CACHEDIR.TAG", "Signature: 8a477f597d28d172789f06886806bc55\n");
    write(p, "target/debug/deps/libx.rlib", &"d".repeat(60_000));
    write(p, "target/debug/incremental/x-1/s.bin", &"i".repeat(40_000));
    write(p, "target/aarch64-apple-darwin/release/incremental/x-2/s.bin", &"i".repeat(30_000));
    write(p, "node_modules/a/index.js", &"n".repeat(20_000));
}

#[test]
fn sizes_find_build_folders_and_incremental_caches() {
    let (_d, p) = tmp();
    rust_project(&p);
    // not build folders: `.next` without package.json, `target` of a non-cargo project
    write(&p, ".next/cache.bin", &"c".repeat(10_000));
    write(&p, "docs/target/page.md", "hello");
    let u = repo::disk_usage(&p, &[], &Cancel::default());
    let paths: Vec<&Path> = u.artifacts.iter().map(|(a, _)| a.as_path()).collect();
    assert_eq!(paths, [p.join("node_modules"), p.join("target")]);
    let target = u.artifacts.iter().find(|(a, _)| a.ends_with("target")).unwrap().1;
    assert!(target >= 130_000, "{target}");
    let inc: Vec<&Path> = u.incremental.iter().map(|(a, _)| a.as_path()).collect();
    assert_eq!(inc, [p.join("target/aarch64-apple-darwin/release/incremental"), p.join("target/debug/incremental")]);
    assert!(u.incremental.iter().all(|(_, b)| *b >= 30_000));
    assert!(u.touched > 0);
    // with package.json, .next counts
    write(&p, "package.json", "{}");
    assert!(repo::disk_usage(&p, &[], &Cancel::default()).artifacts.iter().any(|(a, _)| a.ends_with(".next")));

    // an external cargo target folder is one artifact
    let ext = p.join("ext-target");
    write(&ext, "debug/incremental/y/s.bin", &"i".repeat(10_000));
    let t = repo::target_usage(&ext, &Cancel::default());
    assert_eq!(t.artifacts.len(), 1);
    assert_eq!(t.artifacts[0], (ext.clone(), t.total));
    assert_eq!(t.incremental.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>(), [ext.join("debug/incremental")]);
    assert!(t.touched > 0, "a target folder's age is its last build");
}

#[test]
fn cargo_target_dir_from_env_and_config() {
    let (_d, p) = tmp();
    let wt = p.join("code/app");
    write(&wt, "Cargo.toml", "[package]\n");
    let shared = p.join("cache/app-target");
    std::fs::create_dir_all(&shared).unwrap();
    assert_eq!(reclaim::cargo_target_dir(&wt, None, None), None);
    assert_eq!(reclaim::cargo_target_dir(&wt, Some(shared.to_str().unwrap()), None), Some(shared.clone()));
    assert_eq!(reclaim::cargo_target_dir(&wt, Some("relative/x"), None), None);
    // a config above the worktree, relative to the folder holding `.cargo`
    write(&p, "code/.cargo/config.toml", "[build]\ntarget-dir = \"../cache/app-target\" # shared\n");
    assert_eq!(reclaim::cargo_target_dir(&wt, None, None), Some(shared.clone()));
    // the nearest config wins; a target inside the worktree is left to the walk
    write(&wt, ".cargo/config.toml", "build.target-dir = 'out'\n");
    std::fs::create_dir_all(wt.join("out")).unwrap();
    assert_eq!(reclaim::cargo_target_dir(&wt, None, None), None);
    // no Cargo.toml: not a cargo project
    assert_eq!(reclaim::cargo_target_dir(&p.join("code"), Some(shared.to_str().unwrap()), None), None);
    assert_eq!(reclaim::config_target_dir("[profile.dev]\ntarget-dir = \"no\"\n"), None);
    assert_eq!(reclaim::config_target_dir("[build]\njobs = 4\n\"target-dir\" = \"/t\"\n").as_deref(), Some("/t"));
}

fn entry(path: &Path, branch: Option<&str>, main: bool) -> WorktreeEntry {
    WorktreeEntry {
        wt: Worktree { path: path.to_path_buf(), head: None, branch: branch.map(str::to_string), bare: false, detached: branch.is_none(), locked: None, prunable: None, main },
        info: WorktreeInfo { status: Some(WorkStatus::default()), merged: true, vs: vec![("main".into(), 0, 3)], time: Some(NOW - 30 * DAY), ..Default::default() },
    }
}

fn usage(path: &Path, touched: i64) -> DiskUsage {
    DiskUsage {
        total: 1_000,
        files: 3,
        touched,
        artifacts: vec![(path.join("node_modules"), 200), (path.join("target"), 500)],
        incremental: vec![(path.join("target/debug/incremental"), 100)],
        ..Default::default()
    }
}

fn plan_of(entries: &[WorktreeEntry], sizes: &HashMap<PathBuf, DiskUsage>, shared: &[SharedTarget], current: Option<&Path>) -> Vec<Suggestion> {
    let protected = vec!["main".to_string(), "release*".to_string()];
    reclaim::plan(&PlanInput { entries, usage: sizes, shared, current, protected: &protected, now: NOW, stale_days: 14 })
}

#[test]
fn plan_safety_rules() {
    let root = PathBuf::from(if cfg!(windows) { r"C:\w" } else { "/w" });
    let p = |s: &str| root.join(s);
    let mut main = entry(&p("app"), Some("main"), true);
    main.info.time = Some(NOW - DAY);
    let merged = entry(&p("app-done"), Some("done"), false);
    let mut dirty = entry(&p("app-dirty"), Some("dirty"), false);
    dirty.info.status = Some(WorkStatus { unstaged: vec![WorkFile { path: "new.txt".into(), state: FileState::Untracked }], ..Default::default() });
    let mut unpushed = entry(&p("app-unpushed"), Some("unpushed"), false);
    unpushed.info.upstream = Some("origin/unpushed".into());
    unpushed.info.ahead = 2;
    let mut ahead = entry(&p("app-ahead"), Some("ahead"), false);
    (ahead.info.merged, ahead.info.vs) = (false, vec![("main".into(), 1, 0)]);
    let mut locked = entry(&p("app-locked"), Some("locked"), false);
    locked.wt.locked = Some(String::new());
    let current = entry(&p("app-current"), Some("current"), false);
    let release = entry(&p("app-release"), Some("release-1"), false);
    let mut unknown = entry(&p("app-unknown"), Some("unknown"), false);
    unknown.info.status = None;
    let all = vec![main.clone(), merged, dirty, unpushed, ahead, locked, current, release, unknown];
    let mut sizes: HashMap<PathBuf, DiskUsage> = all.iter().map(|e| (e.wt.path.clone(), usage(&e.wt.path, NOW - 20 * DAY))).collect();
    sizes.get_mut(&main.wt.path).unwrap().touched = NOW - DAY;
    let plan = plan_of(&all, &sizes, &[], Some(&p("app-current")));

    let of = |kind: ReclaimKind| plan.iter().filter(|s| s.kind == kind).map(|s| s.owner.file_name().unwrap().to_string_lossy().into_owned()).collect::<Vec<_>>();
    // only clean, merged, pushed, unlocked, unprotected, not-open worktrees are removed
    assert_eq!(of(ReclaimKind::MergedWorktree), ["app-done"]);
    let done = plan.iter().find(|s| s.kind == ReclaimKind::MergedWorktree).unwrap();
    assert_eq!(done.paths, [(p("app-done"), 1_000)]);
    assert_eq!(done.branch.as_deref(), Some("done"));
    // dirty / unreadable worktrees get nothing at all
    for s in &plan {
        assert!(!s.owner.ends_with("app-dirty") && !s.owner.ends_with("app-unknown"), "{s:?}");
    }
    // the main worktree was used yesterday: only its incremental caches
    assert_eq!(of(ReclaimKind::Incremental), ["app"]);
    // the stale ones (not removed) lose target/ and node_modules/, incremental included in target
    let stale = ["app-ahead", "app-current", "app-locked", "app-release", "app-unpushed"];
    assert_eq!(of(ReclaimKind::StaleTarget), stale);
    assert_eq!(of(ReclaimKind::StaleDeps), stale);
    // every path stays inside its worktree
    for s in plan.iter().filter(|s| s.kind != ReclaimKind::MergedWorktree) {
        assert!(s.paths.iter().all(|(x, _)| x.starts_with(&s.owner) && *x != s.owner), "{s:?}");
    }
    assert_eq!(reclaim::reclaimable(&plan), plan.iter().map(|s| s.bytes).sum::<u64>());

    // a protected branch's worktree is never removed and its branch never offered
    let release = plan.iter().find(|s| s.owner.ends_with("app-release")).unwrap();
    assert!(release.branch.is_none());
}

#[test]
fn plan_drops_paths_outside_the_worktree_and_is_deterministic() {
    let root = PathBuf::from(if cfg!(windows) { r"C:\w" } else { "/w" });
    let a = entry(&root.join("a"), Some("a-branch"), false);
    let mut b = entry(&root.join("b"), Some("b-branch"), false);
    (b.info.merged, b.info.vs) = (false, vec![("main".into(), 4, 0)]);
    let mut sizes = HashMap::new();
    sizes.insert(a.wt.path.clone(), usage(&a.wt.path, NOW - 20 * DAY));
    let mut ub = usage(&b.wt.path, NOW - 20 * DAY);
    ub.artifacts.push((root.join("elsewhere/target"), 9_999));
    ub.artifacts.push((b.wt.path.join("../a/node_modules"), 9_999));
    sizes.insert(b.wt.path.clone(), ub);
    let shared = vec![SharedTarget { path: root.join("cache/t"), usage: DiskUsage { total: 5_000, touched: NOW - 30 * DAY, incremental: vec![(root.join("cache/t/debug/incremental"), 700), (root.join("outside/incremental"), 1)], ..Default::default() }, users: vec![a.wt.path.clone(), b.wt.path.clone()] }];
    let one = plan_of(&[a.clone(), b.clone()], &sizes, &shared, None);
    let two = plan_of(&[b.clone(), a.clone()], &sizes, &shared, None);
    assert_eq!(one, two);
    let kinds: Vec<(ReclaimKind, String)> = one.iter().map(|s| (s.kind, s.owner.file_name().unwrap().to_string_lossy().into_owned())).collect();
    assert_eq!(kinds, [(ReclaimKind::MergedWorktree, "a".into()), (ReclaimKind::StaleTarget, "b".into()), (ReclaimKind::StaleTarget, "t".into()), (ReclaimKind::StaleDeps, "b".into())]);
    for s in &one {
        for (x, _) in &s.paths {
            assert!(reclaim::contained(x, &s.owner, true), "{x:?} outside {:?}", s.owner);
        }
    }
    // both users idle: the whole shared folder; one recently used: only its incremental cache
    let t = one.iter().find(|s| s.shared).unwrap();
    assert_eq!(t.paths, [(root.join("cache/t"), 5_000)]);
    sizes.get_mut(&b.wt.path).unwrap().touched = NOW - DAY;
    let fresh = plan_of(&[a.clone(), b.clone()], &sizes, &shared, None);
    let t = fresh.iter().find(|s| s.shared).unwrap();
    assert_eq!((t.kind, t.paths.clone()), (ReclaimKind::Incremental, vec![(root.join("cache/t/debug/incremental"), 700)]));
    // another repository built there yesterday: only the incremental cache again
    sizes.get_mut(&b.wt.path).unwrap().touched = NOW - 20 * DAY;
    let mut busy = shared.clone();
    busy[0].usage.touched = NOW - DAY;
    assert_eq!(plan_of(&[a.clone(), b.clone()], &sizes, &busy, None).iter().find(|s| s.shared).unwrap().kind, ReclaimKind::Incremental);
    // a user with changes: the shared folder isn't offered
    let mut dirty_b = b.clone();
    dirty_b.info.status = None;
    assert!(plan_of(&[a, dirty_b], &sizes, &shared, None).iter().all(|s| !s.shared));

    if cfg!(windows) {
        return;
    }
    assert!(reclaim::contained(Path::new("/w/a/target"), Path::new("/w/a"), false));
    assert!(!reclaim::contained(Path::new("/w/a"), Path::new("/w/a"), false));
    assert!(reclaim::contained(Path::new("/w/a"), Path::new("/w/a"), true));
    assert!(!reclaim::contained(Path::new("/w/a/../b/target"), Path::new("/w/a"), false));
    assert!(!reclaim::contained(Path::new("/w/ab/target"), Path::new("/w/a"), false));
    assert!(!reclaim::contained(Path::new("w/a/target"), Path::new("w/a"), false));
}

#[test]
fn delete_artifact_refuses_anything_but_build_folders_inside() {
    let (_d, p) = tmp();
    let wt = p.join("wt");
    rust_project(&wt);
    write(&p, "outside/target/CACHEDIR.TAG", "x");
    write(&wt, "src/keep.rs", "keep");
    assert_eq!(reclaim::delete_artifact(&wt.join("src"), &wt, false), Err(Skip::NotArtifact));
    assert_eq!(reclaim::delete_artifact(&wt, &wt, false), Err(Skip::NotArtifact));
    assert_eq!(reclaim::delete_artifact(&p.join("outside/target"), &wt, false), Err(Skip::NotArtifact));
    assert_eq!(reclaim::delete_artifact(&wt.join("Cargo.toml"), &wt, false), Err(Skip::NotArtifact));
    #[cfg(unix)]
    {
        // a link named like a build folder pointing elsewhere is never followed
        std::os::unix::fs::symlink(p.join("outside/target"), wt.join("linked")).unwrap();
        std::fs::create_dir_all(wt.join("web")).unwrap();
        std::os::unix::fs::symlink(p.join("outside"), wt.join("web/node_modules")).unwrap();
        assert_eq!(reclaim::delete_artifact(&wt.join("web/node_modules"), &wt, false), Err(Skip::NotArtifact));
        assert!(p.join("outside/target/CACHEDIR.TAG").exists());
    }
    reclaim::delete_artifact(&wt.join("target/debug/incremental"), &wt, false).unwrap();
    assert!(!wt.join("target/debug/incremental").exists() && wt.join("target/debug/deps").exists());
    reclaim::delete_artifact(&wt.join("node_modules"), &wt, false).unwrap();
    reclaim::delete_artifact(&wt.join("target"), &wt, false).unwrap();
    assert!(wt.join("src/keep.rs").exists() && wt.join("Cargo.toml").exists());
    // a shared folder goes whole only when cargo tagged it
    let shared = p.join("cache-target");
    write(&shared, "debug/x", "x");
    assert_eq!(reclaim::delete_artifact(&shared, &shared, true), Err(Skip::NotArtifact));
    write(&shared, "CACHEDIR.TAG", "Signature");
    reclaim::delete_artifact(&shared, &shared, true).unwrap();
    assert!(!shared.exists());
}

/// main with one commit; `done` (merged), `wip` (own commit), `dirty` (merged, untracked file).
fn repo_with_worktrees() -> (tempfile::TempDir, PathBuf, Repo) {
    let (d, p) = tmp();
    let main = p.join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    write(&main, ".gitignore", "target/\nnode_modules/\n");
    write(&main, "src/main.rs", &"x".repeat(5_000));
    write(&main, "Cargo.toml", "[package]\nname = \"x\"\n");
    git(&main, &["add", "."]);
    git(&main, &["commit", "-q", "-m", "base"]);
    for (name, extra) in [("done", false), ("wip", true), ("dirty", false)] {
        let wt = p.join(name);
        git(&main, &["worktree", "add", "-q", "-b", name, wt.to_str().unwrap()]);
        if extra {
            commit(&wt, "wip.txt", "wip\n", "wip");
        }
        rust_project(&wt);
    }
    write(&p.join("dirty"), "notes.txt", "uncommitted");
    let r = Repo::discover(&main).unwrap();
    (d, p, r)
}

fn real_plan(r: &Repo, stale_days: u32) -> Vec<Suggestion> {
    let bases = r.base_branches(None);
    let entries = r.worktree_overview(&bases).unwrap();
    let sizes: HashMap<PathBuf, DiskUsage> = entries.iter().map(|e| (e.wt.path.clone(), repo::disk_usage(&e.wt.path, &[], &Cancel::default()))).collect();
    let protected = vec!["main".to_string()];
    reclaim::plan(&PlanInput { entries: &entries, usage: &sizes, shared: &[], current: None, protected: &protected, now: NOW, stale_days })
}

#[test]
fn merged_worktrees_are_found_and_removed_safely() {
    let (_d, p, r) = repo_with_worktrees();
    let plan = real_plan(&r, 100_000);
    let merged: Vec<&Path> = plan.iter().filter(|s| s.kind == ReclaimKind::MergedWorktree).map(|s| s.owner.as_path()).collect();
    assert_eq!(merged, [p.join("done")]);
    assert!(plan.iter().all(|s| !s.owner.ends_with("dirty")));
    // wip has its own commit: only its incremental caches (not stale with 100 000 days)
    let wip: Vec<ReclaimKind> = plan.iter().filter(|s| s.owner.ends_with("wip")).map(|s| s.kind).collect();
    assert_eq!(wip, [ReclaimKind::Incremental]);

    // after planning, a new commit lands in `done`: it is skipped, not removed
    let done = p.join("done");
    commit(&done, "late.txt", "late\n", "late");
    let rep = r.reclaim(&plan, "refs/heads/main", true, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(done.exists());
    assert!(rep.skipped.contains(&(done.clone(), Skip::NotMerged)), "{rep:?}");
    // the incremental caches went, sources stayed
    assert!(!p.join("wip/target/debug/incremental").exists() && p.join("wip/target/debug/deps").exists());
    assert!(p.join("wip/wip.txt").exists());

    // undo the late commit: now it goes, with its merged branch
    git(&done, &["reset", "-q", "--keep", "HEAD~1"]);
    let plan = real_plan(&r, 100_000);
    let only: Vec<Suggestion> = plan.into_iter().filter(|s| s.kind == ReclaimKind::MergedWorktree).collect();
    let rep = r.reclaim(&only, "refs/heads/main", true, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert_eq!(rep.done, vec![done.clone()]);
    assert_eq!(rep.branches_deleted, ["done"]);
    assert!(!done.exists());
    assert!(rep.freed > 0);
}

#[test]
fn stale_build_folders_go_but_never_with_changes() {
    let (_d, p, r) = repo_with_worktrees();
    // `now` is far in the future of these files: everything is stale
    let plan = real_plan(&r, 14);
    let wip: Vec<ReclaimKind> = plan.iter().filter(|s| s.owner.ends_with("wip")).map(|s| s.kind).collect();
    assert_eq!(wip, [ReclaimKind::StaleTarget, ReclaimKind::StaleDeps]);
    // a change appears in wip after planning: nothing of it is touched
    write(&p.join("wip"), "fresh.txt", "new");
    let rep = r.reclaim(&plan, "refs/heads/main", false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(p.join("wip/target").exists() && p.join("wip/node_modules").exists());
    assert!(rep.skipped.iter().any(|(x, s)| x.ends_with("wip") && *s == Skip::Changed), "{rep:?}");
    // without it, wip's build folders go and its sources stay
    std::fs::remove_file(p.join("wip/fresh.txt")).unwrap();
    let rep = r.reclaim(&plan, "refs/heads/main", false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(!p.join("wip/target").exists() && !p.join("wip/node_modules").exists());
    assert!(p.join("wip/Cargo.toml").exists() && p.join("wip/src/main.rs").exists());
    assert!(rep.branches_deleted.is_empty());
}

#[test]
fn tracked_build_folders_are_kept() {
    let (_d, p, r) = repo_with_worktrees();
    let wip = p.join("wip");
    // someone committed node_modules on purpose
    git(&wip, &["add", "-f", "node_modules"]);
    git(&wip, &["commit", "-q", "-m", "vendored deps"]);
    let plan = real_plan(&r, 14);
    let rep = r.reclaim(&plan, "refs/heads/main", false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(wip.join("node_modules/a/index.js").exists());
    assert!(rep.skipped.contains(&(wip.join("node_modules"), Skip::Tracked)), "{rep:?}");
    assert!(!wip.join("target").exists());
}
