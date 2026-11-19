use repo::{LineKind, RefKind, Repo, Status};
use std::path::Path;
use std::process::Command;

/// `git` isolated from the user's and system config, with a fixed identity and date.
fn git_at(dir: &Path, args: &[&str], date: &str) {
    // untracked and never added: the fixtures only stage the files they name
    let config = dir.join(".test-gitconfig");
    if !config.exists() {
        std::fs::write(&config, "").unwrap();
    }
    let out = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Ana Test")
        .env("GIT_AUTHOR_EMAIL", "ana@example.com")
        .env("GIT_COMMITTER_NAME", "Ana Test")
        .env("GIT_COMMITTER_EMAIL", "ana@example.com")
        .env("GIT_AUTHOR_DATE", date)
        .env("GIT_COMMITTER_DATE", date)
        .env("GIT_CONFIG_GLOBAL", &config)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
}

fn git(dir: &Path, args: &[&str]) {
    git_at(dir, args, "2026-01-01T09:00:00Z");
}

fn commit(dir: &Path, file: &str, body: &str, msg: &str, date: &str) {
    std::fs::write(dir.join(file), body).unwrap();
    git(dir, &["add", "--", file]);
    git_at(dir, &["commit", "-q", "-m", msg], date);
}

/// main: base → edit a → merge(feature) → rename b   feature: base → add b
fn sample() -> tempfile::TempDir {
    let d = tempfile::tempdir().unwrap();
    let p = d.path();
    git(p, &["init", "-q", "-b", "main"]);
    commit(p, "a.txt", "one\ntwo\nthree\n", "base", "2026-01-01T10:00:00Z");
    git(p, &["checkout", "-q", "-b", "feature"]);
    commit(p, "b.txt", "feature\n", "add b", "2026-01-02T10:00:00Z");
    git(p, &["checkout", "-q", "main"]);
    commit(p, "a.txt", "one\nTWO\nthree\nfour\n", "edit a", "2026-01-03T10:00:00Z");
    git_at(p, &["merge", "-q", "--no-ff", "-m", "merge feature", "feature"], "2026-01-04T10:00:00Z");
    git(p, &["tag", "-a", "v1", "-m", "release one"]);
    git(p, &["mv", "b.txt", "c.txt"]);
    git_at(p, &["commit", "-q", "-m", "rename b"], "2026-01-05T10:00:00Z");
    d
}

#[test]
fn history_graph_and_refs() {
    let d = sample();
    let repo = Repo::discover(&d.path().join(".")).unwrap();
    let h = repo.history(1000).unwrap();
    let summaries: Vec<&str> = h.commits.iter().map(|c| c.summary.as_str()).collect();
    assert_eq!(summaries, ["rename b", "merge feature", "edit a", "add b", "base"]);
    assert!(!h.truncated);
    assert_eq!(h.head_branch.as_deref(), Some("main"));
    assert_eq!(h.head, Some(h.commits[0].id));
    let names: Vec<(&str, RefKind)> = h.refs.iter().map(|r| (r.name.as_str(), r.kind)).collect();
    assert_eq!(names, [("feature", RefKind::Local), ("main", RefKind::Local), ("v1", RefKind::Tag)]);
    // the annotated tag points at the merge commit, not the tag object
    let merge = h.index[&h.commits[1].id];
    assert!(h.refs_at(merge).any(|r| r.name == "v1"));
    // merge forks into a second lane that rejoins at base
    assert_eq!(h.rows[1].down.len(), 2);
    assert_eq!(h.rows[3].lane, 1);
    assert_eq!(h.rows[4].lane, 0);
    assert_eq!(h.commits[0].author, "Ana Test");
    assert_eq!(repo.message(h.commits[1].id).unwrap(), "merge feature");

    let small = repo.history(2).unwrap();
    assert!(small.truncated);
    assert_eq!(small.commits.len(), 2);
}

#[test]
fn changes_and_diff() {
    let d = sample();
    let repo = Repo::discover(d.path()).unwrap();
    let h = repo.history(100).unwrap();
    let edit = h.commits.iter().find(|c| c.summary == "edit a").unwrap().id;
    let ch = repo.changes(edit).unwrap();
    assert_eq!(ch.len(), 1);
    assert_eq!((ch[0].path.as_str(), &ch[0].status, ch[0].added, ch[0].removed), ("a.txt", &Status::Modified, 2, 1));
    let lines = repo.diff(&ch[0]).unwrap();
    assert_eq!(lines[0].kind, LineKind::Hunk);
    assert!(lines.iter().any(|l| l.kind == LineKind::Removed && l.text == "two"));
    assert!(lines.iter().any(|l| l.kind == LineKind::Added && l.text == "TWO" && l.new_no == Some(2)));

    let root = h.commits.last().unwrap().id;
    let rc = repo.changes(root).unwrap();
    assert_eq!((rc[0].status.clone(), rc[0].added), (Status::Added, 3));

    let rename = h.commits[0].id;
    let rn = repo.changes(rename).unwrap();
    assert_eq!(rn.len(), 1);
    assert_eq!(rn[0].status, Status::Renamed { from: "b.txt".into() });
    assert_eq!(rn[0].path, "c.txt");
}

#[test]
fn not_a_repository() {
    let d = tempfile::tempdir().unwrap();
    assert!(Repo::discover(d.path()).is_err());
}

#[test]
fn search_by_message_author_ref_and_sha() {
    let d = sample();
    let repo = Repo::discover(d.path()).unwrap();
    let h = repo.history(100).unwrap();
    let row = |summary: &str| h.commits.iter().position(|c| c.summary == summary).unwrap();
    assert_eq!(h.search("EDIT"), vec![row("edit a")]);
    // "feature" is in a merge message and is the name of the branch at "add b"
    assert_eq!(h.search("feature"), vec![row("merge feature"), row("add b")]);
    // tag name
    assert_eq!(h.search("v1"), vec![row("merge feature")]);
    assert_eq!(h.search("ana test").len(), h.commits.len());
    assert_eq!(h.search("example.com").len(), h.commits.len());
    let sha = h.commits[2].id.to_hex_with_len(7).to_string();
    assert!(h.search(&sha).contains(&2));
    assert!(h.search("   ").is_empty());
    assert!(h.search("nothing like this").is_empty());
}

#[test]
fn fold_ignores_case_and_accents() {
    assert_eq!(repo::fold("Café Ñandú ÉXITO"), "cafe nandu exito");
}
