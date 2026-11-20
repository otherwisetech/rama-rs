//! Working copy and write operations against throwaway repositories.
use repo::{Cancel, FileState, Repo, WorkFile, shell_env};
use std::collections::HashMap;
use std::path::Path;
use std::process::Command;
use std::sync::Once;

/// The operations under test spawn `git` with the process environment, so isolate it from the
/// user's global / system config (hooksPath, gpgsign, …) once for the whole test binary.
fn setup() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let dir = std::env::temp_dir().join(format!("rama-test-home-{}", std::process::id()));
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
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(p, body).unwrap();
}

/// One commit on main with a.txt and b.txt.
fn base() -> tempfile::TempDir {
    setup();
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q", "-b", "main"]);
    write(p, "a.txt", "one\ntwo\nthree\n");
    write(p, "b.txt", "bee\n");
    git(p, &["add", "a.txt", "b.txt"]);
    git(p, &["commit", "-q", "-m", "base"]);
    d
}

fn open(d: &tempfile::TempDir) -> Repo {
    Repo::discover(d.path()).unwrap()
}

fn head(d: &Path) -> String {
    git(d, &["rev-parse", "HEAD"]).trim().to_string()
}

fn wf(path: &str, state: FileState) -> WorkFile {
    WorkFile { path: path.into(), state }
}

#[cfg(unix)]
fn hook(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn status_parsing() {
    let raw = b"# branch.oid 1234567890abcdef1234567890abcdef12345678\0# branch.head main\0# branch.upstream origin/main\0# branch.ab +2 -1\0\
1 MM N... 100644 100644 100644 aaaa bbbb both.txt\0\
1 .D N... 100644 100644 000000 aaaa aaaa gone.txt\0\
2 R. N... 100644 100644 100644 aaaa aaaa R100 new name.txt\0old name.txt\0\
u UU N... 100644 100644 100644 100644 aaaa bbbb cccc clash.txt\0\
? dir/new file.txt\0";
    let s = repo::parse_status(raw);
    assert_eq!(s.branch.head.as_deref(), Some("main"));
    assert_eq!(s.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!((s.branch.ahead, s.branch.behind), (2, 1));
    assert_eq!(s.staged, vec![wf("both.txt", FileState::Modified), wf("new name.txt", FileState::Renamed { from: "old name.txt".into() })]);
    assert_eq!(
        s.unstaged,
        vec![wf("both.txt", FileState::Modified), wf("gone.txt", FileState::Deleted), wf("clash.txt", FileState::Conflicted), wf("dir/new file.txt", FileState::Untracked)]
    );
    let initial = repo::parse_status(b"# branch.oid (initial)\0# branch.head (detached)\0");
    assert!(initial.branch.oid.is_none() && initial.branch.head.is_none() && initial.is_clean());
}

#[test]
fn status_of_a_real_worktree() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    assert!(r.status().unwrap().is_clean());
    write(p, "a.txt", "one\nTWO\nthree\n");
    git(p, &["add", "a.txt"]);
    write(p, "a.txt", "one\nTWO\nthree\nfour\n");
    git(p, &["mv", "b.txt", "c.txt"]);
    write(p, "new dir/x y.txt", "x\n");
    let s = r.status().unwrap();
    assert_eq!(s.branch.head.as_deref(), Some("main"));
    assert_eq!(s.staged, vec![wf("a.txt", FileState::Modified), wf("c.txt", FileState::Renamed { from: "b.txt".into() })]);
    assert_eq!(s.unstaged, vec![wf("a.txt", FileState::Modified), wf("new dir/x y.txt", FileState::Untracked)]);
}

#[test]
fn conflicted_status() {
    let d = base();
    let p = d.path();
    git(p, &["checkout", "-q", "-b", "other"]);
    write(p, "a.txt", "other\n");
    git(p, &["commit", "-qam", "other"]);
    git(p, &["checkout", "-q", "main"]);
    write(p, "a.txt", "mine\n");
    git(p, &["commit", "-qam", "mine"]);
    let out = Command::new("git").args(["merge", "other"]).current_dir(p).output().unwrap();
    assert!(!out.status.success());
    let s = open(&d).status().unwrap();
    assert_eq!(s.unstaged, vec![wf("a.txt", FileState::Conflicted)]);
}

#[test]
fn stage_unstage_discard() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    write(p, "a.txt", "changed\n");
    std::fs::remove_file(p.join("b.txt")).unwrap();
    write(p, "u.txt", "untracked\n");
    r.stage(&["a.txt".into(), "b.txt".into()]).unwrap();
    let s = r.status().unwrap();
    assert_eq!(s.staged, vec![wf("a.txt", FileState::Modified), wf("b.txt", FileState::Deleted)]);
    assert_eq!(s.unstaged, vec![wf("u.txt", FileState::Untracked)]);

    r.unstage(&[wf("a.txt", FileState::Modified)]).unwrap();
    let s = r.status().unwrap();
    assert_eq!(s.staged, vec![wf("b.txt", FileState::Deleted)]);
    r.stage_all().unwrap();
    assert_eq!(r.status().unwrap().staged.len(), 3);
    r.unstage_all().unwrap();
    let s = r.status().unwrap();
    assert!(s.staged.is_empty());
    assert_eq!(s.unstaged.len(), 3);

    // untracked files go through the remover (the Trash in the app), tracked ones are restored
    let bin = tempfile::tempdir().unwrap();
    let moved = |paths: &[std::path::PathBuf]| -> repo::Result<()> {
        for f in paths {
            std::fs::rename(f, bin.path().join(f.file_name().unwrap()))?;
        }
        Ok(())
    };
    r.discard_with(&s.unstaged, moved).unwrap();
    assert!(r.status().unwrap().is_clean());
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "one\ntwo\nthree\n");
    assert!(p.join("b.txt").exists());
    assert!(bin.path().join("u.txt").exists() && !p.join("u.txt").exists());
}

#[test]
fn discard_keeps_staged_content() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    write(p, "a.txt", "staged\n");
    r.stage(&["a.txt".into()]).unwrap();
    write(p, "a.txt", "staged\nand more\n");
    r.discard_with(&[wf("a.txt", FileState::Modified)], |_| Ok(())).unwrap();
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "staged\n");
    assert_eq!(r.status().unwrap().staged, vec![wf("a.txt", FileState::Modified)]);
}

#[test]
fn unstage_before_the_first_commit() {
    setup();
    let d = tempfile::tempdir().unwrap();
    git(d.path(), &["init", "-q", "-b", "main"]);
    write(d.path(), "a.txt", "a\n");
    let r = open(&d);
    r.stage_all().unwrap();
    assert_eq!(r.status().unwrap().staged, vec![wf("a.txt", FileState::Added)]);
    r.unstage(&[wf("a.txt", FileState::Added)]).unwrap();
    assert_eq!(r.status().unwrap().unstaged, vec![wf("a.txt", FileState::Untracked)]);
}

#[test]
fn work_diffs() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    write(p, "a.txt", "one\nTWO\nthree\n");
    r.stage_all().unwrap();
    write(p, "a.txt", "one\nTWO\nthree\nfour\n");
    write(p, "n.txt", "x\ny\n");
    let (lines, added, removed, binary) = r.work_diff(&wf("a.txt", FileState::Modified), true).unwrap();
    assert_eq!((added, removed, binary), (1, 1, false));
    assert!(lines.iter().any(|l| l.kind == repo::LineKind::Removed && l.text == "two" && l.old_no == Some(2)));
    assert!(lines.iter().any(|l| l.kind == repo::LineKind::Added && l.text == "TWO" && l.new_no == Some(2)));
    let (lines, added, removed, _) = r.work_diff(&wf("a.txt", FileState::Modified), false).unwrap();
    assert_eq!((added, removed), (1, 0));
    assert!(lines.iter().any(|l| l.kind == repo::LineKind::Added && l.text == "four" && l.new_no == Some(4)));
    let (_, added, _, _) = r.work_diff(&wf("n.txt", FileState::Untracked), false).unwrap();
    assert_eq!(added, 2);
    let fc = repo::FileChange::detached("n.txt".into(), repo::Status::Added, added, 0, false);
    assert!(r.diff(&fc).unwrap().is_empty());
}

#[test]
fn unified_diff_parsing() {
    let text = "diff --git a/x b/x\nindex 1..2 100644\n--- a/x\n+++ b/x\n@@ -1,3 +1,3 @@ fn\n a\n-b\n+B\n c\n\\ No newline at end of file\n@@ -10 +10,2 @@\n x\n+y\n";
    let (lines, added, removed, binary) = repo::parse_unified(text);
    assert_eq!((added, removed, binary), (2, 1, false));
    assert_eq!(lines.iter().filter(|l| l.kind == repo::LineKind::Hunk).count(), 2);
    let y = lines.iter().find(|l| l.text == "y").unwrap();
    assert_eq!((y.old_no, y.new_no), (None, Some(11)));
    let b = lines.iter().find(|l| l.text == "b").unwrap();
    assert_eq!((b.old_no, b.new_no), (Some(2), None));
    assert!(repo::parse_unified("diff --git a/i b/i\nBinary files a/i and b/i differ\n").3);
}

#[test]
fn commit_and_amend() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    write(p, "a.txt", "new\n");
    r.stage_all().unwrap();
    let mut seen = Vec::new();
    let run = r.commit("Change a\n\nWith a body.", false, false, &Cancel::default(), &mut |l, _| seen.push(l.to_string())).unwrap();
    assert!(run.success, "{}", run.output);
    assert!(run.failed_hook.is_none());
    assert_eq!(r.head_message().as_deref(), Some("Change a\n\nWith a body."));
    assert!(r.status().unwrap().is_clean());
    let first = head(p);
    let run = r.commit("Change a, reworded", true, false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(run.success);
    assert_ne!(head(p), first);
    assert_eq!(git(p, &["rev-list", "--count", "HEAD"]).trim(), "2");
    assert_eq!(r.head_message().as_deref(), Some("Change a, reworded"));
    // nothing staged: git refuses, and it was not a hook
    let run = r.commit("empty", false, false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(!run.success && run.failed_hook.is_none());
}

#[cfg(unix)]
#[test]
fn failing_pre_commit_hook_blocks_the_commit() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    hook(&p.join(".git/hooks/pre-commit"), "#!/bin/sh\necho 'lint: 2 problems' >&2\nexit 1\n");
    write(p, "a.txt", "new\n");
    r.stage_all().unwrap();
    let before = head(p);
    let mut lines = Vec::new();
    let run = r.commit("blocked", false, false, &Cancel::default(), &mut |l, _| lines.push(l.to_string())).unwrap();
    assert!(!run.success);
    assert_eq!(run.failed_hook.as_deref(), Some("pre-commit"));
    assert!(run.output.contains("lint: 2 problems"));
    assert!(lines.iter().any(|l| l.contains("lint: 2 problems")));
    assert_eq!(head(p), before);
    assert_eq!(r.status().unwrap().staged.len(), 1);
    // the explicit escape hatch
    let run = r.commit("forced past hooks", false, true, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(run.success);
    assert_ne!(head(p), before);
}

#[cfg(unix)]
#[test]
fn husky_style_hooks_path_is_honored() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    // husky v9: core.hooksPath = .husky/_, whose stub runs the user's .husky/pre-commit
    hook(&p.join(".husky/_/pre-commit"), "#!/bin/sh\nsh \"$(dirname \"$0\")/../pre-commit\"\n");
    std::fs::write(p.join(".husky/pre-commit"), "echo ran > hook-ran.txt\necho 'running lint-staged'\n").unwrap();
    git(p, &["config", "core.hooksPath", ".husky/_"]);
    // a hook in .git/hooks must be ignored once hooksPath is set
    hook(&p.join(".git/hooks/pre-commit"), "#!/bin/sh\nexit 1\n");
    write(p, "a.txt", "new\n");
    r.stage(&["a.txt".into()]).unwrap();
    let run = r.commit("with husky", false, false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(run.success, "{}", run.output);
    assert!(run.output.contains("running lint-staged"));
    assert!(p.join("hook-ran.txt").exists());

    std::fs::write(p.join(".husky/pre-commit"), "echo 'husky - pre-commit script failed' >&2\nexit 1\n").unwrap();
    write(p, "a.txt", "newer\n");
    r.stage(&["a.txt".into()]).unwrap();
    let run = r.commit("rejected", false, false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(!run.success);
    assert_eq!(run.failed_hook.as_deref(), Some("pre-commit"));
}

#[cfg(unix)]
#[test]
fn commit_msg_hook_edits_the_message() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    hook(&p.join(".git/hooks/commit-msg"), "#!/bin/sh\nprintf '\\nReviewed-by: Hook\\n' >> \"$1\"\n");
    write(p, "a.txt", "new\n");
    r.stage_all().unwrap();
    let run = r.commit("Tidy", false, false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(run.success, "{}", run.output);
    assert_eq!(r.head_message().as_deref(), Some("Tidy\n\nReviewed-by: Hook"));

    hook(&p.join(".git/hooks/commit-msg"), "#!/bin/sh\necho 'subject must be lowercase' >&2\nexit 1\n");
    write(p, "a.txt", "newer\n");
    r.stage_all().unwrap();
    let run = r.commit("Bad", false, false, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert_eq!(run.failed_hook.as_deref(), Some("commit-msg"));
}

#[cfg(unix)]
#[test]
fn cancelling_stops_a_running_hook() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    hook(&p.join(".git/hooks/pre-commit"), "#!/bin/sh\necho started\nsleep 30\n");
    write(p, "a.txt", "new\n");
    r.stage_all().unwrap();
    let cancel = Cancel::default();
    let c2 = cancel.clone();
    let t = std::time::Instant::now();
    let run = r
        .commit("slow", false, false, &cancel, &mut move |l, _| {
            if l == "started" {
                c2.cancel();
            }
        })
        .unwrap();
    assert!(run.cancelled && !run.success);
    assert!(t.elapsed().as_secs() < 10, "took {:?}", t.elapsed());
    assert_eq!(git(p, &["rev-list", "--count", "HEAD"]).trim(), "1");
}

#[test]
fn trace2_hook_detection() {
    let events = r#"{"event":"version","sid":"x","evt":"4"}
{"event":"child_start","child_id":0,"child_class":"hook","use_shell":false,"hook_name":"pre-commit","argv":[".git/hooks/pre-commit"]}
{"event":"child_exit","child_id":0,"pid":12,"code":0,"t_rel":0.1}
{"event":"child_start","child_id":1,"child_class":"hook","use_shell":false,"hook_name":"commit-msg","argv":["x"]}
{"event":"child_exit","child_id":1,"pid":13,"code":1,"t_rel":0.1}"#;
    assert_eq!(repo::failed_hook(events).as_deref(), Some("commit-msg"));
    assert_eq!(repo::failed_hook(""), None);
    assert_eq!(repo::strip_ansi("\u{1b}[31merror\u{1b}[0m: x"), "error: x");
}

#[test]
fn stash_push_apply_pop_drop() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    write(p, "a.txt", "wip one\n");
    r.stash_push("first", false).unwrap();
    assert!(r.status().unwrap().is_clean());
    write(p, "b.txt", "wip two\n");
    write(p, "new.txt", "untracked\n");
    r.stash_push("second", true).unwrap();
    assert!(r.status().unwrap().is_clean(), "untracked file stashed too");
    let list = r.stashes().unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!((list[0].index, list[0].message.as_str()), (0, "On main: second"));
    assert_eq!(list[1].name(), "stash@{1}");
    assert!(r.changes(list[1].id).unwrap().iter().any(|c| c.path == "a.txt"));

    r.stash_apply(1).unwrap();
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "wip one\n");
    assert_eq!(r.stashes().unwrap().len(), 2);
    r.discard_with(&[wf("a.txt", FileState::Modified)], |_| Ok(())).unwrap();
    r.stash_pop(0).unwrap();
    assert!(p.join("new.txt").exists());
    assert_eq!(r.stashes().unwrap().len(), 1);
    r.stash_drop(0).unwrap();
    assert!(r.stashes().unwrap().is_empty());
}

#[test]
fn branches_checkout_create_delete() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    let base_id = r.history(10).unwrap().commits[0].id;
    r.create_branch("topic", base_id, false).unwrap();
    assert_eq!(r.status().unwrap().branch.head.as_deref(), Some("main"));
    r.switch("topic").unwrap();
    assert_eq!(r.status().unwrap().branch.head.as_deref(), Some("topic"));
    write(p, "t.txt", "topic\n");
    r.stage_all().unwrap();
    assert!(r.commit("on topic", false, false, &Cancel::default(), &mut |_, _| {}).unwrap().success);
    r.create_branch("other", base_id, true).unwrap();
    assert_eq!(r.status().unwrap().branch.head.as_deref(), Some("other"));
    // unmerged: `git branch -d` refuses (-D needs its own, second confirmation in the app)
    let err = r.delete_branch("topic").unwrap_err();
    assert!(err.to_string().contains("not fully merged"), "{err}");
    r.switch("main").unwrap();
    r.delete_branch("other").unwrap();
    let names: Vec<String> = r.history(10).unwrap().refs.iter().map(|x| x.name.clone()).collect();
    assert_eq!(names, ["main", "topic"]);
    assert!(r.switch("no-such-branch").is_err());
}

#[test]
fn checkout_refused_when_changes_would_be_lost() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    git(p, &["checkout", "-q", "-b", "other"]);
    write(p, "a.txt", "other\n");
    git(p, &["commit", "-qam", "other"]);
    git(p, &["checkout", "-q", "main"]);
    write(p, "a.txt", "dirty\n");
    let err = r.switch("other").unwrap_err().to_string();
    assert!(err.contains("a.txt"), "{err}");
    assert_eq!(std::fs::read_to_string(p.join("a.txt")).unwrap(), "dirty\n");
}

#[test]
fn push_sets_upstream_then_fetch_and_pull() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(p, &["remote", "add", "origin", remote.path().to_str().unwrap()]);
    let st = r.status().unwrap();
    assert!(st.branch.upstream.is_none());
    let args = r.push_args(&st.branch).unwrap();
    assert_eq!(args, ["push", "--progress", "--set-upstream", "origin", "main"]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let run = r.run(&args, &Cancel::default(), &mut |_, _| {}).unwrap();
    assert!(run.success, "{}", run.output);
    let st = r.status().unwrap();
    assert_eq!(st.branch.upstream.as_deref(), Some("origin/main"));
    assert_eq!(r.push_args(&st.branch).unwrap(), ["push", "--progress"]);

    // someone else pushes; fetch shows we're behind, pull brings it in
    let other = tempfile::tempdir().unwrap();
    git(other.path(), &["clone", "-q", remote.path().to_str().unwrap(), "."]);
    write(other.path(), "c.txt", "from elsewhere\n");
    git(other.path(), &["add", "c.txt"]);
    git(other.path(), &["commit", "-qm", "elsewhere"]);
    git(other.path(), &["push", "-q"]);
    assert!(r.run(&["fetch", "--all", "--progress"], &Cancel::default(), &mut |_, _| {}).unwrap().success);
    assert_eq!(r.status().unwrap().branch.behind, 1);
    assert!(r.run(&["pull", "--no-edit", "--progress"], &Cancel::default(), &mut |_, _| {}).unwrap().success);
    assert!(p.join("c.txt").exists());
}

#[test]
fn login_shell_env_parsing_and_merge() {
    let raw = b"Last login: today\nwelcome banner\n__RAMA_ENV_START__PATH=/Users/a/.volta/bin:/opt/homebrew/bin:/usr/bin\0NVM_DIR=/Users/a/.nvm\0MULTI=line one\nline two\0PWD=/somewhere\0SHLVL=2\0HOME=/Users/a\0__RAMA_ENV_END__trailing noise";
    let env = shell_env::parse_env_block(raw).unwrap();
    assert_eq!(env["PATH"], "/Users/a/.volta/bin:/opt/homebrew/bin:/usr/bin");
    assert_eq!(env["MULTI"], "line one\nline two");
    assert!(shell_env::parse_env_block(b"no markers here").is_none());

    let current: HashMap<String, String> = [("PATH", "/usr/bin:/bin"), ("HOME", "/Users/a")].into_iter().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    let merged: HashMap<String, String> = shell_env::merge(&current, Some(&env)).into_iter().collect();
    if cfg!(windows) {
        return;
    }
    // shell PATH first, the inherited entries that were missing appended once
    assert_eq!(merged["PATH"], "/Users/a/.volta/bin:/opt/homebrew/bin:/usr/bin:/bin");
    assert_eq!(merged["NVM_DIR"], "/Users/a/.nvm");
    assert!(!merged.contains_key("PWD") && !merged.contains_key("SHLVL"));
    // variables we already have are not overridden
    assert!(!merged.contains_key("HOME"));
    let alone: HashMap<String, String> = shell_env::merge(&current, None).into_iter().collect();
    assert!(alone["PATH"].starts_with("/usr/bin:/bin"));
}

#[test]
fn rename_and_force_delete_unmerged_branch() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    git(p, &["checkout", "-q", "-b", "topic"]);
    write(p, "t.txt", "topic\n");
    git(p, &["add", "t.txt"]);
    git(p, &["commit", "-qm", "topic"]);
    git(p, &["checkout", "-q", "main"]);
    git(p, &["branch", "merged"]);
    assert!(r.branch_merged("merged"));
    assert!(!r.branch_merged("topic"));
    r.rename_branch("topic", "topic-2").unwrap();
    assert!(r.rename_branch("topic-2", "merged").is_err(), "never overwrites with -M");
    assert!(r.delete_branch("topic-2").is_err());
    r.delete_branch_force("topic-2").unwrap();
    let names: Vec<String> = r.history(10).unwrap().refs.iter().map(|x| x.name.clone()).collect();
    assert_eq!(names, ["main", "merged"]);
}

#[test]
fn branch_push_pull_and_delete_remote_against_a_bare_remote() {
    let d = base();
    let p = d.path();
    let r = open(&d);
    let remote = tempfile::tempdir().unwrap();
    git(remote.path(), &["init", "-q", "--bare", "-b", "main"]);
    git(p, &["remote", "add", "origin", remote.path().to_str().unwrap()]);
    let run = |args: Vec<String>| {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        let out = r.run(&a, &Cancel::default(), &mut |_, _| {}).unwrap();
        assert!(out.success, "{a:?}: {}", out.output);
    };
    run(r.push_branch_args("main", true).unwrap());

    // a branch that is not checked out: first push sets its upstream, the next goes to it
    git(p, &["branch", "feature/x"]);
    assert_eq!(r.push_branch_args("feature/x", false).unwrap(), ["push", "--progress", "--set-upstream", "origin", "feature/x"]);
    run(r.push_branch_args("feature/x", false).unwrap());
    assert_eq!(r.upstreams().unwrap().get("feature/x").map(String::as_str), Some("origin/feature/x"));
    assert_eq!(r.push_branch_args("feature/x", false).unwrap(), ["push", "--progress", "origin", "refs/heads/feature/x:refs/heads/feature/x"]);
    assert_eq!(r.push_branch_args("main", true).unwrap(), ["push", "--progress"]);

    // someone else moves feature/x; fast-forward it without checking it out
    let other = tempfile::tempdir().unwrap();
    git(other.path(), &["clone", "-q", "-b", "feature/x", remote.path().to_str().unwrap(), "."]);
    write(other.path(), "x.txt", "x\n");
    git(other.path(), &["add", "x.txt"]);
    git(other.path(), &["commit", "-qm", "x"]);
    git(other.path(), &["push", "-q"]);
    let theirs = head(other.path());
    run(r.pull_ff_args("feature/x", false).unwrap());
    assert_eq!(git(p, &["rev-parse", "feature/x"]).trim(), theirs);
    assert_eq!(r.pull_ff_args("main", true).unwrap(), ["pull", "--ff-only", "--progress"]);
    assert!(r.pull_ff_args("no-upstream", false).is_err());

    // diverged: the fast-forward-only fetch refuses
    git(p, &["checkout", "-q", "feature/x"]);
    write(p, "y.txt", "mine\n");
    git(p, &["add", "y.txt"]);
    git(p, &["commit", "-qm", "mine"]);
    git(p, &["checkout", "-q", "main"]);
    write(other.path(), "z.txt", "theirs\n");
    git(other.path(), &["add", "z.txt"]);
    git(other.path(), &["commit", "-qm", "theirs"]);
    git(other.path(), &["push", "-q"]);
    let args = r.pull_ff_args("feature/x", false).unwrap();
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    assert!(!r.run(&a, &Cancel::default(), &mut |_, _| {}).unwrap().success);

    // delete the remote branch
    // newer git refuses remote names containing '/'; only check the split where it's allowed
    let added = Command::new("git").args(["remote", "add", "origin/mirror", remote.path().to_str().unwrap()]).current_dir(p).output().expect("git runs");
    if added.status.success() {
        assert_eq!(r.split_remote("origin/mirror/feature/x").unwrap(), ("origin/mirror".to_string(), "feature/x".to_string()));
    }
    assert!(r.split_remote("upstream/main").is_err());
    let args = r.delete_remote_args("origin/feature/x").unwrap();
    assert_eq!(args, ["push", "origin", "--delete", "feature/x"]);
    run(args);
    let left = git(remote.path(), &["branch", "--list"]);
    assert!(!left.contains("feature/x"), "{left}");
    assert!(left.contains("main"));
}
