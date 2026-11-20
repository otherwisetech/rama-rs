//! Tiny settings file (`settings.json` in the platform config dir). Hand-rolled JSON: a few string keys.
use std::path::PathBuf;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Settings {
    pub language: Option<String>,
    /// Local branch order in the sidebar: `"recent"`, else by name.
    pub branch_sort: Option<String>,
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

    fn parse(s: &str) -> Settings {
        Settings { language: json_string(s, "language"), branch_sort: json_string(s, "branch_sort") }
    }

    fn to_json(&self) -> String {
        let fields: Vec<String> = [("language", &self.language), ("branch_sort", &self.branch_sort)].iter().filter_map(|(k, v)| v.as_ref().map(|v| format!("  \"{k}\": \"{}\"", v.replace(['"', '\\'], "")))).collect();
        if fields.is_empty() { "{}\n".into() } else { format!("{{\n{}\n}}\n", fields.join(",\n")) }
    }

    pub fn side_recent(&self) -> bool {
        self.branch_sort.as_deref() == Some("recent")
    }
}

fn json_string(s: &str, key: &str) -> Option<String> {
    let at = s.find(&format!("\"{key}\""))? + key.len() + 2;
    let rest = s[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
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
        let s = Settings { language: Some("en".into()), branch_sort: None };
        assert_eq!(Settings::parse(&s.to_json()), s);
        let s = Settings { language: Some("pt".into()), branch_sort: Some("recent".into()) };
        assert_eq!(Settings::parse(&s.to_json()), s);
        assert!(s.side_recent());
        assert_eq!(Settings::parse(&Settings::default().to_json()), Settings::default());
        assert_eq!(Settings::parse("garbage"), Settings::default());
    }
}
