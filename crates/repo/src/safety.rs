//! Places rama never offers to free, mirroring diskmap's `scan::safety` (v0.6.1): tool configs
//! and installs, hidden folders in the home folder, the system, app bundles, cloud-synced
//! folders and whole user folders. rama works inside a repository's worktrees, so on top of
//! [`protected`] (a path judged on its own, as diskmap does) it asks two narrower questions:
//! does the worktree live in a place like that ([`place_protected`]), and does the part of a
//! path below the worktree go through a tool's folder ([`protected_below`]).
use std::path::{Component, Path, PathBuf};

/// Hidden or system folders under the home folder that are regenerable caches
/// (`/`-separated, relative to the home folder). Everything else hidden in home is protected.
pub const HOME_CACHE_ALLOW: &[&str] = &[".npm/_cacache", ".pnpm-store", ".gradle/caches", "Library/pnpm/store", "Library/Developer/Xcode/DerivedData", "AppData/Local/pnpm/store", "AppData/Local/npm-cache/_cacache"];

/// Folder names protected wherever they are: tool configs, installs, credentials, app data.
pub const NAMES: &[&str] = &[
    ".config", ".local", ".opencode", ".codex", ".pi", ".cursor", ".npm", ".nvm", ".rustup", ".cargo", ".bun", ".deno", ".serverless", ".docker", ".kube", ".aws", ".gcloud", ".azure", ".ssh", ".gnupg", ".kiri", ".ollama", ".otherwise", ".git", ".Trash", ".Trashes", "Keychains", "CloudStorage",
    "Mobile Documents",
];

/// Name prefixes protected wherever they are.
pub const PREFIXES: &[&str] = &[".vscode", ".pi-", ".pi_"];

/// Bundles and libraries: never a cleanup target, nor anything inside them.
pub const BUNDLE_SUFFIXES: &[&str] = &[".app", ".appex", ".framework", ".bundle", ".kext", ".plugin", ".photoslibrary", ".musiclibrary", ".keychain", ".keychain-db"];

/// Cloud-synced folders kept directly in the home folder (prefixes: `OneDrive - Acme`).
const CLOUD_IN_HOME: &[&str] = &["OneDrive", "Dropbox", "Google Drive", "iCloud Drive", "Box", "Box Sync", "pCloud Drive", "Nextcloud", "MEGA", "Creative Cloud Files"]; // i18n-ok: folder names

/// Folders under the home folder protected with everything in them.
const HOME_PROTECTED: &[&str] = &["Library", "AppData", "Applications", "Pictures", "Movies", "Music"];

#[cfg(unix)]
const SYSTEM: &[&str] = &["System", "Library", "Applications", "usr", "bin", "sbin", "opt", "private", "etc", "var", "dev", "cores", "nix", "snap", "boot", "proc", "sys", "lib", "lib64", "root"];
#[cfg(windows)]
const SYSTEM: &[&str] = &["Windows", "Program Files", "Program Files (x86)", "ProgramData"];
#[cfg(not(any(unix, windows)))]
const SYSTEM: &[&str] = &[];

/// Files that make a folder a source project: never offered as a build folder.
pub const PROJECT_MARKERS: &[&str] = &[".git", "package.json", "Cargo.toml", "pom.xml", "build.gradle", "build.gradle.kts", "settings.gradle", "settings.gradle.kts", "pyproject.toml", "setup.py", "go.mod", "Gemfile", "composer.json", "Package.swift", "Podfile", "Makefile", "CMakeLists.txt", "deno.json"];

/// The user's home folder, canonical (`HOME`, `USERPROFILE` on Windows).
pub fn user_home() -> Option<PathBuf> {
    let h = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).filter(|h| !h.is_empty())?;
    canonical(Path::new(&h)).ok().or_else(|| Some(PathBuf::from(h)))
}

/// `fs::canonicalize` without Windows' `\\?\` prefix, so the result compares with the plain
/// paths git and the home folder use.
pub fn canonical(p: &Path) -> std::io::Result<PathBuf> {
    let c = std::fs::canonicalize(p)?;
    #[cfg(windows)]
    {
        let s = c.to_string_lossy();
        if let Some(rest) = s.strip_prefix(r"\\?\").filter(|r| r.as_bytes().get(1) == Some(&b':')) {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(c)
}

fn names(p: &Path) -> Vec<&str> {
    p.components().filter_map(|c| if let Component::Normal(n) = c { n.to_str().or(Some("\u{FFFD}")) } else { None }).collect()
}

fn is_cloud_name(n: &str) -> bool {
    CLOUD_IN_HOME.iter().any(|c| n == *c || (n.starts_with(c) && n[c.len()..].starts_with([' ', '-', '_', '(', '.'])))
}

/// A folder name that marks a tool's config or install, a bundle, or a cloud folder.
pub fn protected_name(n: &str) -> bool {
    NAMES.contains(&n) || PREFIXES.iter().any(|p| n.starts_with(p)) || BUNDLE_SUFFIXES.iter().any(|s| n.len() > s.len() && n.ends_with(s))
}

/// `true` when `path` (absolute, with no `..`) must never be offered for deletion, judged on
/// its own (diskmap's rule). `home` is the user's home folder (canonical), or `None`.
pub fn protected(path: &Path, home: Option<&Path>) -> bool {
    if !plain(path) {
        return true;
    }
    let all = names(path);
    if all.iter().any(|n| protected_name(n)) {
        return !home.is_some_and(|h| allowed_cache(path, h));
    }
    let by_place = match home {
        Some(h) if path == h => true,
        Some(h) if path.starts_with(h) => in_home(path, h),
        _ => outside_home(&all),
    };
    by_place || looks_like_project(path)
}

fn plain(p: &Path) -> bool {
    p.is_absolute() && !p.components().any(|c| matches!(c, Component::ParentDir | Component::CurDir))
}

/// `dir` holds a project marker directly: it is (or looks like) source code, not build output.
pub fn looks_like_project(dir: &Path) -> bool {
    PROJECT_MARKERS.iter().any(|m| std::fs::symlink_metadata(dir.join(m)).is_ok())
}

/// The worktree at `worktree` sits somewhere rama offers nothing in: the home folder itself,
/// a hidden folder of it (`~/.codex/worktrees/x`: a tool's own checkout), `~/Library`, a
/// cloud-synced folder, a bundle, a system location. A worktree straight in the home folder
/// (`~/app`) is fine: git registered it, so it is a checkout, not a stray install.
pub fn place_protected(worktree: &Path, home: Option<&Path>) -> bool {
    const PROBE: &str = "\u{1}rama-probe";
    let probe = worktree.join(PROBE);
    if !plain(&probe) || home.is_some_and(|h| worktree == h) {
        return true;
    }
    let all = names(&probe);
    if all.iter().any(|n| protected_name(n)) {
        return true;
    }
    match home {
        Some(h) if probe.starts_with(h) => in_home(&probe, h),
        _ => outside_home(&all),
    }
}

/// A worktree folder that must not go as a whole: anything [`place_protected`], the home
/// folder, a user's home, a volume or drive root.
pub fn removal_protected(worktree: &Path, home: Option<&Path>) -> bool {
    place_protected(worktree, home)
        || match home {
            Some(h) if worktree.starts_with(h) => worktree == h,
            _ => outside_home(&names(worktree)),
        }
}

/// The part of `path` below `worktree` goes through a tool's folder or bundle
/// (`wt/.cargo/…`, `wt/.config/x/node_modules`), or `path` itself holds a project marker.
pub fn protected_below(path: &Path, worktree: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(worktree) else { return true };
    names(rel).iter().any(|n| protected_name(n)) || looks_like_project(path)
}

fn allowed_cache(path: &Path, home: &Path) -> bool {
    HOME_CACHE_ALLOW.iter().any(|a| path.starts_with(home.join(a))) || standalone_target(path, home)
}

/// `~/.cache/<name with "target">` holding a `CACHEDIR.TAG`: a `CARGO_TARGET_DIR`.
fn standalone_target(path: &Path, home: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(home.join(".cache")) else { return false };
    let Some(first) = names(rel).first().map(|s| s.to_string()) else { return false };
    first.contains("target") && home.join(".cache").join(&first).join("CACHEDIR.TAG").is_file()
}

fn in_home(path: &Path, home: &Path) -> bool {
    if allowed_cache(path, home) {
        return false;
    }
    let rel = names(path.strip_prefix(home).unwrap_or(path));
    let Some(first) = rel.first() else { return true };
    first.starts_with('.') || HOME_PROTECTED.contains(first) || is_cloud_name(first) || rel.len() == 1
}

fn outside_home(all: &[&str]) -> bool {
    if all.is_empty() {
        return true;
    }
    if SYSTEM.contains(&all[0]) {
        return true;
    }
    if matches!(all[0], "Volumes" | "mnt" | "media" | "run") && all.len() <= 2 + usize::from(all[0] == "media" || all[0] == "run") {
        return true;
    }
    if let Some(i) = all.iter().position(|n| *n == "Users" || *n == "home") {
        let rest = &all[i + 1..];
        if rest.len() <= 1 {
            return true;
        }
        let first = rest[1];
        if first.starts_with('.') || HOME_PROTECTED.contains(&first) || is_cloud_name(first) || rest.len() == 2 {
            return true;
        }
    }
    false
}
