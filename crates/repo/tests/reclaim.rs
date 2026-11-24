//! Disk reclaim: sizes of build folders, the cleanup plan's safety rules and order, and
//! running it — all against throwaway folders and repositories, with a fake home folder and a
//! fake Trash (a folder in the temp dir): never the real ones.
use repo::reclaim::{self, Category, PlanInput, ReclaimKind, Report, Skip, Suggestion};
use repo::safety;
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

fn canon(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

fn tmp() -> (tempfile::TempDir, PathBuf) {
    setup();
    let d = tempfile::tempdir().unwrap();
    let p = std::fs::canonicalize(d.path()).unwrap();
    // git can't create folders under Windows' `\\?\` paths
    #[cfg(windows)]
    let p = PathBuf::from(p.to_string_lossy().trim_start_matches(r"\\?\").to_string());
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
    write(p, "package.json", "{}");
    write(p, "node_modules/a/index.js", &"n".repeat(20_000));
}

/// A Trash that is a folder of the temp dir: what goes there can be looked at afterwards.
struct FakeTrash {
    dir: PathBuf,
    got: Vec<PathBuf>,
}

impl FakeTrash {
    fn new(root: &Path) -> Self {
        let dir = root.join("fake-trash");
        std::fs::create_dir_all(&dir).unwrap();
        FakeTrash { dir, got: Vec::new() }
    }

    fn remover(&mut self) -> impl FnMut(&Path) -> Result<(), String> + '_ {
        |p: &Path| {
            let to = self.dir.join(format!("{}-{}", self.got.len(), p.file_name().unwrap().to_string_lossy()));
            std::fs::rename(p, &to).map_err(|e| e.to_string())?;
            self.got.push(p.to_path_buf());
            Ok(())
        }
    }

    /// Where `name` went (the last one of that name).
    fn find(&self, name: &str) -> PathBuf {
        let mut hits: Vec<PathBuf> = std::fs::read_dir(&self.dir).unwrap().flatten().map(|e| e.path()).filter(|p| p.file_name().unwrap().to_string_lossy().ends_with(&format!("-{name}"))).collect();
        hits.sort();
        hits.pop().unwrap_or_else(|| panic!("{name} not in the fake Trash"))
    }
}

fn never(p: &Path) -> Result<(), String> {
    panic!("must not remove {}", p.display())
}

#[test]
fn sizes_find_build_folders_and_incremental_caches() {
    let (_d, p) = tmp();
    rust_project(&p);
    // not build folders: `.next` without package.json, `target` of a non-cargo project
    write(&p, ".next/cache.bin", &"c".repeat(10_000));
    write(&p, "docs/target/page.md", "hello");
    // a node_modules with no package.json next to it is a tool's or a global install
    write(&p, "tools/lib/node_modules/x/index.js", &"g".repeat(10_000));
    // dist/ and build/ only next to a package.json (or a Gradle file for build/)
    write(&p, "docs/dist/x.html", "hello");
    write(&p, "web/package.json", "{}");
    write(&p, "web/dist/app.js", &"w".repeat(10_000));
    write(&p, "android/build.gradle", "");
    write(&p, "android/build/out.bin", &"a".repeat(10_000));
    write(&p, "android/.gradle/c.bin", &"a".repeat(10_000));
    write(&p, "ios/DerivedData/x.bin", &"a".repeat(10_000));
    let u = repo::disk_usage(&p, &[], &Cancel::default());
    let paths: Vec<&Path> = u.artifacts.iter().map(|(a, _)| a.as_path()).collect();
    assert_eq!(paths, [p.join(".next"), p.join("android/.gradle"), p.join("android/build"), p.join("ios/DerivedData"), p.join("node_modules"), p.join("target"), p.join("web/dist")]);
    let target = u.artifacts.iter().find(|(a, _)| a.ends_with("target")).unwrap().1;
    assert!(target >= 130_000, "{target}");
    let inc: Vec<&Path> = u.incremental.iter().map(|(a, _)| a.as_path()).collect();
    assert_eq!(inc, [p.join("target/aarch64-apple-darwin/release/incremental"), p.join("target/debug/incremental")]);
    assert!(u.incremental.iter().all(|(_, b)| *b >= 30_000));
    assert!(u.touched > 0);
    // with package.json, .next counts
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

fn plan_of(entries: &[WorktreeEntry], sizes: &HashMap<PathBuf, DiskUsage>, current: Option<&Path>) -> Vec<Suggestion> {
    let protected = vec!["main".to_string(), "release*".to_string()];
    reclaim::plan(&PlanInput { entries, usage: sizes, current, protected: &protected, home: None, now: NOW, stale_days: 14 })
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
    let plan = plan_of(&all, &sizes, Some(&p("app-current")));

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
    let one = plan_of(&[a.clone(), b.clone()], &sizes, None);
    let two = plan_of(&[b.clone(), a.clone()], &sizes, None);
    assert_eq!(one, two);
    let kinds: Vec<(ReclaimKind, String)> = one.iter().map(|s| (s.kind, s.owner.file_name().unwrap().to_string_lossy().into_owned())).collect();
    assert_eq!(kinds, [(ReclaimKind::MergedWorktree, "a".into()), (ReclaimKind::StaleTarget, "b".into()), (ReclaimKind::StaleDeps, "b".into())]);
    for s in &one {
        for (x, _) in &s.paths {
            assert!(reclaim::contained(x, &s.owner, true), "{x:?} outside {:?}", s.owner);
        }
    }
    // a cargo target folder elsewhere that b builds into is never offered, tagged or not
    assert!(one.iter().all(|s| s.paths.iter().all(|(x, _)| !x.starts_with(root.join("elsewhere")))));

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
    let home = Some(p.as_path());
    let wt = p.join("wt");
    git_init(&wt);
    rust_project(&wt);
    write(&p, "outside/target/CACHEDIR.TAG", "x");
    write(&wt, "src/keep.rs", "keep");
    let del = |x: &Path| reclaim::delete_artifact(x, &wt, home, &mut never);
    assert_eq!(del(&wt.join("src")), Err(Skip::NotArtifact));
    assert_eq!(del(&wt), Err(Skip::NotArtifact));
    assert_eq!(del(&p.join("outside/target")), Err(Skip::NotArtifact));
    assert_eq!(del(&wt.join("Cargo.toml")), Err(Skip::NotArtifact));
    // node_modules with no package.json next to it: an install, not a project's
    write(&wt, "tools/node_modules/x/i.js", "x");
    assert_eq!(del(&wt.join("tools/node_modules")), Err(Skip::NotArtifact));
    // build folders under a tool's folder inside the worktree
    write(&wt, ".cargo/registry/package.json", "{}");
    write(&wt, ".cargo/registry/node_modules/x/i.js", "x");
    assert_eq!(del(&wt.join(".cargo/registry/node_modules")), Err(Skip::Protected));
    // a dist/ git tracks is sources, not output
    write(&wt, "dist/index.js", "tracked");
    git(&wt, &["add", "-f", "dist/index.js"]);
    git(&wt, &["commit", "-q", "-m", "dist"]);
    assert_eq!(del(&wt.join("dist")), Err(Skip::Tracked));
    #[cfg(unix)]
    {
        // a link named like a build folder pointing elsewhere is never followed
        std::fs::create_dir_all(wt.join("web")).unwrap();
        write(&wt, "web/package.json", "{}");
        std::os::unix::fs::symlink(p.join("outside"), wt.join("web/node_modules")).unwrap();
        assert_eq!(del(&wt.join("web/node_modules")), Err(Skip::NotArtifact));
        // nor a real folder reached through a linked parent
        std::os::unix::fs::symlink(&wt, p.join("wt-link")).unwrap();
        assert_eq!(del(&p.join("wt-link/target")), Err(Skip::NotArtifact));
        assert!(p.join("outside/target/CACHEDIR.TAG").exists());
    }
    let mut trash = FakeTrash::new(&p);
    for x in ["target/debug/incremental", "node_modules", "target"] {
        reclaim::delete_artifact(&wt.join(x), &wt, home, &mut trash.remover()).unwrap();
    }
    assert_eq!(trash.got, [wt.join("target/debug/incremental"), wt.join("node_modules"), wt.join("target")]);
    // they went to the Trash, not away
    assert!(trash.find("node_modules").join("a/index.js").exists());
    assert!(wt.join("src/keep.rs").exists() && wt.join("Cargo.toml").exists());
    // a shared cargo folder outside the worktree is refused even with cargo's tag
    let shared = p.join("cache-target");
    write(&shared, "CACHEDIR.TAG", "Signature");
    assert_eq!(reclaim::delete_artifact(&shared, &shared, home, &mut never), Err(Skip::NotArtifact));
    assert!(shared.exists());
}

fn git_init(dir: &Path) {
    std::fs::create_dir_all(dir).unwrap();
    git(dir, &["init", "-q", "-b", "main"]);
    write(dir, ".gitignore", "target/\nnode_modules/\n");
    git(dir, &["add", "."]);
    git(dir, &["commit", "-q", "-m", "init"]);
}

/// main with one commit; `done` (merged), `wip` (own commit), `dirty` (merged, untracked file).
fn repo_with_worktrees() -> (tempfile::TempDir, PathBuf, Repo) {
    let (d, p) = tmp();
    let main = p.join("main");
    std::fs::create_dir(&main).unwrap();
    git(&main, &["init", "-q", "-b", "main"]);
    write(&main, ".gitignore", "target/\nnode_modules/\n.env\n*.local.md\n");
    write(&main, "src/main.rs", &"x".repeat(5_000));
    write(&main, "Cargo.toml", "[package]\nname = \"x\"\n");
    write(&main, "package.json", "{}");
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
    let home = r.root().parent().unwrap().to_path_buf();
    reclaim::plan(&PlanInput { entries: &entries, usage: &sizes, current: None, protected: &protected, home: Some(&home), now: NOW, stale_days })
}

fn run(r: &Repo, items: &[Suggestion], branches: bool, trash: &mut FakeTrash) -> Report {
    let home = r.root().parent().unwrap().to_path_buf();
    r.reclaim(items, "refs/heads/main", branches, Some(&home), &mut trash.remover(), &Cancel::default(), &mut |_, _| {}).unwrap()
}

#[test]
fn merged_worktrees_are_found_and_removed_safely() {
    let (_d, p, r) = repo_with_worktrees();
    let plan = real_plan(&r, 100_000);
    let merged: Vec<PathBuf> = plan.iter().filter(|s| s.kind == ReclaimKind::MergedWorktree).map(|s| canon(&s.owner)).collect();
    assert_eq!(merged, [canon(&p.join("done"))]);
    assert!(plan.iter().all(|s| !s.owner.ends_with("dirty")));
    // wip has its own commit: only its incremental caches (not stale with 100 000 days)
    let wip: Vec<ReclaimKind> = plan.iter().filter(|s| s.owner.ends_with("wip")).map(|s| s.kind).collect();
    assert_eq!(wip, [ReclaimKind::Incremental]);

    // after planning, a new commit lands in `done`: it is skipped, not removed
    let done = p.join("done");
    commit(&done, "late.txt", "late\n", "late");
    let mut trash = FakeTrash::new(&p);
    let rep = run(&r, &plan, true, &mut trash);
    assert!(done.exists());
    assert!(rep.skipped.iter().any(|(x, s)| x.ends_with("done") && *s == Skip::NotMerged), "{rep:?}");
    // the incremental caches went, sources stayed
    assert!(!p.join("wip/target/debug/incremental").exists() && p.join("wip/target/debug/deps").exists());
    assert!(p.join("wip/wip.txt").exists());

    // undo the late commit: now it goes, with its merged branch
    git(&done, &["reset", "-q", "--keep", "HEAD~1"]);
    let plan = real_plan(&r, 100_000);
    let only: Vec<Suggestion> = plan.into_iter().filter(|s| s.kind == ReclaimKind::MergedWorktree).collect();
    let rep = run(&r, &only, true, &mut trash);
    assert_eq!(rep.done.len(), 1);
    assert!(rep.done[0].ends_with("done"), "{rep:?}");
    assert_eq!(rep.branches_deleted, ["done"]);
    assert!(!done.exists());
    assert!(rep.freed > 0);
    // the folder is in the Trash, and git forgot the worktree
    assert!(trash.find("done").join("src/main.rs").exists());
    assert!(!git(&p.join("main"), &["worktree", "list", "--porcelain"]).contains("/done\n"));
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
    let mut trash = FakeTrash::new(&p);
    let rep = run(&r, &plan, false, &mut trash);
    assert!(p.join("wip/target").exists() && p.join("wip/node_modules").exists());
    assert!(rep.skipped.iter().any(|(x, s)| x.ends_with("wip") && *s == Skip::Changed), "{rep:?}");
    // without it, wip's build folders go and its sources stay
    std::fs::remove_file(p.join("wip/fresh.txt")).unwrap();
    let rep = run(&r, &plan, false, &mut trash);
    assert!(!p.join("wip/target").exists() && !p.join("wip/node_modules").exists());
    assert!(trash.got.contains(&p.join("wip/target")) && trash.got.contains(&p.join("wip/node_modules")));
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
    let rep = run(&r, &plan, false, &mut FakeTrash::new(&p));
    assert!(wip.join("node_modules/a/index.js").exists());
    assert!(rep.skipped.iter().any(|(x, s)| x.ends_with("node_modules") && *s == Skip::Tracked), "{rep:?}");
    assert!(!wip.join("target").exists());
}

#[test]
fn merged_worktree_keeps_ignored_files_recoverable() {
    let (_d, p, r) = repo_with_worktrees();
    let done = p.join("done");
    // files git ignores: a plain `git worktree remove` would delete them for good
    write(&done, ".env", "SECRET=1\n");
    write(&done, "plan.local.md", "my notes\n");
    let keep = reclaim::untracked_keepers(&done).unwrap();
    // build output isn't listed, the rest is
    assert_eq!(keep, [".env", "plan.local.md"]);
    let plan: Vec<Suggestion> = real_plan(&r, 100_000).into_iter().filter(|s| s.kind == ReclaimKind::MergedWorktree).collect();
    assert_eq!(plan.len(), 1);
    assert_eq!(reclaim::category(plan[0].kind, &plan[0].owner), Category::Worktree);
    let mut trash = FakeTrash::new(&p);
    let rep = run(&r, &plan, false, &mut trash);
    assert_eq!(rep.done, [plan[0].owner.clone()], "{rep:?}");
    assert_eq!(rep.kept_in_trash, [(plan[0].owner.clone(), vec![".env".to_string(), "plan.local.md".to_string()])]);
    // nothing was deleted: the whole folder, .env included, is in the Trash
    let t = trash.find("done");
    assert_eq!(std::fs::read_to_string(t.join(".env")).unwrap(), "SECRET=1\n");
    assert!(t.join("plan.local.md").exists() && t.join("target/debug/deps/libx.rlib").exists());
    assert!(!done.exists());
    // its branch stays unless asked for
    assert!(git(&p.join("main"), &["branch", "--list", "done"]).contains("done"));
}

#[test]
fn worktrees_with_work_or_locks_are_never_removed() {
    let (_d, p, r) = repo_with_worktrees();
    let main = p.join("main");
    let plan: Vec<Suggestion> = real_plan(&r, 100_000).into_iter().filter(|s| s.kind == ReclaimKind::MergedWorktree).collect();
    let mut trash = FakeTrash::new(&p);
    // locked after planning
    git(&main, &["worktree", "lock", p.join("done").to_str().unwrap()]);
    let rep = run(&r, &plan, true, &mut trash);
    assert_eq!(rep.skipped, [(plan[0].owner.clone(), Skip::Locked)]);
    git(&main, &["worktree", "unlock", p.join("done").to_str().unwrap()]);
    // an untracked (not ignored) file: uncommitted work
    write(&p.join("done"), "draft.txt", "work");
    assert_eq!(run(&r, &plan, true, &mut trash).skipped, [(plan[0].owner.clone(), Skip::Changed)]);
    std::fs::remove_file(p.join("done/draft.txt")).unwrap();
    // merged into main but not pushed to its own upstream
    let remote = p.join("remote.git");
    git(&p, &["init", "-q", "--bare", remote.to_str().unwrap()]);
    git(&main, &["remote", "add", "origin", remote.to_str().unwrap()]);
    git(&p.join("done"), &["push", "-q", "-u", "origin", "done"]);
    commit(&main, "more.txt", "more\n", "more");
    git(&p.join("done"), &["merge", "-q", "--ff-only", "main"]);
    assert_eq!(run(&r, &plan, true, &mut trash).skipped, [(plan[0].owner.clone(), Skip::Unpushed)]);
    // the main worktree is never removed, even when handed over
    let mut fake = plan[0].clone();
    fake.owner = r.root();
    fake.paths = vec![(r.root(), 1)];
    assert_eq!(run(&r, &[fake], true, &mut trash).skipped, [(r.root(), Skip::NotArtifact)]);
    assert!(trash.got.is_empty(), "{:?}", trash.got);
    assert!(p.join("done/src/main.rs").exists() && main.join("src/main.rs").exists());
}

/// Every excluded place from diskmap's list, relative to the home folder.
const EXCLUDED: &[&str] = &[
    ".config/opencode", ".local/share/app", ".opencode", ".claude", ".claude/worktrees/x", ".codex/worktrees/x", ".pi", ".pi-agent", ".cursor/extensions/e", ".vscode/extensions/e", ".vscode-server/cli/servers/Stable-abc/server", ".npm/_npx/abc", ".nvm/versions/node/v20.1.0/lib", ".rustup/toolchains/stable", ".cargo/registry/src/x",
    ".cargo/git/checkouts/x", ".bun/install/global", ".deno", ".serverless/releases/4.17.1/package", ".docker", ".kube", ".aws", ".ssh", ".ollama/models", ".otherwise/rama", ".some-new-tool", ".cache/rama-target", "Library/Application Support/Code/User", "Library/Containers/com.x/Data",
    "Library/CloudStorage/OneDrive-Acme/proj", "Library/Mobile Documents/com~apple~CloudDocs/proj", "OneDrive - Acme/proj", "OneDrive/proj", "Dropbox/proj", "Google Drive/proj", "Applications/Tool.app/Contents/Resources/app", "code/Thing.app/Contents/Resources/app", "code/Lib.framework/Resources", "Pictures/proj", "Movies/proj", "Music/proj",
];

#[test]
fn excluded_places_are_never_offered() {
    let (_d, home) = tmp();
    let mut entries = Vec::new();
    let mut sizes = HashMap::new();
    // a checkout in each excluded place, every one with build folders and a merged branch
    for (i, e) in EXCLUDED.iter().chain(["", "code"].iter()).enumerate() {
        let wt = home.join(e);
        rust_project(&wt);
        entries.push(entry(&wt, Some(&format!("b{i}")), false));
        sizes.insert(wt.clone(), usage(&wt, NOW - 30 * DAY));
    }
    // a fine checkout, with build folders hiding under tool folders inside it
    let ok = home.join("code/app");
    rust_project(&ok);
    let mut u = usage(&ok, NOW - 30 * DAY);
    for inner in [".config/x/node_modules", ".cargo/registry/target", "Tool.app/Contents/node_modules", ".vscode-server/bin/node_modules"] {
        write(ok.join(inner).parent().unwrap(), "package.json", "{}");
        u.artifacts.push((ok.join(inner), 9_000));
    }
    let mut busy = entry(&ok, Some("busy"), false);
    (busy.info.merged, busy.info.vs) = (false, vec![("main".into(), 2, 0)]);
    entries.push(busy);
    sizes.insert(ok.clone(), u);
    let protected = vec!["main".to_string()];
    let plan = reclaim::plan(&PlanInput { entries: &entries, usage: &sizes, current: None, protected: &protected, home: Some(&home), now: NOW, stale_days: 14 });
    let offered: Vec<PathBuf> = plan.iter().flat_map(|s| s.paths.iter().map(|(p, _)| p.clone())).collect();
    for e in EXCLUDED {
        assert!(offered.iter().all(|p| !p.starts_with(home.join(e))), "offered from excluded {e}: {offered:?}");
        assert!(safety::place_protected(&home.join(e), Some(&home)), "{e}");
    }
    // the home folder itself is never a worktree to remove; ~/code is a fine checkout
    assert!(offered.iter().all(|p| *p != home));
    let mut want = vec![home.join("code"), ok.join("node_modules"), ok.join("target")];
    want.sort();
    let mut got = offered.clone();
    got.sort();
    assert_eq!(got, want, "only the checkouts outside excluded places and their own build folders");
    // and the remover refuses them when handed one directly
    for e in [".config/opencode", ".codex/worktrees/x", "Library/Mobile Documents/com~apple~CloudDocs/proj"] {
        let wt = home.join(e);
        git_init(&wt);
        assert_eq!(reclaim::delete_artifact(&wt.join("node_modules"), &wt, Some(&home), &mut never), Err(Skip::Protected), "{e}");
    }
    std::fs::create_dir_all(ok.join(".config/x/node_modules")).unwrap();
    assert_eq!(reclaim::delete_artifact(&ok.join(".config/x/node_modules"), &ok, Some(&home), &mut never), Err(Skip::Protected));
}

#[cfg(unix)]
#[test]
fn protected_mirrors_diskmap() {
    for p in ["/System/Library/x", "/usr/local/lib/node_modules", "/opt/homebrew/lib/node_modules", "/Applications/Foo.app/Contents/Resources/app/node_modules", "/Library/Caches/x", "/private/var/x", "/", "/Users", "/Users/ana", "/Users/ana/.config/opencode/node_modules", "/Users/ana/Documents", "/Volumes/Work/.ssh", "/Volumes/Work", "/Users/ana/node_modules", "relative/target"] {
        assert!(safety::protected(Path::new(p), None), "{p}");
    }
    for p in ["/Volumes/Work/code/app/target", "/Users/ana/code/app/node_modules", "/srv/code/app/target"] {
        assert!(!safety::protected(Path::new(p), None), "{p}");
    }
    let home = Path::new("/Users/ana");
    // a checkout straight in the home folder is fine (git registered it); home itself is not
    assert!(!safety::place_protected(Path::new("/Users/ana/app"), Some(home)));
    assert!(safety::place_protected(home, Some(home)));
    for p in ["/Users/ana/.codex/worktrees/x", "/Users/ana/Library/x", "/Users/ana/Dropbox/app", "/Users/ana/OneDrive - Acme/app", "/Users/ana/code/X.app/app", "/usr/src/app"] {
        assert!(safety::place_protected(Path::new(p), Some(home)), "{p}");
    }
    // a repository at a volume's root: its build folders may go, the folder never as a whole
    assert!(!safety::place_protected(Path::new("/Volumes/Work"), Some(home)));
    for p in ["/Volumes/Work", "/Users/ana", "/Users/bob", "/Users/ana/.codex/worktrees/x"] {
        assert!(safety::removal_protected(Path::new(p), Some(home)), "{p}");
    }
    assert!(!safety::removal_protected(Path::new("/Users/ana/app"), Some(home)) && !safety::removal_protected(Path::new("/Volumes/Work/app-wt"), Some(home)));
    assert!(safety::protected_below(Path::new("/w/app/.config/x/node_modules"), Path::new("/w/app")));
    assert!(!safety::protected_below(Path::new("/w/app/web/node_modules"), Path::new("/w/app")));
    assert_eq!(reclaim::category(ReclaimKind::StaleDeps, Path::new("/w/app/node_modules")), Category::NodeModules);
    assert_eq!(reclaim::category(ReclaimKind::StaleDeps, Path::new("/w/app/dist")), Category::BuildOutput);
}
