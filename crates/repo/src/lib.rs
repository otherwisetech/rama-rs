//! A git repository: history laid out as a graph, refs, the files a commit changed and line
//! diffs, read through gitoxide; working-copy status and every write through the `git` CLI
//! (`work`, `git`). No UI.
use gix::bstr::ByteSlice;
use gix::ObjectId;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

pub use gix::ObjectId as Oid;

mod git;
pub mod shell_env;
mod work;
mod worktree;
mod cleanup;
pub mod reclaim;
pub mod swap;

pub use git::{Cancel, GitError, Run, failed_hook, strip_ansi, version as git_version};
pub use cleanup::{Candidate, PROTECTED, Reason, glob_match, is_protected};
pub use worktree::{BASE_NAMES, BaseRef, BranchFacts, DiskUsage, HEAVY_DIRS, is_artifact_dir, target_usage, NewWorktree, Worktree, WorktreeEntry, WorktreeInfo, default_worktree_path, disk_usage, parse_track, parse_worktree_list, worktree_add_args};
pub use work::{BranchStatus, FileState, Stash, WorkFile, WorkStatus, parse_stash_list, parse_status, parse_unified, split_remote_name};

pub type Error = Box<dyn std::error::Error + Send + Sync>;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone)]
pub struct Commit {
    pub id: ObjectId,
    pub summary: String,
    pub author: String,
    pub email: String,
    /// Author time, seconds since the epoch.
    pub time: i64,
    /// Offset from UTC in seconds of the author's clock.
    pub offset: i32,
    pub parents: Vec<ObjectId>,
}

impl Commit {
    pub fn short(&self) -> String {
        self.id.to_hex_with_len(7).to_string()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RefInfo {
    /// Short name: `main`, `origin/main`, `v1.0`.
    pub name: String,
    pub kind: RefKind,
    pub target: ObjectId,
}

/// Commits in display order (children before parents, newest first) with their graph rows.
#[derive(Debug, Clone)]
pub struct History {
    pub commits: Vec<Commit>,
    pub rows: Vec<graph::Row>,
    pub refs: Vec<RefInfo>,
    pub index: HashMap<ObjectId, usize>,
    pub head: Option<ObjectId>,
    /// Checked-out branch, `None` when detached.
    pub head_branch: Option<String>,
    /// The walk stopped at the limit; older history is not loaded.
    pub truncated: bool,
}

impl History {
    /// Rows matching `query`, top to bottom: in the summary, author name or email, a branch or tag
    /// name pointing at the commit, or a SHA prefix (4+ hex digits). Case- and accent-insensitive.
    pub fn search(&self, query: &str) -> Vec<usize> {
        let q = fold(query.trim());
        if q.is_empty() {
            return Vec::new();
        }
        let hex = q.len() >= 4 && q.chars().all(|c| c.is_ascii_hexdigit());
        let mut hit = vec![false; self.commits.len()];
        for r in &self.refs {
            if fold(&r.name).contains(&q)
                && let Some(&row) = self.index.get(&r.target)
            {
                hit[row] = true;
            }
        }
        let mut buf = String::new();
        for (row, c) in self.commits.iter().enumerate() {
            if hit[row] {
                continue;
            }
            hit[row] = (hex && c.id.to_hex().to_string().starts_with(&q)) || [&c.summary, &c.author, &c.email].iter().any(|f| {
                fold_into(f, &mut buf);
                buf.contains(&q)
            });
        }
        hit.iter().enumerate().filter_map(|(i, &h)| h.then_some(i)).collect()
    }

    /// Refs pointing at the commit in `row`, branches first.
    pub fn refs_at(&self, row: usize) -> impl Iterator<Item = &RefInfo> {
        let id = self.commits[row].id;
        self.refs.iter().filter(move |r| r.target == id)
    }
}

/// Where `query` matches a ref name, as byte ranges to highlight; ASCII case is ignored.
/// A plain substring wins; otherwise the query's words (split on `-`, `/`, `_`, `.`, spaces)
/// must each start a segment of the name, in order: `fe/lo` and `feat login` both match
/// `feature/user-login`. An empty query matches everything with no highlight.
pub fn ref_match(query: &str, name: &str) -> Option<Vec<std::ops::Range<usize>>> {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return Some(Vec::new());
    }
    let n = name.to_ascii_lowercase();
    if let Some(at) = n.find(&q) {
        return Some(vec![std::ops::Range { start: at, end: at + q.len() }]);
    }
    let sep = |c: char| matches!(c, '-' | '/' | '_' | '.' | ' ');
    let words: Vec<&str> = q.split(sep).filter(|w| !w.is_empty()).collect();
    if words.is_empty() {
        return None;
    }
    let mut starts = Vec::new();
    let mut prev_sep = true;
    for (i, c) in n.char_indices() {
        if prev_sep && !sep(c) {
            starts.push(i);
        }
        prev_sep = sep(c);
    }
    let mut out = Vec::with_capacity(words.len());
    let mut seg = starts.iter();
    'words: for w in words {
        for &s in seg.by_ref() {
            if n[s..].starts_with(w) {
                out.push(s..s + w.len());
                continue 'words;
            }
        }
        return None;
    }
    Some(out)
}

/// Lowercase without accents, for matching: "Café Ñandú" → "cafe nandu".
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    fold_into(s, &mut out);
    out
}

fn fold_into(s: &str, out: &mut String) {
    out.clear();
    for c in s.chars() {
        for l in c.to_lowercase() {
            out.push(match l {
                'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'í' | 'ì' | 'î' | 'ï' => 'i',
                'ó' | 'ò' | 'ô' | 'ö' | 'õ' => 'o',
                'ú' | 'ù' | 'û' | 'ü' => 'u',
                'ñ' => 'n',
                'ç' => 'c',
                other => other,
            });
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Added,
    Deleted,
    Modified,
    Renamed { from: String },
    Copied { from: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    pub path: String,
    pub status: Status,
    pub added: u32,
    pub removed: u32,
    pub binary: bool,
    pub(crate) old: Option<ObjectId>,
    pub(crate) new: Option<ObjectId>,
}

impl FileChange {
    /// A change that isn't between two blobs (a working-copy file); [`Repo::diff`] of it is empty.
    pub fn detached(path: String, status: Status, added: u32, removed: u32, binary: bool) -> FileChange {
        FileChange { path, status, added, removed, binary, old: None, new: None }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    Hunk,
    Context,
    Added,
    Removed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: LineKind,
    pub old_no: Option<u32>,
    pub new_no: Option<u32>,
    pub text: String,
}

/// Files above this size are not diffed line by line.
const MAX_DIFF_BYTES: usize = 2 * 1024 * 1024;

/// A repository handle that can be sent to background threads.
#[derive(Clone)]
pub struct Repo {
    inner: gix::ThreadSafeRepository,
}

impl Repo {
    /// Open the repository containing `path` (searching parent directories like `git` does).
    pub fn discover(path: &Path) -> Result<Repo> {
        Ok(Repo { inner: gix::ThreadSafeRepository::discover(path)? })
    }

    /// Working tree root, or the git dir for a bare repository.
    pub fn root(&self) -> PathBuf {
        let r = self.inner.to_thread_local();
        r.workdir().map(Path::to_path_buf).unwrap_or_else(|| r.git_dir().to_path_buf())
    }

    pub fn name(&self) -> String {
        let root = self.root();
        root.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| root.display().to_string())
    }

    /// Walk history from every branch, remote branch, tag and HEAD; at most `limit` commits.
    pub fn history(&self, limit: usize) -> Result<History> {
        let repo = self.inner.to_thread_local();
        let refs = refs(&repo)?;
        let head = repo.head_id().ok().map(|id| id.detach());
        let head_branch = repo.head_name()?.and_then(|n| n.category_and_short_name().filter(|(c, _)| *c == gix::refs::Category::LocalBranch).map(|(_, s)| s.to_str_lossy().into_owned()));
        let mut tips: Vec<ObjectId> = refs.iter().map(|r| r.target).chain(head).collect();
        tips.sort();
        tips.dedup();

        let mut commits = Vec::new();
        let mut truncated = false;
        if !tips.is_empty() {
            let walk = repo.rev_walk(tips).sorting(gix::revision::walk::Sorting::ByCommitTime(Default::default())).all()?;
            for info in walk {
                if commits.len() >= limit {
                    truncated = true;
                    break;
                }
                let info = info?;
                let c = repo.find_commit(info.id)?;
                let msg = c.message()?;
                let author = c.author()?;
                let (time, offset) = author.time().map(|t| (t.seconds, t.offset)).unwrap_or((0, 0));
                commits.push(Commit {
                    id: info.id,
                    summary: msg.summary().to_str_lossy().trim().to_string(),
                    author: author.name.to_str_lossy().trim().to_string(),
                    email: author.email.to_str_lossy().trim().to_string(),
                    time,
                    offset,
                    parents: info.parent_ids.iter().copied().collect(),
                });
            }
        }
        Ok(arrange(commits, refs, head, head_branch, truncated))
    }

    /// Full commit message.
    pub fn message(&self, id: ObjectId) -> Result<String> {
        let repo = self.inner.to_thread_local();
        Ok(repo.find_commit(id)?.message_raw()?.to_str_lossy().trim_end().to_string())
    }

    /// Files changed by `id` against its first parent (or against nothing for a root commit),
    /// with renames detected as `git` would by default.
    pub fn changes(&self, id: ObjectId) -> Result<Vec<FileChange>> {
        let repo = self.inner.to_thread_local();
        let commit = repo.find_commit(id)?;
        let new_tree = commit.tree()?;
        let old_tree = match commit.parent_ids().next() {
            Some(p) => repo.find_commit(p.detach())?.tree()?,
            None => repo.empty_tree(),
        };
        let mut out = Vec::new();
        for ch in repo.diff_tree_to_tree(Some(&old_tree), Some(&new_tree), None)? {
            use gix::object::tree::diff::ChangeDetached as C;
            let (path, status, mode, old, new) = match ch {
                C::Addition { location, entry_mode, id, .. } => (location, Status::Added, entry_mode, None, Some(id)),
                C::Deletion { location, entry_mode, id, .. } => (location, Status::Deleted, entry_mode, Some(id), None),
                C::Modification { location, entry_mode, previous_id, id, .. } => (location, Status::Modified, entry_mode, Some(previous_id), Some(id)),
                C::Rewrite { source_location, location, entry_mode, source_id, id, copy, .. } => {
                    let from = source_location.to_str_lossy().into_owned();
                    (location, if copy { Status::Copied { from } } else { Status::Renamed { from } }, entry_mode, Some(source_id), Some(id))
                }
            };
            if mode.is_tree() {
                continue;
            }
            let mut fc = FileChange { path: path.to_str_lossy().into_owned(), status, added: 0, removed: 0, binary: false, old, new };
            if mode.is_blob() || mode.is_link() {
                let (a, b) = (blob(&repo, old)?, blob(&repo, new)?);
                match (text(&a), text(&b)) {
                    (Some(a), Some(b)) => {
                        let d = similar::TextDiff::from_lines(a, b);
                        for op in d.ops() {
                            for change in d.iter_changes(op) {
                                match change.tag() {
                                    similar::ChangeTag::Insert => fc.added += 1,
                                    similar::ChangeTag::Delete => fc.removed += 1,
                                    similar::ChangeTag::Equal => {}
                                }
                            }
                        }
                    }
                    _ => fc.binary = true,
                }
            }
            out.push(fc);
        }
        out.sort_by(|a, b| a.path.cmp(&b.path));
        Ok(out)
    }

    /// Unified diff of one changed file, 3 lines of context. Empty for binary or huge files.
    pub fn diff(&self, change: &FileChange) -> Result<Vec<DiffLine>> {
        let repo = self.inner.to_thread_local();
        let (a, b) = (blob(&repo, change.old)?, blob(&repo, change.new)?);
        let (Some(a), Some(b)) = (text(&a), text(&b)) else { return Ok(Vec::new()) };
        Ok(unified(a, b))
    }
}

fn blob(repo: &gix::Repository, id: Option<ObjectId>) -> Result<Vec<u8>> {
    match id {
        Some(id) => Ok(repo.find_blob(id)?.detach().data),
        None => Ok(Vec::new()),
    }
}

/// UTF-8 text small enough to diff; `None` for binary or huge content.
fn text(bytes: &[u8]) -> Option<&str> {
    if bytes.len() > MAX_DIFF_BYTES || bytes[..bytes.len().min(8000)].contains(&0) {
        return None;
    }
    std::str::from_utf8(bytes).ok()
}

pub fn unified(a: &str, b: &str) -> Vec<DiffLine> {
    let d = similar::TextDiff::from_lines(a, b);
    let mut out = Vec::new();
    for group in d.grouped_ops(3) {
        let (first, last) = (&group[0], &group[group.len() - 1]);
        let (o, n) = (first.old_range().start, first.new_range().start);
        let (ol, nl) = (last.old_range().end - o, last.new_range().end - n);
        out.push(DiffLine { kind: LineKind::Hunk, old_no: None, new_no: None, text: format!("@@ -{},{} +{},{} @@", o + 1, ol, n + 1, nl) });
        for op in &group {
            for c in d.iter_changes(op) {
                let kind = match c.tag() {
                    similar::ChangeTag::Equal => LineKind::Context,
                    similar::ChangeTag::Insert => LineKind::Added,
                    similar::ChangeTag::Delete => LineKind::Removed,
                };
                out.push(DiffLine {
                    kind,
                    old_no: c.old_index().map(|i| i as u32 + 1),
                    new_no: c.new_index().map(|i| i as u32 + 1),
                    text: c.value().trim_end_matches(['\n', '\r']).to_string(),
                });
            }
        }
    }
    out
}

fn refs(repo: &gix::Repository) -> Result<Vec<RefInfo>> {
    let mut out = Vec::new();
    let platform = repo.references()?;
    for r in platform.all()? {
        let Ok(mut r) = r else { continue };
        let kind = match r.name().category_and_short_name() {
            Some((gix::refs::Category::LocalBranch, _)) => RefKind::Local,
            Some((gix::refs::Category::RemoteBranch, _)) => RefKind::Remote,
            Some((gix::refs::Category::Tag, _)) => RefKind::Tag,
            _ => continue,
        };
        let name = r.name().shorten().to_str_lossy().into_owned();
        // origin/HEAD is a pointer to another remote branch, not a branch of its own
        if kind == RefKind::Remote && name.ends_with("/HEAD") {
            continue;
        }
        // annotated tags peel to the commit they tag
        let Ok(id) = r.peel_to_id() else { continue };
        out.push(RefInfo { name, kind, target: id.detach() });
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then_with(|| a.name.cmp(&b.name)));
    Ok(out)
}

/// Topologically order the loaded commits and lay out the graph.
pub fn arrange(commits: Vec<Commit>, refs: Vec<RefInfo>, head: Option<ObjectId>, head_branch: Option<String>, truncated: bool) -> History {
    let by_id: HashMap<ObjectId, usize> = commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let nodes: Vec<graph::Node> = commits.iter().map(|c| graph::Node { parents: c.parents.iter().filter_map(|p| by_id.get(p).copied()).collect(), time: c.time }).collect();
    let order = graph::topo_order(&nodes);
    let mut slots: Vec<Option<Commit>> = commits.into_iter().map(Some).collect();
    let commits: Vec<Commit> = order.iter().map(|&i| slots[i].take().expect("each index once")).collect();
    let index: HashMap<ObjectId, usize> = commits.iter().enumerate().map(|(i, c)| (c.id, i)).collect();
    let parents: Vec<Vec<usize>> = commits.iter().map(|c| c.parents.iter().filter_map(|p| index.get(p).copied()).collect()).collect();
    let rows = graph::layout(&parents);
    History { commits, rows, refs, index, head, head_branch, truncated }
}
