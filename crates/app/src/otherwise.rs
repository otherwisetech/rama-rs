//! otherwise-link: `otherwise-rama://` links in, other Otherwise apps in About, and agents that
//! wait for you in Otherwise Apps' Today view.
use crate::agents::{Agent, AgentState, ErrKind, Found, Need};
use otherwise_link::apps::{self, AppSpec, Installed, Locations};
use otherwise_link::today::{self, Event, EventKind, Severity};
use otherwise_link::{Link, Root};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

pub const APP_ID: &str = "rama";
#[cfg(target_os = "linux")]
const BUNDLE_ID: &str = "io.github.otherwisetech.rama";
/// Today events outlive the last poll this long: polls stop while rama is hidden, so an agent
/// answered in its terminal meanwhile must not stay listed forever.
const TODAY_TTL_MIN: i64 = 30;

/// What an incoming link asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Bring a window to the front.
    Focus,
    /// Show the repository containing this folder.
    Repo(PathBuf),
    /// Open the conversation of the agent working in `worktree` (the session's, if given).
    Agent { worktree: PathBuf, session: Option<String> },
}

fn abs(link: &Link, key: &str) -> Option<PathBuf> {
    link.get(key).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// `repo?path=…`, `agent?worktree=…&session=…`; anything else of ours focuses.
pub fn action(link: &Link) -> Option<Action> {
    if link.app != APP_ID {
        return None;
    }
    Some(match link.at(0) {
        Some("repo") => abs(link, "path").map(Action::Repo).unwrap_or(Action::Focus),
        Some("agent") => match abs(link, "worktree") {
            Some(worktree) => Action::Agent { worktree, session: link.get("session").filter(|s| !s.is_empty()).map(str::to_string) },
            None => Action::Focus,
        },
        _ => Action::Focus,
    })
}

#[cfg(test)]
fn repo_link(path: &Path) -> Link {
    Link::new(APP_ID).segment("repo").param("path", path.to_string_lossy())
}

pub fn agent_link(worktree: &Path, session: Option<&str>) -> Link {
    let l = Link::new(APP_ID).segment("agent").param("worktree", worktree.to_string_lossy());
    match session {
        Some(s) => l.param("session", s),
        None => l,
    }
}

/// Links that arrived before a window could take them (argv, Apple events off the main loop).
static INBOUND: Mutex<Vec<Action>> = Mutex::new(Vec::new());

pub fn push(a: Action) {
    INBOUND.lock().unwrap_or_else(|e| e.into_inner()).push(a);
}

pub fn take() -> Vec<Action> {
    std::mem::take(&mut *INBOUND.lock().unwrap_or_else(|e| e.into_inner()))
}

/// GPUI's open-URLs callback.
pub fn push_urls(urls: Vec<String>) {
    for a in urls.iter().filter_map(|u| Link::parse(u).ok()).filter_map(|l| action(&l)) {
        push(a);
    }
}

/// The command-line argument: one of our links (Linux `%u`, Windows URL handler) or a folder.
pub fn arg_action(arg: &str) -> Option<Action> {
    match Link::parse(arg) {
        Ok(l) => action(&l),
        // Linux file managers may hand `%u` a file:// URL
        Err(_) => match arg.strip_prefix("file://") {
            Some(rest) => otherwise_link::link::decode(rest, false).ok().map(|p| Action::Repo(PathBuf::from(p))),
            None => Some(Action::Repo(PathBuf::from(arg))),
        },
    }
}

/// Windows and Linux learn about our scheme at run time (macOS from Info.plist).
pub fn register_scheme() {
    std::thread::spawn(|| {
        let scheme = otherwise_link::link::app_scheme(APP_ID);
        #[cfg(windows)]
        if let Ok(exe) = std::env::current_exe() {
            use std::os::windows::process::CommandExt;
            // reg.exe is a console program: without this a window flashes on every start
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            for args in otherwise_link::register::windows_reg_commands(&scheme, &exe, crate::APP_NAME) {
                let _ = std::process::Command::new("reg").args(&args).creation_flags(CREATE_NO_WINDOW).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null()).status();
            }
        }
        #[cfg(target_os = "linux")]
        {
            let apps = otherwise_link::paths::home().join(".local/share/applications");
            for file in ["rama.desktop".to_string(), format!("{BUNDLE_ID}.desktop")] { // i18n-ok: file names
                if apps.join(&file).is_file() {
                    let _ = otherwise_link::register::register_linux(&file, &[&scheme]);
                    break;
                }
            }
        }
        let _ = scheme;
    });
}

/// Other Otherwise apps installed on this computer with a window to open (not CLI-only), by name.
pub fn other_apps(loc: &Locations) -> Vec<(AppSpec, Installed)> {
    let mut found: Vec<_> = apps::discover(loc).into_iter().filter(|(s, i)| s.id != APP_ID && (i.path.is_some() || i.executable.is_some())).collect();
    found.sort_by_key(|(s, _)| s.name.to_lowercase());
    found
}

/// Starts an installed app: the bundle on macOS, its executable elsewhere.
pub fn launch(app: &Installed) -> std::io::Result<()> {
    if cfg!(target_os = "macos")
        && let Some(p) = &app.path
    {
        return std::process::Command::new("open").arg(p).status().map(|_| ());
    }
    let exe = app.executable.as_ref().ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))?;
    std::process::Command::new(exe).spawn().map(|_| ())
}

fn slug(s: &str) -> String {
    let s: String = s.chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
    s.trim_matches('-').to_string()
}

/// Waiting for you (question / permission) or cut off from the API: worth a Today entry.
fn wants_attention(a: &Agent) -> bool {
    a.state == AgentState::NeedsYou || (a.state == AgentState::Error && a.error == Some(ErrKind::Connection))
}

/// Today events for agents that need the user, in the current language. Only the agent's
/// kind, the worktree's folder name and its state go out (no titles, no conversation text).
pub fn agent_events(found: &Found, now: chrono::DateTime<chrono::Utc>) -> Vec<Event> {
    let mut out = Vec::new();
    let mut worktrees: Vec<_> = found.iter().collect();
    worktrees.sort_by(|a, b| a.0.cmp(b.0));
    for (w, agents) in worktrees {
        let name = w.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| w.display().to_string());
        for a in agents.iter().filter(|a| wants_attention(a)) {
            let who = a.session_id.clone().or_else(|| a.pid.map(|p| p.to_string())).unwrap_or_else(|| a.kind.to_string());
            let (kind, severity, title) = match (a.state, a.need) {
                (AgentState::NeedsYou, Some(Need::Question)) => (EventKind::Review, Severity::High, i18n::t!("app.today.agent_question", agent = a.kind, worktree = name)),
                (AgentState::NeedsYou, _) => (EventKind::Review, Severity::High, i18n::t!("app.today.agent_permission", agent = a.kind, worktree = name)),
                _ => (EventKind::Alert, Severity::Normal, i18n::t!("app.today.agent_connection", agent = a.kind, worktree = name)),
            };
            let since = chrono::DateTime::from_timestamp(a.since, 0).unwrap_or(now);
            let mut e = Event::new(format!("agent-{}-{}", slug(&w.to_string_lossy()), slug(&who)), kind, title) // i18n-ok: event id
                .severity(severity)
                .when(since.to_rfc3339_opts(chrono::SecondsFormat::Secs, true))
                .link(&agent_link(w, a.session_id.as_deref()))
                .expires((now + chrono::Duration::minutes(TODAY_TTL_MIN)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true));
            e.id.truncate(200);
            out.push(e);
        }
    }
    out
}

/// Each window's latest events, by repository; the feed is their union.
static FEEDS: Mutex<BTreeMap<PathBuf, Vec<Event>>> = Mutex::new(BTreeMap::new());

fn publish_all(feeds: &BTreeMap<PathBuf, Vec<Event>>) {
    let mut all: Vec<Event> = Vec::new();
    for e in feeds.values().flatten() {
        if !all.iter().any(|x| x.id == e.id) {
            all.push(e.clone());
        }
    }
    let root = Root::system();
    std::thread::spawn(move || {
        let _ = if all.is_empty() { today::clear(&root, APP_ID) } else { today::publish(&root, APP_ID, &all) };
    });
}

/// After a poll in the window showing `repo`. Rewrites the file when something changed, and
/// at least every few minutes so the entries' expiry moves forward.
pub fn set_repo_events(repo: &Path, events: Vec<Event>) {
    let mut feeds = FEEDS.lock().unwrap_or_else(|e| e.into_inner());
    let strip = |v: &[Event]| v.iter().map(|e| (e.id.clone(), e.title.clone())).collect::<Vec<_>>();
    let old = feeds.get(repo).map(|v| strip(v)).unwrap_or_default();
    let stale = feeds.get(repo).and_then(|v| v.first()).and_then(|e| e.expires.as_deref()).and_then(|x| chrono::DateTime::parse_from_rfc3339(x).ok()).is_some_and(|x| x.with_timezone(&chrono::Utc) - chrono::Utc::now() < chrono::Duration::minutes(TODAY_TTL_MIN - 5));
    if old == strip(&events) && !stale {
        return;
    }
    if events.is_empty() {
        feeds.remove(repo);
    } else {
        feeds.insert(repo.to_path_buf(), events);
    }
    publish_all(&feeds);
}

/// The window stopped showing `repo` (closed, another repo, badges off).
pub fn forget_repo(repo: &Path) {
    let mut feeds = FEEDS.lock().unwrap_or_else(|e| e.into_inner());
    if feeds.remove(repo).is_some() {
        publish_all(&feeds);
    }
}

/// Quitting: nothing of ours stays in Today.
pub fn clear_today() {
    FEEDS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let _ = today::clear(&Root::system(), APP_ID);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> PathBuf {
        if cfg!(windows) { PathBuf::from(format!("C:{}", s.replace('/', "\\"))) } else { PathBuf::from(s) }
    }

    #[test]
    fn links_round_trip() {
        let repo = p("/Users/ana/code/rama ñ");
        assert_eq!(action(&Link::parse(&repo_link(&repo).to_url()).unwrap()), Some(Action::Repo(repo.clone())));
        assert_eq!(action(&Link::parse(&repo_link(&repo).app_url()).unwrap()), Some(Action::Repo(repo.clone())));
        let l = agent_link(&repo, Some("abc-123"));
        assert!(l.to_url().starts_with("otherwise://rama/agent?worktree="));
        assert_eq!(action(&Link::parse(&l.to_url()).unwrap()), Some(Action::Agent { worktree: repo.clone(), session: Some("abc-123".into()) }));
        assert_eq!(action(&agent_link(&repo, None)), Some(Action::Agent { worktree: repo, session: None }));
    }

    #[test]
    fn other_links() {
        let a = |s: &str| action(&Link::parse(s).unwrap());
        assert_eq!(a("otherwise://rama"), Some(Action::Focus));
        assert_eq!(a("otherwise-rama://entity/6f0c2c1e-0000-4000-8000-000000000001"), Some(Action::Focus));
        assert_eq!(a("otherwise://rama/repo?path=relative"), Some(Action::Focus));
        assert_eq!(a("otherwise://rama/agent"), Some(Action::Focus));
        assert_eq!(a("otherwise://diskmap/scan?path=/"), None);
        assert_eq!(arg_action("otherwise://diskmap"), None);
        assert_eq!(arg_action("/tmp/x"), Some(Action::Repo(PathBuf::from("/tmp/x"))));
        assert_eq!(arg_action("file:///tmp/a%20b"), Some(Action::Repo(PathBuf::from("/tmp/a b"))));
        assert!(arg_action("otherwise-rama://repo?path=%2Ftmp%2Fx").is_some());
    }

    fn agent(state: AgentState, need: Option<Need>, error: Option<ErrKind>, session: &str) -> Agent {
        Agent { kind: "claude", pid: Some(7), state, need, error, since: 1_763_700_000, started: None, last_activity: None, title: Some("secret title".into()), session_id: Some(session.into()), session_file: None, procs: 1, host_app: None }
    }

    #[test]
    fn only_agents_that_need_you_reach_today() {
        let now = chrono::DateTime::parse_from_rfc3339("2026-11-21T12:00:00Z").unwrap().with_timezone(&chrono::Utc);
        let mut found = Found::new();
        found.insert(
            p("/w/feature"),
            vec![
                agent(AgentState::NeedsYou, Some(Need::Question), None, "s1"),
                agent(AgentState::Working, None, None, "s2"),
                agent(AgentState::Error, None, Some(ErrKind::Connection), "s3"),
                agent(AgentState::Error, None, Some(ErrKind::RateLimit), "s4"),
                agent(AgentState::NeedsYou, Some(Need::Permission), None, "s5"),
            ],
        );
        let ev = agent_events(&found, now);
        assert_eq!(ev.len(), 3);
        assert_eq!(ev[0].kind, EventKind::Review);
        assert!(ev[0].id.starts_with("agent-") && ev[0].id.ends_with("-s1"), "{}", ev[0].id);
        assert_eq!(ev[1].kind, EventKind::Alert);
        assert!(ev.iter().all(|e| !e.title.contains("secret") && e.title.contains("feature")));
        assert_eq!(ev[0].expires.as_deref(), Some("2026-11-21T12:30:00Z"));
        let link = Link::parse(ev[0].link.as_deref().unwrap()).unwrap();
        assert_eq!(action(&link), Some(Action::Agent { worktree: p("/w/feature"), session: Some("s1".into()) }));
        // published and read back from a temp shared folder
        let dir = tempfile::tempdir().unwrap();
        let root = Root::at(dir.path());
        today::publish(&root, APP_ID, &ev).unwrap();
        let read = today::read_in(&root, now, &chrono::Utc);
        assert_eq!(read.events.len(), 3);
        assert!(read.events.iter().all(|e| e.app == APP_ID));
        let later = now + chrono::Duration::minutes(31);
        assert!(today::read_in(&root, later, &chrono::Utc).events.is_empty());
    }

    #[test]
    fn about_lists_other_installed_apps_only() {
        let dir = tempfile::tempdir().unwrap();
        let (opt, bin) = (dir.path().join("opt"), dir.path().join("bin"));
        for (folder, exe) in [("rama", "rama"), ("diskmap", "diskmap")] {
            std::fs::create_dir_all(opt.join(folder)).unwrap();
            std::fs::write(opt.join(folder).join(exe), "").unwrap();
        }
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("vigia"), "").unwrap();
        let loc = Locations { os: apps::Os::Linux, mac_dirs: vec![], roots: vec![opt], path: vec![bin] };
        let ids: Vec<String> = other_apps(&loc).into_iter().map(|(s, _)| s.id).collect();
        assert_eq!(ids, ["diskmap"]);
    }
}
