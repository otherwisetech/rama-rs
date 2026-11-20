//! AI agents at work in worktrees, found read-only and offline: agent CLI processes whose
//! working directory is inside a worktree, plus Claude Code's own records: the session it runs
//! (`~/.claude/sessions/<pid>.json`: id, status, name) and its log
//! (`~/.claude/projects/<escaped cwd>/<id>.jsonl`). For the badge only the log's modification
//! time and its last entries' type, stop reason, pending tool name, API error kind, whether
//! the last answer ends in a question and the session title are read; no text is kept or
//! logged. (The conversation viewer, `transcript.rs`, reads the log when the user opens it.)
use std::collections::HashMap;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// A session log written to this recently means the agent is busy.
pub const FRESH_SECS: i64 = 20;
/// The agent's process tree using this much CPU (percent of one core) counts as working.
const CPU_BUSY: f32 = 5.;
/// A user turn this recent with no answer yet: the model is still thinking.
const USER_TURN_SECS: i64 = 120;
/// A tool call left without a result this long, with nothing running: a permission prompt.
const PERMISSION_SECS: i64 = 30;
/// Bytes read from the end of a session log.
const TAIL: u64 = 64 * 1024;
/// Polls a working agent must look quiet before it is shown as waiting / idle (no flicker).
const SETTLE_POLLS: u8 = 2;
/// How long an agent that vanished mid-work stays on the map as an error.
const GHOST_SECS: i64 = 10 * 60;

/// Badge states, least to most urgent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentState {
    Idle,
    /// Its turn ended; your move.
    Waiting,
    Working,
    Error,
    /// A question or a permission prompt is open.
    NeedsYou,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Need {
    Question,
    Permission,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrKind {
    RateLimit,
    Overloaded,
    Auth,
    Connection,
    Server,
    Request,
    /// The process went away while it was working.
    Exited,
    Other,
}

impl ErrKind {
    /// From the `error` field of an API error entry (`rate_limit`, `server_error`…).
    pub fn parse(s: &str) -> ErrKind {
        let s = s.to_ascii_lowercase();
        if s.contains("rate") {
            ErrKind::RateLimit
        } else if s.contains("overload") {
            ErrKind::Overloaded
        } else if s.contains("auth") || s.contains("permission_error") || s.contains("billing") {
            ErrKind::Auth
        } else if s.contains("connect") || s.contains("timeout") || s.contains("network") {
            ErrKind::Connection
        } else if s.contains("invalid") || s.contains("request") || s.contains("too_large") {
            ErrKind::Request
        } else if s.contains("server") || s.contains("api") {
            ErrKind::Server
        } else {
            ErrKind::Other
        }
    }
}

/// A process as the scan sees it; built from the OS or by hand in tests.
#[derive(Debug, Clone, Default)]
pub struct Proc {
    pub pid: u32,
    pub ppid: Option<u32>,
    pub name: String,
    pub cmd: Vec<String>,
    pub exe: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
    /// Percent of one core.
    pub cpu: f32,
    /// Seconds since the epoch.
    pub start: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Agent {
    /// CLI name (`claude`, `codex`…).
    pub kind: &'static str,
    pub pid: Option<u32>,
    pub state: AgentState,
    pub need: Option<Need>,
    pub error: Option<ErrKind>,
    /// When `state` began (filled in by [`Tracker`]).
    pub since: i64,
    pub started: Option<i64>,
    pub last_activity: Option<i64>,
    pub title: Option<String>,
    pub session_id: Option<String>,
    /// The session log, for the conversation viewer.
    pub session_file: Option<PathBuf>,
    /// Processes serving this session (wrappers, several terminals on one session).
    pub procs: usize,
    /// macOS app hosting the agent's terminal (`/Applications/Ghostty.app`), to bring forward.
    pub host_app: Option<PathBuf>,
}

impl Agent {
    fn new(kind: &'static str, now: i64) -> Agent {
        Agent { kind, pid: None, state: AgentState::Idle, need: None, error: None, since: now, started: None, last_activity: None, title: None, session_id: None, session_file: None, procs: 1, host_app: None }
    }

    /// One entry per session: by session id, else log file, else process.
    pub fn group_key(&self) -> String {
        match (&self.session_id, &self.session_file, self.pid) {
            (Some(id), _, _) => format!("s:{id}"),
            (None, Some(f), _) => format!("f:{}", f.display()),
            (None, None, Some(pid)) => format!("p:{pid}"),
            _ => format!("k:{}", self.kind),
        }
    }
}

/// Agents per worktree (keys as given to [`scan`]).
pub type Found = HashMap<PathBuf, Vec<Agent>>;

/// CLI name, and package paths that identify it when run by an interpreter.
const KNOWN: &[(&str, &[&str])] = &[
    ("claude", &["@anthropic-ai/claude-code", "/claude/versions/"]),
    ("codex", &["@openai/codex"]),
    ("opencode", &["opencode-ai", "/opencode/"]),
    ("pi", &["@mariozechner/pi-coding-agent"]),
    ("gemini", &["@google/gemini-cli"]),
    ("aider", &["aider-chat", "/aider/"]),
];

fn interpreter(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["node", "bun", "deno"].contains(&n.as_str()) || n.starts_with("python")
}

fn stem(s: &str) -> &str {
    let f = s.rsplit(['/', '\\']).next().unwrap_or(s);
    f.strip_suffix(".exe").or_else(|| f.strip_suffix(".cmd")).or_else(|| f.strip_suffix(".js")).unwrap_or(f)
}

/// Which agent CLI a process is, from its name, executable or arguments.
pub fn agent_kind(p: &Proc) -> Option<&'static str> {
    let by_name = |s: &str| KNOWN.iter().find(|(k, _)| *k == stem(s)).map(|(k, _)| *k);
    let by_path = |s: &str| KNOWN.iter().find(|(_, pats)| pats.iter().any(|pat| s.contains(pat))).map(|(k, _)| *k);
    if let Some(k) = by_name(&p.name).or_else(|| p.cmd.first().and_then(|a| by_name(a))) {
        return Some(k);
    }
    if let Some(k) = p.exe.as_ref().and_then(|e| by_path(&e.to_string_lossy())) {
        return Some(k);
    }
    if interpreter(&p.name) || p.cmd.first().is_some_and(|a| interpreter(stem(a))) {
        // the script is the first argument that isn't a flag
        let script = p.cmd.iter().skip(1).find(|a| !a.starts_with('-'))?;
        return by_name(script).or_else(|| by_path(script));
    }
    None
}

/// The innermost worktree holding `dir`, so `.claude/worktrees/x` inside the main checkout
/// belongs to `x` alone.
pub fn deepest(dir: &Path, worktrees: &[PathBuf]) -> Option<usize> {
    worktrees.iter().enumerate().filter(|(_, w)| dir.starts_with(w)).max_by_key(|(_, w)| w.components().count()).map(|(i, _)| i)
}

/// Claude Code's folder name for a working directory: every character that isn't an ASCII
/// letter or digit becomes `-` (`/Users/ana/.x` → `-Users-ana--x`).
pub fn claude_dir_name(cwd: &Path) -> String {
    cwd.to_string_lossy().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Last {
    /// The latest turn is the model's: `tool` names a tool call still without a result,
    /// `question` when its last text ends in "?", `error` for an API error entry.
    Assistant { tool: Option<String>, question: bool, error: Option<ErrKind> },
    User,
}

/// Claude Code's own status for a running session (`~/.claude/sessions/<pid>.json`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Busy,
    /// Blocked on the user: a permission prompt or a question.
    Waiting,
    Idle,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub id: Option<String>,
    pub file: PathBuf,
    pub modified: i64,
    pub last: Option<Last>,
    pub title: Option<String>,
    pub status: Option<Status>,
}

pub(crate) fn secs(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

struct PidRecord {
    id: Option<String>,
    status: Option<Status>,
    name: Option<String>,
    cwd: Option<PathBuf>,
}

fn pid_record(home: &Path, pid: u32) -> Option<PidRecord> {
    let text = std::fs::read_to_string(home.join(".claude/sessions").join(format!("{pid}.json"))).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let s = |k: &str| v.get(k).and_then(|x| x.as_str()).map(str::to_string);
    let status = s("status").map(|st| match st.as_str() {
        "busy" => Status::Busy,
        "waiting" => Status::Waiting,
        "idle" => Status::Idle,
        _ => Status::Other,
    });
    Some(PidRecord { id: s("sessionId"), status, name: s("name").filter(|n| !n.trim().is_empty()), cwd: s("cwd").map(PathBuf::from) })
}

/// The session a Claude process runs: from its pid record when there is one, else the newest
/// log for its working directory.
pub fn claude_session(home: &Path, pid: Option<u32>, cwd: &Path) -> Option<Session> {
    let rec = pid.and_then(|p| pid_record(home, p));
    let dir_cwd = rec.as_ref().and_then(|r| r.cwd.clone()).unwrap_or_else(|| cwd.to_path_buf());
    let dir = home.join(".claude/projects").join(claude_dir_name(&dir_cwd));
    let exact = rec.as_ref().and_then(|r| r.id.as_ref()).map(|id| dir.join(format!("{id}.jsonl"))).filter(|f| f.is_file());
    let file = match exact {
        Some(f) => f,
        None => std::fs::read_dir(&dir).ok()?.flatten().filter(|e| e.path().extension().is_some_and(|x| x == "jsonl")).filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path()))).max_by_key(|(m, _)| *m)?.1,
    };
    let modified = std::fs::metadata(&file).and_then(|m| m.modified()).map(secs).unwrap_or(0);
    let (last, title) = read_tail(&file);
    let id = rec.as_ref().and_then(|r| r.id.clone()).or_else(|| file.file_stem().map(|s| s.to_string_lossy().into_owned()));
    Some(Session { id, modified, last, title: title.or_else(|| rec.as_ref().and_then(|r| r.name.clone())), status: rec.and_then(|r| r.status), file })
}

/// The end of a log, as complete lines (a line cut by the window is dropped).
pub(crate) fn tail_lines(path: &Path, window: u64) -> Option<(String, u64, u64)> {
    let mut f = std::fs::File::open(path).ok()?;
    let len = f.metadata().ok()?.len();
    let from = len.saturating_sub(window);
    f.seek(SeekFrom::Start(from)).ok()?;
    let mut buf = Vec::new();
    f.take(window).read_to_end(&mut buf).ok()?;
    let mut text = String::from_utf8_lossy(&buf).into_owned();
    if from > 0 {
        let cut = text.find('\n').map(|i| i + 1).unwrap_or(text.len());
        text.drain(..cut);
    }
    Some((text, from, len))
}

/// Subagents' entries and Claude Code's own bookkeeping don't count as turns.
pub(crate) fn turn_entry(v: &serde_json::Value) -> Option<&str> {
    let ty = v.get("type")?.as_str()?;
    let flag = |k: &str| v.get(k).and_then(|x| x.as_bool()).unwrap_or(false);
    ((ty == "user" || ty == "assistant") && !flag("isSidechain") && !flag("isMeta")).then_some(ty)
}

fn blocks(v: &serde_json::Value) -> &[serde_json::Value] {
    v.pointer("/message/content").and_then(|c| c.as_array()).map(Vec::as_slice).unwrap_or(&[])
}

/// The latest turn and session title in the end of a log.
fn read_tail(path: &Path) -> (Option<Last>, Option<String>) {
    match tail_lines(path, TAIL) {
        Some((text, _, _)) => parse_tail(&text),
        None => (None, None),
    }
}

pub fn parse_tail(text: &str) -> (Option<Last>, Option<String>) {
    let (mut last, mut title) = (None, None);
    // going back through the model's latest run of entries (thinking, text, tool calls)
    let mut run: Option<(Option<String>, Option<bool>, Option<ErrKind>)> = None;
    for line in text.lines().rev().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        if title.is_none() {
            title = match ty {
                "custom-title" => v.get("customTitle"),
                "ai-title" => v.get("aiTitle"),
                "summary" => v.get("summary"),
                _ => None,
            }
            .and_then(|s| s.as_str())
            .map(str::to_string);
        }
        if last.is_none() {
            match turn_entry(&v) {
                Some("user") => {
                    last = Some(match run.take() {
                        Some((tool, q, error)) => Last::Assistant { tool, question: q.unwrap_or(false), error },
                        None => Last::User,
                    })
                }
                Some(_) => {
                    let first = run.is_none();
                    let r = run.get_or_insert((None, None, None));
                    if first {
                        r.0 = blocks(&v).iter().rev().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("tool_use")).and_then(|b| b.get("name")?.as_str()).map(str::to_string);
                        if v.get("isApiErrorMessage").and_then(|x| x.as_bool()).unwrap_or(false) || v.get("error").is_some_and(|e| e.is_string()) {
                            r.2 = Some(ErrKind::parse(v.get("error").and_then(|e| e.as_str()).unwrap_or_default()));
                        }
                    }
                    if r.1.is_none()
                        && let Some(text) = blocks(&v).iter().rev().find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text")).and_then(|b| b.get("text")?.as_str())
                    {
                        r.1 = Some(text.trim_end().ends_with('?'));
                    }
                }
                None => {}
            }
        }
        if last.is_some() && title.is_some() {
            break;
        }
    }
    if last.is_none()
        && let Some((tool, q, error)) = run
    {
        last = Some(Last::Assistant { tool, question: q.unwrap_or(false), error });
    }
    (last, title.filter(|t| !t.trim().is_empty()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Verdict {
    pub state: AgentState,
    pub need: Option<Need>,
    pub error: Option<ErrKind>,
}

/// One poll's verdict from what can be observed.
pub fn classify(now: i64, session: Option<&Session>, tree_cpu: Option<f32>) -> Verdict {
    let v = |state| Verdict { state, need: None, error: None };
    let need = |n| Verdict { state: AgentState::NeedsYou, need: Some(n), error: None };
    let age = session.map(|s| now - s.modified);
    let fresh = age.is_some_and(|a| a < FRESH_SECS);
    let status = session.and_then(|s| s.status);
    let last = session.and_then(|s| s.last.as_ref());
    let (tool, question, error) = match last {
        Some(Last::Assistant { tool, question, error }) => (tool.as_deref(), *question, *error),
        _ => (None, false, None),
    };
    let asks = tool == Some("AskUserQuestion");
    let assistant = matches!(last, Some(Last::Assistant { .. }));
    match status {
        Some(Status::Waiting) => return need(if asks { Need::Question } else { Need::Permission }),
        Some(Status::Busy) => return v(AgentState::Working),
        _ => {}
    }
    if let Some(k) = error.filter(|_| !fresh) {
        return Verdict { state: AgentState::Error, need: None, error: Some(k) };
    }
    if asks {
        return need(Need::Question);
    }
    if status == Some(Status::Idle) {
        return if question && tool.is_none() {
            need(Need::Question)
        } else if assistant {
            v(AgentState::Waiting)
        } else {
            v(AgentState::Idle)
        };
    }
    if fresh || tree_cpu.is_some_and(|c| c >= CPU_BUSY) {
        return v(AgentState::Working);
    }
    if tool.is_some() && age.is_some_and(|a| a >= PERMISSION_SECS) {
        return need(Need::Permission);
    }
    if question && tool.is_none() {
        return need(Need::Question);
    }
    match last {
        Some(Last::Assistant { .. }) => v(AgentState::Waiting),
        Some(Last::User) if age.is_some_and(|a| a < USER_TURN_SECS) => v(AgentState::Working),
        _ => v(AgentState::Idle),
    }
}

/// Find the agents working in `worktrees` (canonical paths) among `procs`, one entry per
/// session. A session log changed in the last few seconds counts even without a visible
/// process (another user's process, or a platform where cwds can't be read).
pub fn scan(home: &Path, worktrees: &[PathBuf], procs: &[Proc], now: i64) -> Found {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for p in procs {
        if let Some(pp) = p.ppid {
            children.entry(pp).or_default().push(p.pid);
        }
    }
    let kinds: HashMap<u32, &'static str> = procs.iter().filter_map(|p| Some((p.pid, agent_kind(p)?))).collect();
    let mut found = Found::new();
    let mut seen_claude_dirs = Vec::new();
    for p in procs {
        let Some(&kind) = kinds.get(&p.pid) else { continue };
        // a wrapper's child (node → the real binary) is the same agent
        if ancestors(p, &by_pid).any(|a| kinds.get(&a.pid) == Some(&kind)) {
            continue;
        }
        let Some(cwd) = p.cwd.as_ref() else { continue };
        let Some(w) = deepest(cwd, worktrees) else { continue };
        let session = if kind == "claude" {
            seen_claude_dirs.push(claude_dir_name(cwd));
            claude_session(home, Some(p.pid), cwd)
        } else {
            None
        };
        let verdict = classify(now, session.as_ref(), Some(tree_cpu(p.pid, &by_pid, &children)));
        let mut a = Agent::new(kind, now);
        a.pid = Some(p.pid);
        a.state = verdict.state;
        a.need = verdict.need;
        a.error = verdict.error;
        a.started = Some(p.start).filter(|s| *s > 0);
        a.host_app = ancestors(p, &by_pid).find_map(|a| a.exe.as_deref().and_then(app_bundle));
        if let Some(s) = session {
            a.last_activity = Some(s.modified);
            a.title = s.title;
            a.session_id = s.id;
            a.session_file = Some(s.file);
        }
        found.entry(worktrees[w].clone()).or_default().push(a);
    }
    for w in worktrees {
        if found.contains_key(w) || seen_claude_dirs.contains(&claude_dir_name(w)) {
            continue;
        }
        if let Some(s) = claude_session(home, None, w).filter(|s| now - s.modified < FRESH_SECS) {
            let mut a = Agent::new("claude", now);
            a.state = AgentState::Working;
            a.last_activity = Some(s.modified);
            a.title = s.title;
            a.session_id = s.id;
            a.session_file = Some(s.file);
            found.entry(w.clone()).or_default().push(a);
        }
    }
    for agents in found.values_mut() {
        *agents = dedupe(std::mem::take(agents));
    }
    found
}

/// One entry per session, the most urgent state winning.
pub fn dedupe(agents: Vec<Agent>) -> Vec<Agent> {
    let mut out: Vec<Agent> = Vec::new();
    for a in agents {
        match out.iter_mut().find(|o| o.group_key() == a.group_key()) {
            Some(o) => {
                let procs = o.procs + a.procs;
                let started = o.started.into_iter().chain(a.started).min();
                let last = o.last_activity.max(a.last_activity);
                let host = o.host_app.clone().or_else(|| a.host_app.clone());
                if a.state > o.state {
                    *o = a;
                }
                o.procs = procs;
                o.started = started;
                o.last_activity = last;
                o.host_app = host;
            }
            None => out.push(a),
        }
    }
    out
}

fn ancestors<'a>(p: &Proc, by_pid: &'a HashMap<u32, &'a Proc>) -> impl Iterator<Item = &'a Proc> {
    let mut cur = p.ppid;
    let mut steps = 0;
    std::iter::from_fn(move || {
        steps += 1;
        let q = by_pid.get(&cur?).copied().filter(|_| steps < 64)?;
        cur = q.ppid.filter(|pp| *pp != q.pid);
        Some(q)
    })
}

/// CPU of a process and everything it started (tools, builds).
fn tree_cpu(pid: u32, by_pid: &HashMap<u32, &Proc>, children: &HashMap<u32, Vec<u32>>) -> f32 {
    let mut todo = vec![pid];
    let mut total = 0.;
    let mut n = 0;
    while let Some(p) = todo.pop() {
        n += 1;
        if n > 4096 {
            break;
        }
        total += by_pid.get(&p).map(|p| p.cpu).unwrap_or(0.);
        todo.extend(children.get(&p).into_iter().flatten().copied());
    }
    total
}

/// `/Applications/Ghostty.app/Contents/MacOS/ghostty` → `/Applications/Ghostty.app`.
pub fn app_bundle(exe: &Path) -> Option<PathBuf> {
    let s = exe.to_string_lossy();
    let i = s.find(".app/Contents/")?;
    Some(PathBuf::from(&s[..i + 4]))
}

type TrackKey = (PathBuf, String);

#[derive(Debug, Clone)]
struct Seen {
    agent: Agent,
    quiet: u8,
}

/// Remembers each agent between polls: when its state began, a short grace period before a
/// working agent is shown as waiting or idle, and agents that vanished mid-work (shown as an
/// error for a while).
#[derive(Debug, Default)]
pub struct Tracker {
    prev: HashMap<TrackKey, Seen>,
    ghosts: HashMap<TrackKey, (Agent, i64)>,
}

impl Tracker {
    /// Settle `found` in place; returns the agents that just started needing the user.
    pub fn apply(&mut self, found: &mut Found, now: i64) -> Vec<(PathBuf, Agent)> {
        let mut next = HashMap::new();
        let mut asks = Vec::new();
        for (w, agents) in found.iter_mut() {
            for a in agents.iter_mut() {
                let key = (w.clone(), a.group_key());
                let quiet = match self.prev.get(&key) {
                    Some(p) if p.agent.state == AgentState::Working && !matches!(a.state, AgentState::Working | AgentState::NeedsYou | AgentState::Error) && p.quiet + 1 < SETTLE_POLLS => {
                        a.state = AgentState::Working;
                        a.need = None;
                        a.error = None;
                        a.since = p.agent.since;
                        p.quiet + 1
                    }
                    Some(p) => {
                        if p.agent.state == a.state {
                            a.since = p.agent.since;
                        } else if a.state == AgentState::NeedsYou {
                            asks.push((w.clone(), a.clone()));
                        }
                        0
                    }
                    None => 0,
                };
                self.ghosts.remove(&key);
                next.insert(key, Seen { agent: a.clone(), quiet });
            }
        }
        let alive: Vec<u32> = next.values().filter_map(|s: &Seen| s.agent.pid).collect();
        for (key, seen) in &self.prev {
            // a live process that moved to another session (`/clear`) didn't exit
            if !next.contains_key(key) && seen.agent.state == AgentState::Working && seen.agent.pid.is_some_and(|p| !alive.contains(&p)) {
                let mut g = seen.agent.clone();
                g.state = AgentState::Error;
                g.error = Some(ErrKind::Exited);
                g.need = None;
                g.since = now;
                g.pid = None;
                self.ghosts.insert(key.clone(), (g, now + GHOST_SECS));
            }
        }
        self.ghosts.retain(|_, (_, until)| *until > now);
        for ((w, _), (g, _)) in &self.ghosts {
            found.entry(w.clone()).or_default().push(g.clone());
        }
        for agents in found.values_mut() {
            agents.sort_by_key(|a| (std::cmp::Reverse(a.state), a.kind, a.started));
        }
        self.prev = next;
        asks
    }
}

/// The badge's state for a worktree: its most urgent agent.
pub fn top(agents: &[Agent]) -> Option<&Agent> {
    agents.iter().max_by_key(|a| a.state)
}

#[cfg(not(windows))]
fn canonical(p: &Path) -> PathBuf {
    std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
}

#[cfg(windows)]
fn canonical(p: &Path) -> PathBuf {
    p.to_path_buf()
}

/// Scan the real system for agents in `worktrees`; results are keyed by the paths given.
pub fn detect(worktrees: &[PathBuf]) -> Found {
    let Some(home) = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from) else { return Found::new() };
    let canon: Vec<PathBuf> = worktrees.iter().map(PathBuf::as_path).map(canonical).collect();
    let procs = system::processes();
    let found = scan(&home, &canon, &procs, secs(SystemTime::now()));
    found.into_iter().filter_map(|(k, v)| Some((worktrees[canon.iter().position(|c| *c == k)?].clone(), v))).collect()
}

mod system {
    use super::{Proc, agent_kind, interpreter};
    use std::sync::Mutex;
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

    // kept between polls: CPU usage is measured from one refresh to the next
    static SYS: Mutex<Option<System>> = Mutex::new(None);

    pub fn processes() -> Vec<Proc> {
        let Ok(mut guard) = SYS.lock() else { return Vec::new() };
        let sys = guard.get_or_insert_with(System::new);
        sys.refresh_processes_specifics(ProcessesToUpdate::All, ProcessRefreshKind::new().with_cpu());
        let quick = |p: &sysinfo::Process| Proc { pid: p.pid().as_u32(), ppid: parent(p).map(|x| x.as_u32()), name: p.name().to_string_lossy().into_owned(), cmd: Vec::new(), exe: None, cwd: None, cpu: p.cpu_usage(), start: p.start_time() as i64 };
        // only plausible agents get their arguments and cwd read
        let candidates: Vec<Pid> = sys.processes().values().filter(|p| {
            let q = quick(p);
            agent_kind(&q).is_some() || interpreter(&q.name) || q.name.chars().all(|c| c.is_ascii_digit() || c == '.')
        }).map(|p| p.pid()).collect();
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&candidates), ProcessRefreshKind::new().with_cpu().with_cmd(UpdateKind::OnlyIfNotSet).with_exe(UpdateKind::OnlyIfNotSet).with_cwd(UpdateKind::Always));
        let full = |p: &sysinfo::Process| Proc { cmd: p.cmd().iter().map(|a| a.to_string_lossy().into_owned()).collect(), exe: p.exe().map(|e| e.to_path_buf()), cwd: p.cwd().map(|c| c.to_path_buf()), ..quick(p) };
        let agents: Vec<Pid> = sys.processes().values().filter(|p| agent_kind(&full(p)).is_some()).map(|p| p.pid()).collect();
        // the terminal app an agent runs in, for "bring it forward"
        let mut hosts = Vec::new();
        for pid in &agents {
            let mut cur = sys.process(*pid).and_then(parent);
            while let Some(pp) = cur.filter(|_| hosts.len() < 4096) {
                hosts.push(pp);
                cur = sys.process(pp).and_then(parent).filter(|x| *x != pp);
            }
        }
        sys.refresh_processes_specifics(ProcessesToUpdate::Some(&hosts), ProcessRefreshKind::new().with_cpu().with_exe(UpdateKind::OnlyIfNotSet));
        sys.processes().values().map(full).collect()
    }

    /// sysinfo can't read root-owned processes (`/usr/bin/login` between a terminal and its
    /// shell); the short BSD info still gives their parent.
    fn parent(p: &sysinfo::Process) -> Option<Pid> {
        p.parent().or_else(|| bsd_parent(p.pid().as_u32()).map(Pid::from_u32))
    }

    #[cfg(target_os = "macos")]
    fn bsd_parent(pid: u32) -> Option<u32> {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdshortinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdshortinfo>() as libc::c_int;
        // SAFETY: the buffer is exactly the size passed; the kernel fills at most that much
        let n = unsafe { libc::proc_pidinfo(pid as libc::c_int, libc::PROC_PIDT_SHORTBSDINFO, 0, info.as_mut_ptr().cast(), size) };
        if n != size {
            return None;
        }
        // SAFETY: fully written, checked above
        let ppid = unsafe { info.assume_init() }.pbsi_ppid;
        (ppid > 0 && ppid != pid).then_some(ppid)
    }

    #[cfg(not(target_os = "macos"))]
    fn bsd_parent(_pid: u32) -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proc(pid: u32, ppid: u32, name: &str, cmd: &[&str], cwd: Option<&str>) -> Proc {
        Proc { pid, ppid: Some(ppid), name: name.into(), cmd: cmd.iter().map(|s| s.to_string()).collect(), cwd: cwd.map(PathBuf::from), start: 1000, ..Default::default() }
    }

    fn temp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("rama-agents-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::canonicalize(d).unwrap()
    }

    fn write_session(home: &Path, cwd: &Path, name: &str, lines: &[&str], age: u64) -> PathBuf {
        let dir = home.join(".claude/projects").join(claude_dir_name(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join(format!("{name}.jsonl"));
        std::fs::write(&f, lines.join("\n") + "\n").unwrap();
        let t = SystemTime::now() - std::time::Duration::from_secs(age);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        f
    }

    fn write_pid(home: &Path, pid: u32, id: &str, status: &str, cwd: &Path) {
        let d = home.join(".claude/sessions");
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join(format!("{pid}.json")), format!(r#"{{"pid":{pid},"sessionId":"{id}","status":"{status}","cwd":{},"name":"pid-name"}}"#, serde_json::Value::String(cwd.display().to_string()))).unwrap();
    }

    const ASSISTANT_DONE: &str = r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Done."}]}}"#;
    const ASSISTANT_ASKS: &str = r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"Should I also update the docs?\n"}]}}"#;
    const THINKING: &str = r#"{"type":"assistant","message":{"content":[{"type":"thinking","thinking":"…"}]}}"#;
    const BASH: &str = r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"rm -rf build"}}]}}"#;
    const ASK_TOOL: &str = r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"t2","name":"AskUserQuestion","input":{"questions":[]}}]}}"#;
    const RESULT: &str = r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
    const API_ERROR: &str = r#"{"type":"assistant","isApiErrorMessage":true,"error":"rate_limit","message":{"content":[{"type":"text","text":"API Error: 429"}]}}"#;
    const USER: &str = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#;
    const SIDECHAIN: &str = r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"sub"}}"#;

    fn tail(lines: &[&str]) -> (Option<Last>, Option<String>) {
        parse_tail(&lines.join("\n"))
    }

    #[test]
    fn kinds() {
        assert_eq!(agent_kind(&proc(1, 0, "claude", &["claude"], None)), Some("claude"));
        // native install: the binary is named after its version
        let mut p = proc(1, 0, "2.1.328", &["claude", "--resume"], None);
        assert_eq!(agent_kind(&p), Some("claude"));
        p.cmd.clear();
        p.exe = Some("/Users/a/.local/share/claude/versions/2.1.328".into());
        assert_eq!(agent_kind(&p), Some("claude"));
        assert_eq!(agent_kind(&proc(1, 0, "node", &["node", "--no-warnings", "/opt/homebrew/bin/codex"], None)), Some("codex"));
        assert_eq!(agent_kind(&proc(1, 0, "node", &["node", "/usr/lib/node_modules/@google/gemini-cli/dist/index.js"], None)), Some("gemini"));
        assert_eq!(agent_kind(&proc(1, 0, "python3.12", &["/usr/bin/python3.12", "/home/a/.local/bin/aider"], None)), Some("aider"));
        assert_eq!(agent_kind(&proc(1, 0, "pi", &["pi"], None)), Some("pi"));
        assert_eq!(agent_kind(&proc(1, 0, "node", &["node", "server.js"], None)), None);
        assert_eq!(agent_kind(&proc(1, 0, "Claude", &["/Applications/Claude.app/Contents/MacOS/Claude"], None)), None);
        assert_eq!(agent_kind(&proc(1, 0, "cargo", &["cargo", "build"], None)), None);
    }

    #[test]
    fn nested_worktrees() {
        let wts = [PathBuf::from("/r"), PathBuf::from("/r/.claude/worktrees/x"), PathBuf::from("/r-feature")];
        assert_eq!(deepest(Path::new("/r/.claude/worktrees/x/src"), &wts), Some(1));
        assert_eq!(deepest(Path::new("/r/.claude/worktrees/x"), &wts), Some(1));
        assert_eq!(deepest(Path::new("/r/.claude/worktrees"), &wts), Some(0));
        assert_eq!(deepest(Path::new("/r/crates"), &wts), Some(0));
        // a sibling with a common prefix is not inside
        assert_eq!(deepest(Path::new("/r-feature/a"), &wts), Some(2));
        assert_eq!(deepest(Path::new("/rx"), &wts), None);
        assert_eq!(claude_dir_name(Path::new("/Users/ana/.claude/worktrees/my_wt")), "-Users-ana--claude-worktrees-my-wt");
        assert_eq!(app_bundle(Path::new("/Applications/Ghostty.app/Contents/MacOS/ghostty")), Some(PathBuf::from("/Applications/Ghostty.app")));
        assert_eq!(app_bundle(Path::new("/bin/zsh")), None);
    }

    #[test]
    fn tails() {
        assert_eq!(tail(&[USER, ASSISTANT_DONE]).0, Some(Last::Assistant { tool: None, question: false, error: None }));
        // the question is in an earlier entry of the same turn than the latest thinking
        assert_eq!(tail(&[USER, ASSISTANT_ASKS, THINKING]).0, Some(Last::Assistant { tool: None, question: true, error: None }));
        assert_eq!(tail(&[USER, THINKING, BASH]).0, Some(Last::Assistant { tool: Some("Bash".into()), question: false, error: None }));
        assert_eq!(tail(&[USER, BASH, RESULT]).0, Some(Last::User));
        assert_eq!(tail(&[USER, ASK_TOOL]).0, Some(Last::Assistant { tool: Some("AskUserQuestion".into()), question: false, error: None }));
        assert_eq!(tail(&[USER, API_ERROR]).0, Some(Last::Assistant { tool: None, question: false, error: Some(ErrKind::RateLimit) }));
        // subagents and bookkeeping are skipped
        assert_eq!(tail(&[USER, ASSISTANT_DONE, SIDECHAIN, r#"{"type":"last-prompt","lastPrompt":"x"}"#]).0, Some(Last::Assistant { tool: None, question: false, error: None }));
        // only the model's run counts: an answer before the user's newer message doesn't
        assert_eq!(tail(&[ASSISTANT_ASKS, USER]).0, Some(Last::User));
        assert_eq!(tail(&[ASSISTANT_DONE, r#"{"type":"ai-title","aiTitle":"Fix parser"}"#]).1.as_deref(), Some("Fix parser"));
        assert_eq!(ErrKind::parse("overloaded_error"), ErrKind::Overloaded);
        assert_eq!(ErrKind::parse("server_error"), ErrKind::Server);
        assert_eq!(ErrKind::parse("authentication_failed"), ErrKind::Auth);
        assert_eq!(ErrKind::parse("Connection error"), ErrKind::Connection);
    }

    #[test]
    fn states() {
        let now = 10_000;
        let s = |age: i64, lines: &[&str], status: Option<Status>| Session { id: None, file: PathBuf::new(), modified: now - age, last: tail(lines).0, title: None, status };
        let st = |sess: Session, cpu: f32| {
            let v = classify(now, Some(&sess), Some(cpu));
            (v.state, v.need, v.error)
        };
        use AgentState::*;
        assert_eq!(st(s(3, &[USER], None), 0.), (Working, None, None));
        assert_eq!(st(s(300, &[USER, ASSISTANT_DONE], None), 0.), (Waiting, None, None));
        // the turn ended on a question
        assert_eq!(st(s(300, &[USER, ASSISTANT_ASKS], None), 0.), (NeedsYou, Some(Need::Question), None));
        assert_eq!(st(s(300, &[USER, ASSISTANT_ASKS], Some(Status::Idle)), 0.), (NeedsYou, Some(Need::Question), None));
        // the question tool, even right after it was written
        assert_eq!(st(s(2, &[USER, ASK_TOOL], None), 0.), (NeedsYou, Some(Need::Question), None));
        // a tool call left without a result and nothing running: a permission prompt
        assert_eq!(st(s(300, &[USER, BASH], None), 0.), (NeedsYou, Some(Need::Permission), None));
        // ... unless something under the agent is busy (a long build)
        assert_eq!(st(s(300, &[USER, BASH], None), 80.), (Working, None, None));
        // Claude Code's own status wins
        assert_eq!(st(s(300, &[USER, BASH], Some(Status::Waiting)), 80.), (NeedsYou, Some(Need::Permission), None));
        assert_eq!(st(s(300, &[USER, ASSISTANT_DONE], Some(Status::Busy)), 0.), (Working, None, None));
        assert_eq!(st(s(300, &[USER, ASSISTANT_DONE], Some(Status::Idle)), 50.), (Waiting, None, None));
        // API errors, once the log has gone quiet (a fresh one is being retried)
        assert_eq!(st(s(300, &[USER, API_ERROR], None), 0.), (Error, None, Some(ErrKind::RateLimit)));
        assert_eq!(st(s(300, &[USER, API_ERROR], Some(Status::Idle)), 0.), (Error, None, Some(ErrKind::RateLimit)));
        assert_eq!(st(s(5, &[USER, API_ERROR], None), 0.), (Working, None, None));
        assert_eq!(st(s(60, &[USER], None), 0.), (Working, None, None));
        assert_eq!(st(s(600, &[USER], None), 0.), (Idle, None, None));
        assert_eq!(classify(now, None, Some(0.)).state, Idle);
        assert_eq!(classify(now, None, Some(30.)).state, Working);
        assert!(NeedsYou > Error && Error > Working && Working > Waiting && Waiting > Idle);
    }

    fn agent(pid: u32, state: AgentState, since: i64) -> Agent {
        Agent { pid: Some(pid), state, since, session_id: Some(format!("s{pid}")), ..Agent::new("claude", since) }
    }

    #[test]
    fn tracker() {
        let w = PathBuf::from("/r");
        let mut tr = Tracker::default();
        let mut poll = |agents: Vec<Agent>, now| {
            let mut f = Found::from([(w.clone(), agents)]);
            let asks = tr.apply(&mut f, now);
            (f.remove(&w).unwrap_or_default(), asks.len())
        };
        let one = |r: (Vec<Agent>, usize)| (r.0[0].state, r.0[0].since, r.1);
        use AgentState::*;
        assert_eq!(one(poll(vec![agent(7, Working, 0)], 0)), (Working, 0, 0));
        assert_eq!(one(poll(vec![agent(7, Working, 3)], 3)), (Working, 0, 0));
        // one quiet poll is not enough to stop working
        assert_eq!(one(poll(vec![agent(7, Waiting, 6)], 6)), (Working, 0, 0));
        assert_eq!(one(poll(vec![agent(7, Waiting, 9)], 9)), (Waiting, 9, 0));
        assert_eq!(one(poll(vec![agent(7, Waiting, 12)], 12)), (Waiting, 9, 0));
        // needing the user shows at once, and is reported once
        assert_eq!(one(poll(vec![agent(7, NeedsYou, 15)], 15)), (NeedsYou, 15, 1));
        assert_eq!(one(poll(vec![agent(7, NeedsYou, 18)], 18)), (NeedsYou, 15, 0));
        assert_eq!(one(poll(vec![agent(7, Working, 21)], 21)), (Working, 21, 0));
        // the process vanished while working: an error for a while
        let (gone, _) = poll(vec![], 24);
        assert_eq!((gone.len(), gone[0].state, gone[0].error), (1, Error, Some(ErrKind::Exited)));
        assert_eq!(poll(vec![], 24 + 300).0.len(), 1);
        assert!(poll(vec![], 24 + GHOST_SECS + 1).0.is_empty());
        // an idle agent that quits is just gone; urgent ones sort first
        poll(vec![agent(8, Idle, 0), agent(9, Waiting, 0)], 2000);
        assert!(poll(vec![agent(9, Waiting, 0)], 2003).0.iter().all(|a| a.pid == Some(9)));
        let (l, _) = poll(vec![agent(9, Waiting, 0), agent(10, NeedsYou, 0)], 2006);
        assert_eq!(l[0].pid, Some(10));
    }

    #[test]
    fn dedupes_by_session() {
        use AgentState::Idle;
        let mut a = agent(1, Idle, 0);
        a.session_id = Some("one".into());
        a.started = Some(50);
        let mut b = agent(2, AgentState::Working, 0);
        b.session_id = Some("one".into());
        b.started = Some(40);
        let mut c = agent(3, Idle, 0);
        c.session_id = Some("one".into());
        let d = agent(4, Idle, 0);
        let out = dedupe(vec![a, b, c, d]);
        assert_eq!(out.len(), 2);
        assert_eq!((out[0].procs, out[0].state, out[0].started), (3, AgentState::Working, Some(40)));
        assert_eq!(out[1].procs, 1);
    }

    #[test]
    fn scan_fake_system() {
        let home = temp("home");
        let root = temp("repo");
        let inner = root.join(".claude/worktrees/x");
        let other = root.with_file_name(format!("{}-feature", root.file_name().unwrap().to_string_lossy()));
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let wts = vec![root.clone(), inner.clone(), other.clone()];
        // claude in the nested worktree, its turn over
        write_session(&home, &inner, "aaa", &[USER, ASSISTANT_DONE, r#"{"type":"ai-title","aiTitle":"Fix the parser"}"#, r#"{"type":"last-prompt","lastPrompt":"secret"}"#], 120);
        // a newer log in the same folder belongs to another session: the pid record picks ours
        write_session(&home, &inner, "bbb", &[USER, ASK_TOOL], 60);
        write_pid(&home, 12, "aaa", "idle", &inner);
        write_pid(&home, 13, "aaa", "idle", &inner);
        // a fresh log with no process: working
        write_session(&home, &other, "ccc", &[USER], 1);
        let term = Proc { pid: 10, ppid: Some(1), name: "ghostty".into(), exe: Some("/Applications/Ghostty.app/Contents/MacOS/ghostty".into()), ..Default::default() };
        let inner_s = inner.to_str().unwrap();
        let procs = vec![
            term,
            proc(11, 10, "zsh", &["-zsh"], Some(root.to_str().unwrap())),
            proc(12, 11, "claude", &["claude"], Some(inner_s)),
            // a second terminal on the same session
            proc(13, 11, "claude", &["claude", "--resume"], Some(inner_s)),
            // codex: a node wrapper and the real binary; one agent
            proc(20, 11, "node", &["node", "/opt/homebrew/bin/codex"], Some(root.join("src").to_str().unwrap())),
            proc(21, 20, "codex", &["codex"], Some(root.join("src").to_str().unwrap())),
            Proc { cpu: 40., ..proc(22, 21, "cargo", &["cargo", "test"], Some(root.to_str().unwrap())) },
            // not inside any worktree
            proc(30, 1, "claude", &["claude"], Some("/tmp")),
        ];
        let now = secs(SystemTime::now());
        let found = scan(&home, &wts, &procs, now);
        assert_eq!(found.len(), 3, "{found:?}");
        let a = &found[&inner];
        assert_eq!((a.len(), a[0].kind, a[0].state, a[0].procs), (1, "claude", AgentState::Waiting, 2));
        assert_eq!(a[0].session_id.as_deref(), Some("aaa"));
        assert!(a[0].session_file.as_ref().unwrap().ends_with("aaa.jsonl"));
        assert_eq!(a[0].title.as_deref(), Some("Fix the parser"));
        assert_eq!(a[0].host_app, Some(PathBuf::from("/Applications/Ghostty.app")));
        assert_eq!(a[0].started, Some(1000));
        let r = &found[&root];
        assert_eq!((r.len(), r[0].kind, r[0].pid, r[0].state), (1, "codex", Some(20), AgentState::Working));
        let o = &found[&other];
        assert_eq!((o[0].kind, o[0].pid, o[0].state), ("claude", None, AgentState::Working));
        // Claude Code says it is blocked on us
        write_pid(&home, 12, "aaa", "waiting", &inner);
        write_pid(&home, 13, "aaa", "waiting", &inner);
        let a = &scan(&home, &wts, &procs, now)[&inner][0];
        assert_eq!((a.state, a.need), (AgentState::NeedsYou, Some(Need::Permission)));
        // an old log without a process is nothing
        write_session(&home, &other, "ccc", &[USER], 3600);
        assert!(!scan(&home, &wts, &procs, now).contains_key(&other));
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&other);
    }

    #[test]
    fn long_lines_and_tail() {
        let home = temp("tail");
        let cwd = PathBuf::from("/w/p");
        let big = format!(r#"{{"type":"user","message":{{"content":"{}"}}}}"#, "x".repeat(200_000));
        write_session(&home, &cwd, "s", &[ASSISTANT_DONE, &big, BASH, r#"{"type":"custom-title","customTitle":"Mine"}"#], 50);
        let s = claude_session(&home, None, &cwd).unwrap();
        assert_eq!(s.last, Some(Last::Assistant { tool: Some("Bash".into()), question: false, error: None }));
        assert_eq!(s.title.as_deref(), Some("Mine"));
        write_session(&home, &cwd, "s", &[ASSISTANT_DONE, &big], 50);
        assert_eq!(claude_session(&home, None, &cwd).unwrap().last, None, "the cut line is skipped, the older one is out of the window");
        let _ = std::fs::remove_dir_all(&home);
    }
}
