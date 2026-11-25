//! Where rama keeps its own files: `<data root>/rama/` (`~/.otherwise/rama/` unless the user
//! moved the root in Otherwise Hub) — settings, recent repositories and small caches (folder sizes, star count). Before 0.4 they were in the
//! platform config folder ([`legacy_dir`]); [`migrate_on_startup`] moves them over once, never
//! overwriting anything.
use otherwise_link::storage;
use std::io;
use std::path::{Path, PathBuf};

/// Left in the old folder once its files moved (names the new folder).
pub const MARKER: &str = ".migrated-to-otherwise";

/// `<data root>/rama/` (not created here).
pub fn app_dir() -> PathBuf {
    otherwise_link::paths::app_dir(crate::otherwise::APP_ID)
}

/// The settings folder before 0.4.
pub fn legacy_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("APPDATA").filter(|v| !v.is_empty()).map(|d| PathBuf::from(d).join("rama"));
    }
    let home = std::env::var_os("HOME").map(PathBuf::from);
    if cfg!(target_os = "macos") {
        return home.map(|h| h.join("Library/Application Support/rama"));
    }
    std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()).map(PathBuf::from).or_else(|| home.map(|h| h.join(".config"))).map(|d| d.join("rama"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Migrated {
    Nothing,
    AlreadyDone,
    /// Moved these (relative paths); `kept` were already at the new place and stay behind.
    Done { moved: Vec<String>, kept: Vec<String> },
}

/// Moves every file of `old` into `new` once (rename, or verified copy then delete), leaving
/// [`MARKER`] behind. A file that already exists in `new` is never overwritten.
pub fn migrate(old: &Path, new: &Path) -> io::Result<Migrated> {
    if !old.is_dir() || old == new {
        return Ok(Migrated::Nothing);
    }
    if old.join(MARKER).exists() {
        return Ok(Migrated::AlreadyDone);
    }
    let (mut moved, mut kept) = (Vec::new(), Vec::new());
    move_files(old, new, "", &mut moved, &mut kept)?;
    otherwise_link::fsutil::write_atomic(&old.join(MARKER), format!("{}\n", new.display()).as_bytes())?;
    Ok(Migrated::Done { moved, kept })
}

fn move_files(from: &Path, to: &Path, rel: &str, moved: &mut Vec<String>, kept: &mut Vec<String>) -> io::Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(from)?.collect::<io::Result<_>>()?;
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        let r = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
        let t = e.file_type()?;
        if t.is_dir() {
            move_files(&e.path(), &to.join(&name), &r, moved, kept)?;
            let _ = std::fs::remove_dir(e.path());
        } else if t.is_file() && name != MARKER && !storage::skipped_in_copy(&name) {
            let dest = to.join(&name);
            if dest.exists() {
                kept.push(r);
            } else {
                storage::move_any(&e.path(), &dest)?;
                moved.push(r);
            }
        }
    }
    Ok(())
}

/// [`migrate`] from the real old folder, once at startup. Skipped when `OTHERWISE_DATA_DIR` is
/// set (tests, portable setups).
pub fn migrate_on_startup() {
    if std::env::var_os(otherwise_link::paths::ENV).is_some_and(|v| !v.is_empty()) {
        return;
    }
    let Some(old) = legacy_dir() else { return };
    match migrate(&old, &app_dir()) {
        Ok(Migrated::Done { kept, .. }) if !kept.is_empty() => eprintln!("rama: left in {}: {}", old.display(), kept.join(", ")), // i18n-ok: log only
        Ok(_) => {}
        Err(e) => eprintln!("rama: moving {} failed: {e}", old.display()), // i18n-ok: log only
    }
}

/// Shows `<data root>/rama/` in the file manager.
pub fn reveal_data_dir(cx: &mut gpui::App) {
    let d = app_dir();
    let _ = std::fs::create_dir_all(&d);
    cx.reveal_path(&d);
}

#[cfg(test)]
mod tests {
    use super::{MARKER, Migrated, migrate};

    #[test]
    fn migration_moves_once_and_never_overwrites() {
        let t = tempfile::tempdir().unwrap();
        let (old, new) = (t.path().join("old"), t.path().join("root").join("rama"));
        assert_eq!(migrate(&old, &new).unwrap(), Migrated::Nothing);
        std::fs::create_dir_all(&old).unwrap();
        std::fs::write(old.join("settings.json"), b"{\"language\": \"pt\"}").unwrap();
        std::fs::write(old.join("github-stars.json"), b"{\"old\": 1}").unwrap();
        std::fs::create_dir_all(&new).unwrap();
        std::fs::write(new.join("github-stars.json"), b"{\"new\": 1}").unwrap();
        let m = migrate(&old, &new).unwrap();
        assert_eq!(m, Migrated::Done { moved: vec!["settings.json".into()], kept: vec!["github-stars.json".into()] });
        assert_eq!(std::fs::read(new.join("settings.json")).unwrap(), b"{\"language\": \"pt\"}");
        assert_eq!(std::fs::read(new.join("github-stars.json")).unwrap(), b"{\"new\": 1}");
        assert!(old.join("github-stars.json").exists() && !old.join("settings.json").exists());
        assert!(old.join(MARKER).exists());
        assert_eq!(migrate(&old, &new).unwrap(), Migrated::AlreadyDone);
    }
}
