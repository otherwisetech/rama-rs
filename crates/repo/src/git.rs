//! Write operations run the `git` binary, not gitoxide, so hooks, `core.hooksPath` (husky),
//! signing, LFS, credential helpers and the user's config behave exactly as on the command line.
use crate::Result;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub enum GitError {
    /// No `git` executable on PATH.
    Missing,
    /// git exited non-zero; its stderr (or stdout).
    Failed(String),
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GitError::Missing => f.write_str("git: not found"), // i18n-ok: the app shows its own text
            GitError::Failed(s) => f.write_str(s.trim()),
        }
    }
}

impl std::error::Error for GitError {}

/// Set from another thread to stop a running command (and the hooks it spawned).
#[derive(Clone, Default)]
pub struct Cancel(Arc<AtomicBool>);

impl Cancel {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// How a streamed command ended.
#[derive(Debug, Clone, Default)]
pub struct Run {
    pub success: bool,
    pub cancelled: bool,
    /// stdout and stderr interleaved, progress lines collapsed to their last state.
    pub output: String,
    /// The hook that exited non-zero (`pre-commit`, `commit-msg`, …), from git's trace2 events.
    pub failed_hook: Option<String>,
}

/// `git` with the resolved login-shell environment, never prompting on a terminal.
pub(crate) fn command(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.current_dir(dir);
    for (k, v) in crate::shell_env::resolved() {
        c.env(k, v);
    }
    c.env("GIT_TERMINAL_PROMPT", "0");
    // merges from pull must never open an editor: there is no terminal
    c.env("GIT_EDITOR", "true");
    c.env("GIT_SEQUENCE_EDITOR", "true");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    }
    c
}

fn spawn_error(e: std::io::Error) -> crate::Error {
    if e.kind() == std::io::ErrorKind::NotFound { Box::new(GitError::Missing) } else { Box::new(e) }
}

/// `git --version`, or `None` when git is not installed.
pub fn version() -> Option<String> {
    let out = command(&std::env::temp_dir()).arg("--version").stdin(Stdio::null()).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Run git and return stdout; a non-zero exit becomes [`GitError::Failed`].
pub(crate) fn output(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = command(dir).args(args).stdin(Stdio::null()).output().map_err(spawn_error)?;
    if out.status.success() {
        Ok(out.stdout)
    } else {
        let err = if out.stderr.is_empty() { out.stdout } else { out.stderr };
        Err(Box::new(GitError::Failed(String::from_utf8_lossy(&err).into_owned())))
    }
}

/// [`output`] with `input` on stdin (written from another thread, so large inputs can't
/// deadlock against a full stdout pipe).
pub(crate) fn output_with_stdin(dir: &Path, args: &[&str], input: &[u8]) -> Result<Vec<u8>> {
    let mut child = command(dir).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().map_err(spawn_error)?;
    let mut w = child.stdin.take().expect("piped stdin");
    let data = input.to_vec();
    let writer = std::thread::spawn(move || {
        use std::io::Write;
        let _ = w.write_all(&data);
    });
    let out = child.wait_with_output()?;
    let _ = writer.join();
    if out.status.success() {
        Ok(out.stdout)
    } else {
        Err(Box::new(GitError::Failed(String::from_utf8_lossy(&out.stderr).into_owned())))
    }
}

static TRACE_SEQ: AtomicU64 = AtomicU64::new(0);

/// Grace period between asking a cancelled hook to stop (SIGINT, as Ctrl+C would) and killing it;
/// lint-staged uses it to restore the user's unstaged changes.
const KILL_AFTER: Duration = Duration::from_secs(3);

/// Run git off the UI thread, streaming each output line to `on_line(text, replaces_previous)`;
/// `replaces_previous` is set for `\r` progress updates. `cancel` stops the whole process group.
pub fn run(dir: &Path, args: &[&str], stdin: Option<&[u8]>, cancel: &Cancel, on_line: &mut dyn FnMut(&str, bool)) -> Result<Run> {
    // the one forced write: removing a dirty worktree, after its own second confirmation
    let worktree_remove = args.starts_with(&["worktree", "remove"]);
    debug_assert!(worktree_remove || !args.iter().any(|a| a.starts_with("--force") || *a == "-f"), "rama never forces"); // i18n-ok
    let trace = std::env::temp_dir().join(format!("rama-trace2-{}-{}.json", std::process::id(), TRACE_SEQ.fetch_add(1, Ordering::Relaxed)));
    let mut cmd = command(dir);
    cmd.args(args).env("GIT_TRACE2_EVENT", &trace).stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() }).stdout(Stdio::piped()).stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // own session and process group: cancel can signal every hook process at once, and
        // ssh / gpg can't stop the command waiting for a password on a terminal it can't use
        // SAFETY: setsid is async-signal-safe and touches nothing in the parent
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().map_err(spawn_error)?;
    if let (Some(data), Some(mut w)) = (stdin, child.stdin.take()) {
        let data = data.to_vec();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = w.write_all(&data);
        });
    }
    let (tx, rx) = mpsc::channel::<(usize, String, bool)>();
    let sources = [child.stdout.take().map(|s| Box::new(s) as Box<dyn Read + Send>), child.stderr.take().map(|s| Box::new(s) as Box<dyn Read + Send>)];
    for (stream, src) in sources.into_iter().enumerate() {
        let Some(src) = src else { continue };
        let tx = tx.clone();
        std::thread::spawn(move || pump(src, &mut |line, cr| tx.send((stream, line, cr)).is_ok()));
    }
    drop(tx);

    let mut lines: Vec<String> = Vec::new();
    // stream whose last line was a progress update still on screen
    let mut progress: Option<usize> = None;
    let mut interrupted: Option<Instant> = None;
    let mut exited: Option<Instant> = None;
    loop {
        match rx.recv_timeout(Duration::from_millis(50)) {
            Ok((stream, text, cr)) => {
                let replace = progress == Some(stream) && !lines.is_empty();
                if replace {
                    *lines.last_mut().unwrap() = text.clone();
                } else {
                    lines.push(text.clone());
                }
                on_line(&text, replace);
                progress = cr.then_some(stream);
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if cancel.is_cancelled() {
            match interrupted {
                None => {
                    stop(&mut child, false);
                    interrupted = Some(Instant::now());
                }
                Some(at) if at.elapsed() > KILL_AFTER => stop(&mut child, true),
                _ => {}
            }
        }
        // a hook may leave a background process holding the pipes open
        if exited.is_none() && matches!(child.try_wait(), Ok(Some(_))) {
            exited = Some(Instant::now());
        }
        if exited.is_some_and(|t| t.elapsed() > Duration::from_secs(1)) {
            break;
        }
    }
    let status = child.wait()?;
    let events = std::fs::read_to_string(&trace).unwrap_or_default();
    let _ = std::fs::remove_file(&trace);
    let cancelled = cancel.is_cancelled();
    Ok(Run { success: status.success() && !cancelled, cancelled, output: lines.join("\n"), failed_hook: failed_hook(&events) })
}

fn pump(mut src: Box<dyn Read + Send>, send: &mut dyn FnMut(String, bool) -> bool) {
    let mut buf = [0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    // a line ended by "\r" waits for the next byte: "\r\n" is a plain line end, anything else
    // means the line is a progress update that the next one overwrites
    let mut held: Option<String> = None;
    loop {
        let n = match src.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        for &b in &buf[..n] {
            if let Some(line) = held.take() {
                if b == b'\n' {
                    if !send(line, false) {
                        return;
                    }
                    continue;
                }
                if !send(line, true) {
                    return;
                }
            }
            if b == b'\n' || b == b'\r' {
                let line = strip_ansi(&String::from_utf8_lossy(&pending));
                pending.clear();
                if b == b'\r' {
                    held = Some(line);
                } else if !send(line, false) {
                    return;
                }
            } else {
                pending.push(b);
            }
        }
    }
    if let Some(line) = held {
        send(line, false);
    }
    if !pending.is_empty() {
        send(strip_ansi(&String::from_utf8_lossy(&pending)), false);
    }
}

/// Hook output is often coloured; the panel shows plain text.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut it = s.chars().peekable();
    while let Some(c) = it.next() {
        if c == '\u{1b}' {
            if it.peek() == Some(&'[') {
                it.next();
                for c in it.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(unix)]
fn stop(child: &mut Child, kill: bool) {
    let sig = if kill { libc::SIGKILL } else { libc::SIGINT };
    // SAFETY: plain syscall; the child leads its own session and process group (setsid)
    unsafe {
        libc::killpg(child.id() as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn stop(child: &mut Child, _kill: bool) {
    let _ = Command::new("taskkill").args(["/PID", &child.id().to_string(), "/T", "/F"]).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).status();
    let _ = child.kill();
}

/// The hook that exited non-zero, from git's trace2 event stream (`child_start` with
/// `"child_class":"hook"` and its matching `child_exit`).
pub fn failed_hook(events: &str) -> Option<String> {
    let mut hooks: Vec<(String, String)> = Vec::new();
    for line in events.lines() {
        match json_field(line, "event") {
            Some("child_start") if json_field(line, "child_class") == Some("hook") => {
                if let (Some(id), Some(name)) = (json_field(line, "child_id"), json_field(line, "hook_name")) {
                    hooks.push((id.to_string(), name.to_string()));
                }
            }
            Some("child_exit") => {
                let (Some(id), Some(code)) = (json_field(line, "child_id"), json_field(line, "code")) else { continue };
                if code != "0"
                    && let Some((_, name)) = hooks.iter().find(|(i, _)| i == id)
                {
                    return Some(name.clone());
                }
            }
            _ => {}
        }
    }
    None
}

/// Value of a top-level `"key":value` in one line of compact JSON (string without escapes, or
/// a bare number). Enough for trace2 events.
fn json_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("\"{key}\":");
    let rest = &line[line.find(&pat)? + pat.len()..];
    if let Some(s) = rest.strip_prefix('"') {
        Some(&s[..s.find('"')?])
    } else {
        let end = rest.find([',', '}']).unwrap_or(rest.len());
        Some(rest[..end].trim())
    }
}

pub(crate) fn trash_paths(paths: &[PathBuf]) -> Result<()> {
    trash::delete_all(paths)?;
    Ok(())
}
