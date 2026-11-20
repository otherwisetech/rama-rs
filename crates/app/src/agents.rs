//! AI agents at work in worktrees, found read-only and offline: agent CLI processes whose
//! working directory is inside a worktree, plus Claude Code's session logs
//! (`~/.claude/projects/<escaped cwd>/*.jsonl`: only file times and the trailing lines' `type`,
//! `stop_reason` and session title are read; conversation text is never kept, shown or logged).
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
/// Bytes read from the end of a session log.
const TAIL: u64 = 64 * 1024;
/// Polls a working agent must look quiet before it is shown as waiting / idle (no flicker).
const SETTLE_POLLS: u8 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum AgentState {
    Idle,
    Waiting,
    Working,
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
    /// When `state` began (filled in by [`Tracker`]).
    pub since: i64,
    pub started: Option<i64>,
    pub last_activity: Option<i64>,
    pub title: Option<String>,
    /// macOS app hosting the agent's terminal (`/Applications/Ghostty.app`), to bring forward.
    pub host_app: Option<PathBuf>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Last {
    /// The model answered and stopped; `tool` when it stopped on a tool call (a permission
    /// prompt, or a tool still running).
    Assistant { tool: bool },
    User,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    pub modified: i64,
    pub last: Option<Last>,
    pub title: Option<String>,
}

fn secs(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// The newest session log under `~/.claude/projects/<escaped cwd>/`.
pub fn claude_session(home: &Path, cwd: &Path) -> Option<Session> {
    let dir = home.join(".claude/projects").join(claude_dir_name(cwd));
    let newest = std::fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max_by_key(|(m, _)| *m)?;
    let (last, title) = read_tail(&newest.1);
    Some(Session { modified: secs(newest.0), last, title })
}

/// The last user / assistant turn and the latest session title in the end of a log. Long lines
/// cut by the window are skipped.
fn read_tail(path: &Path) -> (Option<Last>, Option<String>) {
    let Ok(mut f) = std::fs::File::open(path) else { return (None, None) };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let from = len.saturating_sub(TAIL);
    if f.seek(SeekFrom::Start(from)).is_err() {
        return (None, None);
    }
    let mut buf = Vec::new();
    if f.take(TAIL).read_to_end(&mut buf).is_err() {
        return (None, None);
    }
    let text = String::from_utf8_lossy(&buf);
    let mut lines: Vec<&str> = text.split('\n').collect();
    if from > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let (mut last, mut title) = (None, None);
    for line in lines.iter().rev().filter(|l| !l.trim().is_empty()) {
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else { continue };
        let ty = v.get("type").and_then(|t| t.as_str()).unwrap_or_default();
        match ty {
            "assistant" if last.is_none() => {
                let stop = v.pointer("/message/stop_reason").and_then(|s| s.as_str());
                last = Some(Last::Assistant { tool: stop == Some("tool_use") });
            }
            "user" if last.is_none() => last = Some(Last::User),
            "custom-title" if title.is_none() => title = v.get("customTitle").and_then(|s| s.as_str()).map(str::to_string),
            "ai-title" if title.is_none() => title = v.get("aiTitle").and_then(|s| s.as_str()).map(str::to_string),
            "summary" if title.is_none() => title = v.get("summary").and_then(|s| s.as_str()).map(str::to_string),
            _ => {}
        }
        if last.is_some() && title.is_some() {
            break;
        }
    }
    (last, title.filter(|t| !t.trim().is_empty()))
}

/// One poll's verdict from what can be observed.
pub fn classify(now: i64, session: Option<&Session>, tree_cpu: Option<f32>) -> AgentState {
    let age = session.map(|s| now - s.modified);
    if age.is_some_and(|a| a < FRESH_SECS) || tree_cpu.is_some_and(|c| c >= CPU_BUSY) {
        return AgentState::Working;
    }
    match session.and_then(|s| s.last) {
        Some(Last::Assistant { .. }) => AgentState::Waiting,
        Some(Last::User) if age.is_some_and(|a| a < USER_TURN_SECS) => AgentState::Working,
        _ => AgentState::Idle,
    }
}

/// Find the agents working in `worktrees` (canonical paths) among `procs`. A session log
/// changed in the last few seconds counts even without a visible process (another user's
/// process, or a platform where cwds can't be read).
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
            claude_session(home, cwd)
        } else {
            None
        };
        let cpu = tree_cpu(p.pid, &by_pid, &children);
        let state = classify(now, session.as_ref(), Some(cpu));
        let host_app = ancestors(p, &by_pid).find_map(|a| a.exe.as_deref().and_then(app_bundle));
        found.entry(worktrees[w].clone()).or_default().push(Agent {
            kind,
            pid: Some(p.pid),
            state,
            since: now,
            started: Some(p.start).filter(|s| *s > 0),
            last_activity: session.as_ref().map(|s| s.modified),
            title: session.and_then(|s| s.title),
            host_app,
        });
    }
    for w in worktrees {
        if found.contains_key(w) || seen_claude_dirs.contains(&claude_dir_name(w)) {
            continue;
        }
        if let Some(s) = claude_session(home, w).filter(|s| now - s.modified < FRESH_SECS) {
            found.entry(w.clone()).or_default().push(Agent { kind: "claude", pid: None, state: AgentState::Working, since: now, started: None, last_activity: Some(s.modified), title: s.title, host_app: None });
        }
    }
    found
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

type TrackKey = (PathBuf, Option<u32>, &'static str);

/// Remembers each agent's state between polls: when it began, and a short grace period
/// before a working agent is shown as waiting or idle.
#[derive(Debug, Default)]
pub struct Tracker {
    /// (worktree, pid, kind) → (state, since, quiet polls)
    prev: HashMap<TrackKey, (AgentState, i64, u8)>,
}

impl Tracker {
    pub fn apply(&mut self, found: &mut Found) {
        let mut next = HashMap::new();
        for (w, agents) in found.iter_mut() {
            for a in agents.iter_mut() {
                let key = (w.clone(), a.pid, a.kind);
                let (state, since, quiet) = match self.prev.get(&key) {
                    Some(&(AgentState::Working, since, quiet)) if a.state != AgentState::Working && quiet + 1 < SETTLE_POLLS => (AgentState::Working, since, quiet + 1),
                    Some(&(s, since, _)) if s == a.state => (s, since, 0),
                    _ => (a.state, a.since, 0),
                };
                a.state = state;
                a.since = since;
                next.insert(key, (state, since, quiet));
            }
            agents.sort_by_key(|a| (std::cmp::Reverse(a.state), a.kind));
        }
        self.prev = next;
    }
}

/// The badge's state for a worktree: its busiest agent.
pub fn top_state(agents: &[Agent]) -> Option<AgentState> {
    agents.iter().map(|a| a.state).max()
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

    fn write_session(home: &Path, cwd: &Path, lines: &[&str], age: u64) -> PathBuf {
        let dir = home.join(".claude/projects").join(claude_dir_name(cwd));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("s.jsonl");
        std::fs::write(&f, lines.join("\n") + "\n").unwrap();
        let t = SystemTime::now() - std::time::Duration::from_secs(age);
        std::fs::File::options().write(true).open(&f).unwrap().set_modified(t).unwrap();
        f
    }

    const ASSISTANT_DONE: &str = r#"{"type":"assistant","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"secret"}]}}"#;
    const USER: &str = r#"{"type":"user","message":{"role":"user","content":"secret"}}"#;

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
    fn states() {
        let now = 10_000;
        let s = |age: i64, last| Session { modified: now - age, last, title: None };
        assert_eq!(classify(now, Some(&s(3, None)), None), AgentState::Working);
        assert_eq!(classify(now, Some(&s(300, Some(Last::Assistant { tool: false }))), Some(0.)), AgentState::Waiting);
        assert_eq!(classify(now, Some(&s(300, Some(Last::Assistant { tool: true }))), Some(0.)), AgentState::Waiting);
        // a tool (a build) running under the agent
        assert_eq!(classify(now, Some(&s(300, Some(Last::Assistant { tool: true }))), Some(80.)), AgentState::Working);
        assert_eq!(classify(now, Some(&s(60, Some(Last::User))), Some(0.)), AgentState::Working);
        assert_eq!(classify(now, Some(&s(600, Some(Last::User))), Some(0.)), AgentState::Idle);
        assert_eq!(classify(now, None, Some(0.)), AgentState::Idle);
        assert_eq!(classify(now, None, Some(30.)), AgentState::Working);
    }

    #[test]
    fn tracker_settles_and_keeps_since() {
        let w = PathBuf::from("/r");
        let agent = |state, since| Agent { kind: "claude", pid: Some(7), state, since, started: None, last_activity: None, title: None, host_app: None };
        let mut tr = Tracker::default();
        let mut poll = |state, now| {
            let mut f = Found::from([(w.clone(), vec![agent(state, now)])]);
            tr.apply(&mut f);
            let a = &f[&w][0];
            (a.state, a.since)
        };
        assert_eq!(poll(AgentState::Working, 0), (AgentState::Working, 0));
        assert_eq!(poll(AgentState::Working, 3), (AgentState::Working, 0));
        // one quiet poll is not enough to stop working
        assert_eq!(poll(AgentState::Waiting, 6), (AgentState::Working, 0));
        assert_eq!(poll(AgentState::Waiting, 9), (AgentState::Waiting, 9));
        assert_eq!(poll(AgentState::Waiting, 12), (AgentState::Waiting, 9));
        assert_eq!(poll(AgentState::Working, 15), (AgentState::Working, 15));
        assert_eq!(poll(AgentState::Idle, 18), (AgentState::Working, 15));
        assert_eq!(poll(AgentState::Working, 21), (AgentState::Working, 15));
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
        // claude in the nested worktree, waiting for an answer
        write_session(&home, &inner, &[USER, ASSISTANT_DONE, r#"{"type":"ai-title","aiTitle":"Fix the parser"}"#, r#"{"type":"last-prompt","lastPrompt":"secret"}"#], 120);
        // a fresh log with no process: working
        write_session(&home, &other, &[USER], 1);
        let term = Proc { pid: 10, ppid: Some(1), name: "ghostty".into(), exe: Some("/Applications/Ghostty.app/Contents/MacOS/ghostty".into()), ..Default::default() };
        let procs = vec![
            term,
            proc(11, 10, "zsh", &["-zsh"], Some(root.to_str().unwrap())),
            proc(12, 11, "claude", &["claude"], Some(inner.to_str().unwrap())),
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
        assert_eq!((a.len(), a[0].kind, a[0].state), (1, "claude", AgentState::Waiting));
        assert_eq!(a[0].title.as_deref(), Some("Fix the parser"));
        assert_eq!(a[0].host_app, Some(PathBuf::from("/Applications/Ghostty.app")));
        assert_eq!(a[0].started, Some(1000));
        let r = &found[&root];
        assert_eq!((r.len(), r[0].kind, r[0].pid, r[0].state), (1, "codex", Some(20), AgentState::Working));
        let o = &found[&other];
        assert_eq!((o[0].kind, o[0].pid, o[0].state), ("claude", None, AgentState::Working));
        // an old log without a process is nothing
        write_session(&home, &other, &[USER], 3600);
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
        let tool = r#"{"type":"assistant","message":{"stop_reason":"tool_use","content":[]}}"#;
        write_session(&home, &cwd, &[ASSISTANT_DONE, &big, tool, r#"{"type":"custom-title","customTitle":"Mine"}"#], 50);
        let s = claude_session(&home, &cwd).unwrap();
        assert_eq!(s.last, Some(Last::Assistant { tool: true }));
        assert_eq!(s.title.as_deref(), Some("Mine"));
        write_session(&home, &cwd, &[ASSISTANT_DONE, &big], 50);
        assert_eq!(claude_session(&home, &cwd).unwrap().last, None, "the cut line is skipped, the older one is out of the window");
        let _ = std::fs::remove_dir_all(&home);
    }
}
