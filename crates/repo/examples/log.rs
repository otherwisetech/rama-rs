//! `cargo run --release -p rama-repo --example log -- <repo> [limit] [query]`: load history (and search it), timing both.
fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let limit: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(50_000);
    let t = std::time::Instant::now();
    let repo = repo::Repo::discover(path.as_ref()).expect("open");
    let h = repo.history(limit).expect("history");
    let width = h.rows.iter().map(|r| r.width).max().unwrap_or(0);
    println!("{} commits, {} refs, max {} lanes, truncated {} in {:?}", h.commits.len(), h.refs.len(), width, h.truncated, t.elapsed());
    for c in h.commits.iter().take(5) {
        println!("{}  {}  {}", c.short(), c.author, c.summary);
    }
    if let Some(q) = std::env::args().nth(3) {
        let t = std::time::Instant::now();
        let hits = h.search(&q);
        println!("search {q:?}: {} hits in {:?}", hits.len(), t.elapsed());
    }
    if let Some(c) = h.commits.first() {
        let t = std::time::Instant::now();
        let ch = repo.changes(c.id).expect("changes");
        println!("{} files changed in {:?}", ch.len(), t.elapsed());
    }
}
