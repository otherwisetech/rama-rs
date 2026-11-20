//! Opening the user's own terminal in a folder, optionally running a command there (an AI
//! agent CLI). No embedded terminal: we build the exact command line for each terminal app,
//! show it once, then spawn it with the login-shell environment.
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[allow(clippy::enum_variant_names)] // product names
pub enum Term {
    TerminalApp,
    ITerm,
    Ghostty,
    Warp,
    WezTerm,
    Kitty,
    Alacritty,
    Gnome,
    Konsole,
    /// `$TERMINAL` on Linux.
    Env,
    WindowsTerminal,
    PowerShell,
    Cmd,
}

const ALL: &[Term] = &[Term::TerminalApp, Term::ITerm, Term::Ghostty, Term::Warp, Term::WezTerm, Term::Kitty, Term::Alacritty, Term::Gnome, Term::Konsole, Term::Env, Term::WindowsTerminal, Term::PowerShell, Term::Cmd];

/// AI agent CLIs looked for on PATH, in menu order.
pub const AGENTS: &[&str] = &["claude", "codex", "opencode", "pi", "gemini", "aider"];

impl Term {
    pub fn id(self) -> &'static str {
        match self {
            Term::TerminalApp => "terminal",
            Term::ITerm => "iterm",
            Term::Ghostty => "ghostty",
            Term::Warp => "warp",
            Term::WezTerm => "wezterm",
            Term::Kitty => "kitty",
            Term::Alacritty => "alacritty",
            Term::Gnome => "gnome-terminal",
            Term::Konsole => "konsole",
            Term::Env => "env",
            Term::WindowsTerminal => "wt",
            Term::PowerShell => "powershell",
            Term::Cmd => "cmd",
        }
    }

    pub fn from_id(id: &str) -> Option<Term> {
        ALL.iter().copied().find(|t| t.id() == id)
    }

    /// Product name (not translated).
    pub fn label(self) -> String {
        match self {
            Term::TerminalApp => "Terminal".into(), // i18n-ok: product name
            Term::ITerm => "iTerm2".into(),         // i18n-ok: product name
            Term::Ghostty => "Ghostty".into(),      // i18n-ok: product name
            Term::Warp => "Warp".into(),            // i18n-ok: product name
            Term::WezTerm => "WezTerm".into(),      // i18n-ok: product name
            Term::Kitty => "kitty".into(),          // i18n-ok: product name
            Term::Alacritty => "Alacritty".into(),  // i18n-ok: product name
            Term::Gnome => "GNOME Terminal".into(), // i18n-ok: product name
            Term::Konsole => "Konsole".into(),      // i18n-ok: product name
            Term::Env => format!("$TERMINAL ({})", std::env::var("TERMINAL").unwrap_or_default()), // i18n-ok: variable name
            Term::WindowsTerminal => "Windows Terminal".into(), // i18n-ok: product name
            Term::PowerShell => "PowerShell".into(), // i18n-ok: product name
            Term::Cmd => "cmd".into(),               // i18n-ok: product name
        }
    }

    fn platform() -> &'static [Term] {
        if cfg!(target_os = "macos") {
            &ALL[..7]
        } else if cfg!(windows) {
            &[Term::WindowsTerminal, Term::PowerShell, Term::Cmd]
        } else {
            &[Term::Env, Term::Gnome, Term::Konsole, Term::Kitty, Term::Alacritty, Term::WezTerm]
        }
    }

    /// macOS app bundle name.
    fn bundle(self) -> &'static str {
        match self {
            Term::TerminalApp => "Terminal",
            Term::ITerm => "iTerm",
            Term::Ghostty => "Ghostty",
            Term::Warp => "Warp",
            Term::WezTerm => "WezTerm",
            Term::Kitty => "kitty",
            Term::Alacritty => "Alacritty",
            _ => "",
        }
    }

    /// Linux / Windows executable.
    fn exe(self) -> &'static str {
        match self {
            Term::Gnome => "gnome-terminal",
            Term::Konsole => "konsole",
            Term::Kitty => "kitty",
            Term::Alacritty => "alacritty",
            Term::WezTerm => "wezterm",
            Term::WindowsTerminal => "wt",
            Term::PowerShell => "powershell",
            Term::Cmd => "cmd",
            _ => "",
        }
    }

    fn installed(self) -> bool {
        if cfg!(target_os = "macos") {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            let app = format!("{}.app", self.bundle());
            return [PathBuf::from("/Applications"), PathBuf::from("/System/Applications/Utilities"), home.join("Applications")].iter().any(|d| d.join(&app).exists());
        }
        match self {
            Term::Env => std::env::var("TERMINAL").is_ok_and(|t| !t.is_empty()),
            Term::Cmd | Term::PowerShell => cfg!(windows),
            t => repo::shell_env::which(t.exe()).is_some(),
        }
    }
}

/// Terminals found on this computer, in preference order.
pub fn detect_terminals() -> Vec<Term> {
    Term::platform().iter().copied().filter(|t| t.installed()).collect()
}

/// The preferred terminal if it's installed, else the first one found.
pub fn pick_terminal(preferred: Option<&str>, found: &[Term]) -> Option<Term> {
    preferred.and_then(Term::from_id).filter(|t| found.contains(t)).or_else(|| found.first().copied())
}

/// Agent CLIs found on the login-shell PATH.
pub fn detect_agents() -> Vec<String> {
    AGENTS.iter().filter(|a| repo::shell_env::which(a).is_some()).map(|a| a.to_string()).collect()
}

/// A process to spawn, plus what to tell the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    /// This terminal can't be told to run a command: it opens at the folder and the command is
    /// copied to the clipboard to paste.
    pub copy: Option<String>,
}

impl Launch {
    /// The command line, shell-quoted, as shown before running it.
    pub fn display(&self) -> String {
        std::iter::once(self.program.as_str()).chain(self.args.iter().map(String::as_str)).map(sh_quote).collect::<Vec<_>>().join(" ")
    }

    pub fn spawn(&self) -> std::io::Result<()> {
        let mut c = Command::new(&self.program);
        c.args(&self.args).current_dir(&self.cwd).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
        for (k, v) in repo::shell_env::resolved() {
            c.env(k, v);
        }
        let mut child = c.spawn()?;
        // reap it so it doesn't linger as a zombie
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        Ok(())
    }
}

/// POSIX shell quoting: plain words as is, anything else in single quotes.
pub fn sh_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_./=:@%+,".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

pub(crate) fn applescript_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `$SHELL -lc '<cmd>; exec $SHELL -l'`: the login shell's PATH for the command, and a shell
/// left open when it ends.
fn shell_wrap(shell: &str, command: &str) -> Vec<String> {
    vec![shell.to_string(), "-lc".into(), format!("{command}; exec {} -l", sh_quote(shell))]
}

/// How to open `term` at `dir`, running `command` there if given. `shell` is the user's login
/// shell (`$SHELL`).
pub fn plan(term: Term, dir: &Path, command: Option<&str>, shell: &str) -> Launch {
    let d = dir.display().to_string();
    let cmd = command.map(str::trim).filter(|c| !c.is_empty());
    let l = |program: &str, args: Vec<String>| Launch { program: program.into(), args, cwd: dir.to_path_buf(), copy: None };
    let open_new = |bundle: &str, mut args: Vec<String>| {
        let mut a = vec!["-na".to_string(), bundle.to_string(), "--args".to_string()];
        a.append(&mut args);
        l("open", a)
    };
    match (term, cmd) {
        (Term::TerminalApp | Term::ITerm | Term::Warp, None) if cfg!(target_os = "macos") => l("open", vec!["-a".into(), term.bundle().into(), d]),
        (Term::TerminalApp, Some(c)) => {
            let script = format!("cd {} && {c}", sh_quote(&d));
            l("osascript", vec!["-e".into(), format!("tell application \"Terminal\" to do script {}", applescript_string(&script)), "-e".into(), "tell application \"Terminal\" to activate".into()])
        }
        (Term::ITerm, Some(c)) => {
            let script = format!("cd {} && {c}", sh_quote(&d));
            let lines = ["tell application \"iTerm\"".to_string(), "activate".into(), "set w to (create window with default profile)".into(), format!("tell current session of w to write text {}", applescript_string(&script)), "end tell".into()];
            l("osascript", lines.into_iter().flat_map(|x| ["-e".to_string(), x]).collect())
        }
        (Term::Warp, Some(c)) => Launch { copy: Some(c.to_string()), ..l("open", vec!["-a".into(), "Warp".into(), d]) },
        (Term::Ghostty, c) => {
            let mut a = vec![format!("--working-directory={d}")];
            if let Some(c) = c {
                a.push("-e".into());
                a.extend(shell_wrap(shell, c));
            }
            if cfg!(target_os = "macos") { open_new("Ghostty", a) } else { l("ghostty", a) }
        }
        (Term::WezTerm, c) => {
            let mut a = vec!["start".to_string(), "--cwd".into(), d];
            if let Some(c) = c {
                a.push("--".into());
                a.extend(shell_wrap(shell, c));
            }
            if cfg!(target_os = "macos") { open_new("WezTerm", a) } else { l("wezterm", a) }
        }
        (Term::Kitty, c) => {
            let mut a = vec!["--directory".to_string(), d];
            if let Some(c) = c {
                a.extend(shell_wrap(shell, c));
            }
            if cfg!(target_os = "macos") { open_new("kitty", a) } else { l("kitty", a) }
        }
        (Term::Alacritty, c) => {
            let mut a = vec!["--working-directory".to_string(), d];
            if let Some(c) = c {
                a.push("-e".into());
                a.extend(shell_wrap(shell, c));
            }
            if cfg!(target_os = "macos") { open_new("Alacritty", a) } else { l("alacritty", a) }
        }
        (Term::Gnome, c) => {
            let mut a = vec![format!("--working-directory={d}")];
            if let Some(c) = c {
                a.push("--".into());
                a.extend(shell_wrap(shell, c));
            }
            l("gnome-terminal", a)
        }
        (Term::Konsole, c) => {
            let mut a = vec!["--workdir".to_string(), d];
            if let Some(c) = c {
                a.push("-e".into());
                a.extend(shell_wrap(shell, c));
            }
            l("konsole", a)
        }
        (Term::Env, c) => {
            let t = std::env::var("TERMINAL").unwrap_or_else(|_| "x-terminal-emulator".into());
            let mut a = Vec::new();
            if let Some(c) = c {
                a.push("-e".into());
                a.extend(shell_wrap(shell, c));
            }
            l(&t, a)
        }
        (Term::WindowsTerminal, c) => {
            let mut a = vec!["-d".to_string(), d];
            if let Some(c) = c {
                a.extend(["cmd".to_string(), "/k".into(), c.to_string()]);
            }
            l("wt", a)
        }
        (Term::PowerShell, c) => {
            let mut a = vec!["/c".to_string(), "start".into(), String::new(), "/D".into(), d, "powershell".into(), "-NoExit".into()];
            if let Some(c) = c {
                a.extend(["-Command".to_string(), c.to_string()]);
            }
            l("cmd", a)
        }
        (Term::Cmd, c) => {
            let mut a = vec!["/c".to_string(), "start".into(), String::new(), "/D".into(), d, "cmd".into()];
            if let Some(c) = c {
                a.extend(["/k".to_string(), c.to_string()]);
            }
            l("cmd", a)
        }
        // Terminal / iTerm / Warp without a command off macOS: not reachable (not detected)
        (t, None) => l("open", vec!["-a".into(), t.bundle().into(), d]),
    }
}

/// Key remembered once a terminal + command pair has been shown and approved.
pub fn approval_key(term: Term, command: Option<&str>) -> String {
    format!("{}|{}", term.id(), command.unwrap_or_default().trim())
}

/// The login shell, for wrapping commands.
pub fn user_shell() -> String {
    repo::shell_env::resolved().iter().find(|(k, _)| k == "SHELL").map(|(_, v)| v.clone()).or_else(|| std::env::var("SHELL").ok()).filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/sh".into())
}

/// Open a folder in the file manager.
pub fn reveal(dir: &Path) -> std::io::Result<()> {
    let program = if cfg!(target_os = "macos") {
        "open"
    } else if cfg!(windows) {
        "explorer"
    } else {
        "xdg-open"
    };
    Launch { program: program.into(), args: vec![dir.display().to_string()], cwd: dir.to_path_buf(), copy: None }.spawn()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(sh_quote("claude"), "claude");
        assert_eq!(sh_quote("/a b/c"), "'/a b/c'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_quote(""), "''");
        assert_eq!(applescript_string("say \"hi\" \\"), "\"say \\\"hi\\\" \\\\\"");
    }

    #[test]
    fn plans() {
        let d = Path::new("/w/my repo");
        let p = plan(Term::Ghostty, d, Some("claude"), "/bin/zsh");
        assert!(p.args.contains(&"--working-directory=/w/my repo".to_string()));
        assert_eq!(&p.args[p.args.len() - 3..], ["/bin/zsh", "-lc", "claude; exec /bin/zsh -l"]);
        let p = plan(Term::Kitty, d, None, "/bin/zsh");
        assert!(p.args.ends_with(&["--directory".to_string(), "/w/my repo".to_string()]));
        let p = plan(Term::Gnome, d, Some("codex"), "/bin/bash");
        assert_eq!(p.program, "gnome-terminal");
        assert_eq!(p.args, ["--working-directory=/w/my repo", "--", "/bin/bash", "-lc", "codex; exec /bin/bash -l"]);
        let p = plan(Term::TerminalApp, d, Some("claude"), "/bin/zsh");
        assert_eq!(p.program, "osascript");
        assert!(p.args[1].contains("cd '/w/my repo' && claude"));
        let p = plan(Term::ITerm, d, Some("aider"), "/bin/zsh");
        assert!(p.args.iter().any(|a| a.contains("write text \"cd '/w/my repo' && aider\"")));
        let p = plan(Term::Warp, d, Some("claude"), "/bin/zsh");
        assert_eq!(p.copy.as_deref(), Some("claude"));
        let p = plan(Term::WindowsTerminal, Path::new("C:\\w"), Some("claude"), "");
        assert_eq!(p.args, ["-d", "C:\\w", "cmd", "/k", "claude"]);
        assert_eq!(plan(Term::Konsole, d, Some("pi"), "/bin/sh").display(), "konsole --workdir '/w/my repo' -e /bin/sh -lc 'pi; exec /bin/sh -l'");
        assert_eq!(approval_key(Term::Ghostty, Some(" claude ")), "ghostty|claude");
        assert_eq!(Term::from_id("wezterm"), Some(Term::WezTerm));
        assert_eq!(pick_terminal(Some("kitty"), &[Term::TerminalApp, Term::Kitty]), Some(Term::Kitty));
        assert_eq!(pick_terminal(Some("kitty"), &[Term::TerminalApp]), Some(Term::TerminalApp));
    }
}
