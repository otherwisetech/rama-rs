//! Tiny settings file (`settings.json` in the platform config dir): a flat JSON object of
//! strings and string lists, parsed by hand.
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub language: Option<String>,
    /// Local branch order in the sidebar: `"recent"`, else by name.
    pub branch_sort: Option<String>,
    /// Default branch to compare against; auto-detected when unset or missing.
    pub default_branch: Option<String>,
    /// Branch patterns never offered for cleanup; `repo::PROTECTED` when empty.
    pub protected: Vec<String>,
    /// Folder new worktrees go in; next to the main worktree when unset.
    pub worktree_base: Option<String>,
    /// Preferred terminal (`launch::Term::id`).
    pub terminal: Option<String>,
    /// AI agent commands offered in "Start AI agent here"; the detected CLIs when empty.
    pub agents: Vec<String>,
    /// Terminal + command pairs the user has already seen and approved once.
    pub launch_ok: Vec<String>,
    /// Worktree map: `"list"` or the map, and `"grouped"` or the radial layout.
    pub map_mode: Option<String>,
    pub map_layout: Option<String>,
    /// `"off"` hides the AI agent badges (and stops looking for agents).
    pub agent_badges: Option<String>,
    /// `"on"`: badges don't animate.
    pub reduce_motion: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Val {
    S(String),
    L(Vec<String>),
}

fn dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").filter(|v| !v.is_empty()).map(|d| PathBuf::from(d).join("rama"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/rama"));
    }
    std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| home.map(|h| h.join(".config"))).map(|d| d.join("rama"))
}

pub fn config_dir() -> Option<PathBuf> {
    dir()
}

fn file() -> Option<PathBuf> {
    dir().map(|d| d.join("settings.json"))
}

impl Settings {
    pub fn load() -> Settings {
        file().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| Settings::parse(&s)).unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(f) = file() else { return };
        if let Some(d) = f.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        let _ = std::fs::write(f, self.to_json());
    }

    /// Load, change, save: other windows may have saved in between.
    pub fn update(f: impl FnOnce(&mut Settings)) -> Settings {
        let mut s = Settings::load();
        f(&mut s);
        s.save();
        s
    }

    fn parse(s: &str) -> Settings {
        let m = parse_object(s).unwrap_or_default();
        let st = |k: &str| match m.get(k) {
            Some(Val::S(v)) if !v.is_empty() => Some(v.clone()),
            _ => None,
        };
        let list = |k: &str| match m.get(k) {
            Some(Val::L(v)) => v.clone(),
            _ => Vec::new(),
        };
        Settings {
            language: st("language"),
            branch_sort: st("branch_sort"),
            default_branch: st("default_branch"),
            protected: list("protected"),
            worktree_base: st("worktree_base"),
            terminal: st("terminal"),
            agents: list("agents"),
            launch_ok: list("launch_ok"),
            map_mode: st("map_mode"),
            map_layout: st("map_layout"),
            agent_badges: st("agent_badges"),
            reduce_motion: st("reduce_motion"),
        }
    }

    fn to_json(&self) -> String {
        let strings = [("language", &self.language), ("branch_sort", &self.branch_sort), ("default_branch", &self.default_branch), ("worktree_base", &self.worktree_base), ("terminal", &self.terminal), ("map_mode", &self.map_mode), ("map_layout", &self.map_layout), ("agent_badges", &self.agent_badges), ("reduce_motion", &self.reduce_motion)];
        let lists = [("protected", &self.protected), ("agents", &self.agents), ("launch_ok", &self.launch_ok)];
        let mut fields: Vec<String> = strings.iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("  \"{k}\": {}", quote(v)))).collect();
        fields.extend(lists.iter().filter(|(_, v)| !v.is_empty()).map(|(k, v)| format!("  \"{k}\": [{}]", v.iter().map(|x| quote(x)).collect::<Vec<_>>().join(", "))));
        if fields.is_empty() { "{}\n".into() } else { format!("{{\n{}\n}}\n", fields.join(",\n")) }
    }

    pub fn side_recent(&self) -> bool {
        self.branch_sort.as_deref() == Some("recent")
    }

    pub fn agent_badges(&self) -> bool {
        self.agent_badges.as_deref() != Some("off")
    }

    pub fn reduce_motion(&self) -> bool {
        self.reduce_motion.as_deref() == Some("on")
    }

    pub fn protected_patterns(&self) -> Vec<String> {
        if self.protected.is_empty() { repo::PROTECTED.iter().map(|s| s.to_string()).collect() } else { self.protected.clone() }
    }
}

fn quote(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// A flat object of strings and arrays of strings; anything else is skipped or fails.
fn parse_object(s: &str) -> Option<BTreeMap<String, Val>> {
    let c: Vec<char> = s.chars().collect();
    let mut i = 0;
    let ws = |i: &mut usize| {
        while *i < c.len() && c[*i].is_whitespace() {
            *i += 1;
        }
    };
    fn string(c: &[char], i: &mut usize) -> Option<String> {
        if c.get(*i) != Some(&'"') {
            return None;
        }
        *i += 1;
        let mut out = String::new();
        loop {
            let ch = *c.get(*i)?;
            *i += 1;
            match ch {
                '"' => return Some(out),
                '\\' => {
                    let e = *c.get(*i)?;
                    *i += 1;
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'u' => {
                            let hex: String = c.get(*i..*i + 4)?.iter().collect();
                            *i += 4;
                            out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                        }
                        other => out.push(other),
                    }
                }
                ch => out.push(ch),
            }
        }
    }
    let mut m = BTreeMap::new();
    ws(&mut i);
    if c.get(i) != Some(&'{') {
        return None;
    }
    i += 1;
    loop {
        ws(&mut i);
        match c.get(i) {
            Some('}') => return Some(m),
            Some(',') => {
                i += 1;
                continue;
            }
            _ => {}
        }
        let k = string(&c, &mut i)?;
        ws(&mut i);
        if c.get(i) != Some(&':') {
            return None;
        }
        i += 1;
        ws(&mut i);
        let v = if c.get(i) == Some(&'[') {
            i += 1;
            let mut l = Vec::new();
            loop {
                ws(&mut i);
                match c.get(i) {
                    Some(']') => {
                        i += 1;
                        break;
                    }
                    Some(',') => i += 1,
                    _ => l.push(string(&c, &mut i)?),
                }
            }
            Val::L(l)
        } else {
            Val::S(string(&c, &mut i)?)
        };
        m.insert(k, v);
    }
}

fn system_locale() -> Option<String> {
    for var in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
            && v != "C"
            && v != "POSIX"
        {
            return Some(v);
        }
    }
    if cfg!(target_os = "macos") {
        let out = std::process::Command::new("defaults").args(["read", "-g", "AppleLocale"]).output().ok()?;
        let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !v.is_empty() {
            return Some(v);
        }
    }
    None
}

/// Saved language, else the system one if bundled, else the default.
pub fn initial_language(settings: &Settings) -> &'static str {
    let saved = settings.language.as_deref().and_then(|l| i18n::available().into_iter().map(|(c, _)| c).find(|c| *c == l));
    saved.or_else(|| system_locale().as_deref().and_then(i18n::match_system)).unwrap_or(i18n::DEFAULT)
}

fn recent_file() -> Option<PathBuf> {
    dir().map(|d| d.join("recent.txt"))
}

const RECENT_MAX: usize = 8;

/// Recently opened repositories, most recent first, only those that still exist.
pub fn recent() -> Vec<PathBuf> {
    recent_file().and_then(|f| std::fs::read_to_string(f).ok()).map(|s| s.lines().filter(|l| !l.trim().is_empty()).map(PathBuf::from).filter(|p| p.exists()).take(RECENT_MAX).collect()).unwrap_or_default()
}

pub fn add_recent(path: &std::path::Path) {
    let Some(f) = recent_file() else { return };
    let mut list = recent();
    list.retain(|p| p != path);
    list.insert(0, path.to_path_buf());
    list.truncate(RECENT_MAX);
    if let Some(d) = f.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let _ = std::fs::write(f, list.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("\n"));
}

#[cfg(test)]
mod tests {
    use super::Settings;

    #[test]
    fn roundtrip() {
        let s = Settings { language: Some("en".into()), ..Default::default() };
        assert_eq!(Settings::parse(&s.to_json()), s);
        let s = Settings {
            language: Some("pt".into()),
            branch_sort: Some("recent".into()),
            default_branch: Some("dev".into()),
            protected: vec!["main".into(), "release*".into()],
            worktree_base: Some("/Users/ana/wt \"x\"".into()),
            terminal: Some("ghostty".into()),
            agents: vec!["claude".into(), "codex --full-auto".into(), "aider --model \"x\\y\"".into()],
            launch_ok: vec!["ghostty|claude".into()],
            map_mode: Some("list".into()),
            map_layout: Some("grouped".into()),
            agent_badges: Some("off".into()),
            reduce_motion: Some("on".into()),
        };
        assert_eq!(Settings::parse(&s.to_json()), s);
        assert!(s.side_recent());
        assert!(!s.agent_badges() && s.reduce_motion());
        assert!(Settings::default().agent_badges() && !Settings::default().reduce_motion());
        assert_eq!(Settings::parse(&Settings::default().to_json()), Settings::default());
        assert_eq!(Settings::parse("garbage"), Settings::default());
        // files written by older versions
        assert_eq!(Settings::parse("{\n  \"language\": \"fr\"\n}\n").language.as_deref(), Some("fr"));
        assert_eq!(Settings::default().protected_patterns()[0], "main");
    }
}
