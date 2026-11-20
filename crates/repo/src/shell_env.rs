//! The login shell's environment for git subprocesses.
//!
//! An app started from Finder, the Dock or a desktop launcher inherits a minimal PATH, so hooks
//! that call `node`, `npx` or `pnpm` (husky, lint-staged) fail with "command not found" — the
//! classic GUI git client bug. Once, in the background, we ask the user's shell
//! (`$SHELL -l -i -c 'env -0'`) for its environment, which has the PATH set up by `.zprofile`,
//! `.zshrc`, `config.fish`, nvm / fnm / volta / asdf / mise and Homebrew.
use std::collections::HashMap;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const START: &str = "__RAMA_ENV_START__";
const END: &str = "__RAMA_ENV_END__";
const TIMEOUT: Duration = Duration::from_secs(6);

/// Shell-specific or per-process variables that must not leak into git's environment.
const SKIP: &[&str] = &["PWD", "OLDPWD", "SHLVL", "_", "PS1", "PS2", "TERM_SESSION_ID", "ZDOTDIR"];

static RESOLVED: OnceLock<Vec<(String, String)>> = OnceLock::new();

/// Variables to set on every git command. The first call resolves the shell (up to a few
/// seconds); call [`init`] at startup so that happens off the UI thread before it's needed.
pub fn resolved() -> &'static [(String, String)] {
    RESOLVED.get_or_init(|| {
        let current: HashMap<String, String> = std::env::vars().collect();
        let shell = if cfg!(windows) { None } else { login_shell_env() };
        merge(&current, shell.as_ref())
    })
}

/// Start resolving in a background thread.
pub fn init() {
    std::thread::spawn(|| {
        resolved();
    });
}

/// The resolved PATH, for display.
pub fn path() -> Option<&'static str> {
    resolved().iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.as_str())
}

fn login_shell_env() -> Option<HashMap<String, String>> {
    let shell = std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or_else(|| if cfg!(target_os = "macos") { "/bin/zsh".into() } else { "/bin/sh".into() });
    // fish and POSIX shells both understand `printf`, `;` and `-l -i -c`
    let script = format!("printf '{START}'; command env -0; printf '{END}'");
    let mut cmd = std::process::Command::new(&shell);
    cmd.args(["-l", "-i", "-c", &script]).stdin(std::process::Stdio::null()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        // a new session without a controlling terminal: an interactive shell started from a
        // terminal would otherwise grab its foreground process group
        // SAFETY: setsid is async-signal-safe and touches nothing in the parent
        unsafe {
            cmd.pre_exec(|| {
                libc::setsid();
                Ok(())
            });
        }
    }
    let mut child = cmd.spawn().ok()?;
    let mut out = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = std::io::Read::read_to_end(&mut out, &mut buf);
        buf
    });
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if started.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                // an rc file waiting for input; give up and keep the inherited env
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
    parse_env_block(&reader.join().ok()?)
}

/// Pull `KEY=value` pairs out of `env -0` output framed by the start/end markers; whatever rc
/// files print around it (banners, warnings) is ignored. `None` if the markers are missing.
pub fn parse_env_block(bytes: &[u8]) -> Option<HashMap<String, String>> {
    let s = String::from_utf8_lossy(bytes);
    let from = s.find(START)? + START.len();
    let to = from + s[from..].find(END)?;
    let vars = s[from..to]
        .split('\0')
        .filter_map(|kv| {
            let (k, v) = kv.split_once('=')?;
            let k = k.trim_start_matches('\n');
            (!k.is_empty() && !k.contains(char::is_whitespace)).then(|| (k.to_string(), v.to_string()))
        })
        .collect::<HashMap<_, _>>();
    (!vars.is_empty()).then_some(vars)
}

/// What to set on git commands: the shell's PATH (with entries we already had appended, so
/// nothing is lost) and any shell variable we don't have yet (NVM_DIR, VOLTA_HOME, …). Without a
/// shell env, common install dirs are appended to PATH as a fallback.
pub fn merge(current: &HashMap<String, String>, shell: Option<&HashMap<String, String>>) -> Vec<(String, String)> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let mut path: Vec<String> = Vec::new();
    let mut push = |p: &str| {
        if !p.is_empty() && !path.iter().any(|x| x == p) {
            path.push(p.to_string());
        }
    };
    if let Some(sh) = shell {
        if let Some(p) = sh.get("PATH") {
            p.split(sep).for_each(&mut push);
        }
    }
    if let Some(p) = current.get("PATH") {
        p.split(sep).for_each(&mut push);
    }
    if shell.is_none() && !cfg!(windows) {
        let home = current.get("HOME").cloned().unwrap_or_default();
        for p in ["/opt/homebrew/bin", "/usr/local/bin", &format!("{home}/.volta/bin"), &format!("{home}/.local/share/mise/shims"), &format!("{home}/.asdf/shims"), &format!("{home}/.local/bin")] {
            if std::path::Path::new(p).is_dir() {
                push(p);
            }
        }
    }
    let mut out: Vec<(String, String)> = Vec::new();
    if !path.is_empty() {
        out.push(("PATH".into(), path.join(&sep.to_string())));
    }
    if let Some(sh) = shell {
        let mut extra: Vec<_> = sh.iter().filter(|(k, _)| k.as_str() != "PATH" && !SKIP.contains(&k.as_str()) && !current.contains_key(*k)).map(|(k, v)| (k.clone(), v.clone())).collect();
        extra.sort();
        out.extend(extra);
    }
    out
}

/// Use the inherited environment as is, without asking the login shell (tests, or when the
/// caller already has the right PATH). No effect once resolved.
pub fn use_inherited() {
    let _ = RESOLVED.set(merge(&std::env::vars().collect(), None));
}

/// The first executable called `name` in `path` (a PATH value), like `which`.
pub fn find_in(path: &str, name: &str) -> Option<std::path::PathBuf> {
    let sep = if cfg!(windows) { ';' } else { ':' };
    let exts: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat", ""] } else { &[""] };
    path.split(sep).filter(|d| !d.is_empty()).flat_map(|d| exts.iter().map(move |e| std::path::Path::new(d).join(format!("{name}{e}")))).find(|p| is_executable(p))
}

/// [`find_in`] on the resolved login-shell PATH.
pub fn which(name: &str) -> Option<std::path::PathBuf> {
    find_in(path()?, name)
}

#[cfg(unix)]
fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &std::path::Path) -> bool {
    p.is_file()
}
