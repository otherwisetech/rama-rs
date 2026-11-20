//! Read-only: time the worktree overview and the merged-branch scan of a repository.
//! `cargo run --release -p rama-repo --example worktrees -- path/to/repo`
use std::time::Instant;

fn main() -> repo::Result<()> {
    repo::shell_env::init();
    let path = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let r = repo::Repo::discover(std::path::Path::new(&path))?;
    let t = Instant::now();
    let bases = r.base_branches(None);
    println!("bases {:?} in {:?}", bases.iter().map(|b| b.short_rev()).collect::<Vec<_>>(), t.elapsed()); // i18n-ok: dev tool
    let t = Instant::now();
    let all = r.worktree_overview(&bases)?;
    println!("{} worktrees in {:?}", all.len(), t.elapsed()); // i18n-ok: dev tool
    for e in &all {
        println!("  {} {:?} changed={} vs={:?} merged={} gone={}", e.wt.path.display(), e.wt.branch, e.info.changed(), e.info.vs_default(), e.info.merged, e.info.gone); // i18n-ok: dev tool
    }
    let t = Instant::now();
    let protected: Vec<String> = repo::PROTECTED.iter().map(|s| s.to_string()).collect();
    let c = r.cleanup_candidates(&bases[0], &protected, true, &repo::Cancel::default(), &|_, _| {})?;
    let squashed = c.iter().filter(|c| matches!(c.reason, repo::Reason::Squashed { .. })).count();
    println!("{} cleanup candidates ({squashed} squash-merged) in {:?}", c.len(), t.elapsed()); // i18n-ok: dev tool
    Ok(())
}
