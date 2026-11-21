// No console window behind the app on Windows release builds.
#![cfg_attr(all(target_os = "windows", not(debug_assertions)), windows_subsystem = "windows")]

mod agents;
mod agents_view;
mod app;
mod cleanup;
mod convo_view;
mod detail;
mod diff_view;
mod graph_view;
mod launch;
mod map;
mod map_view;
mod otherwise;
mod robot;
mod settings;
mod star;
mod storage;
mod vcity;
mod sidebar;
mod theme;
mod transcript;
mod ui;
mod util;
mod work;
mod work_view;

use gpui::*;
use gpui_component::Root;
use gpui_component_assets::Assets;
use std::borrow::Cow;
use std::path::PathBuf;

actions!(rama, [Quit, OpenWebsite, OpenVcity, RevealData]);

pub(crate) const APP_NAME: &str = "rama"; // i18n-ok: product name
pub(crate) const COMPANY: &str = "Otherwise Technologies LLC"; // i18n-ok: company name
pub(crate) const COMPANY_URL: &str = "https://otherwisetech.com";
pub(crate) const REPO_URL: &str = "https://github.com/otherwisetech/rama-rs";
pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The native menu bar holds plain strings, so it is rebuilt on every language switch.
pub(crate) fn set_app_menus(cx: &mut App) {
    let items = vec![
        MenuItem::action(i18n::t!("app.about.menu", app = APP_NAME), app::ShowAbout),
        MenuItem::action(i18n::t!("app.credit.menu", company = COMPANY), OpenWebsite),
        MenuItem::separator(),
        MenuItem::action(i18n::t!("app.menu.reveal_data"), RevealData),
        MenuItem::separator(),
        MenuItem::action(i18n::t!("app.menu.quit"), Quit),
    ];
    let help = vec![MenuItem::action(vcity::menu_label(), OpenVcity)];
    cx.set_menus(vec![Menu { name: APP_NAME.into(), items, disabled: false }, Menu { name: i18n::t!("app.menu.help").into(), items: help, disabled: false }]);
}

/// gpui-component's icon set plus our own `icons/*.svg`.
struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        match path {
            LOGO_SVG => Ok(Some(Cow::Borrowed(include_bytes!("../assets/icons/rama-logo.svg")))),
            _ => Assets.load(path),
        }
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Assets.list(path)
    }
}

pub(crate) const LOGO_SVG: &str = "icons/rama-logo.svg";
pub(crate) static ICON_PNG: &[u8] = include_bytes!("../assets/logo/rama-icon-512.png");

/// Sets the Dock icon at runtime: covers `cargo run` (no bundle) and stale Dock icon caches.
#[cfg(target_os = "macos")]
#[allow(unexpected_cfgs)] // objc 0.2 macros check a `cargo-clippy` feature
fn set_dock_icon() {
    use objc::runtime::Object;
    use objc::{class, msg_send, sel, sel_impl};
    static PNG: &[u8] = include_bytes!("../assets/logo/rama-icon-1024.png");
    // SAFETY: plain AppKit calls on the main thread with a valid byte buffer that NSData copies.
    unsafe {
        let data: *mut Object = msg_send![class!(NSData), dataWithBytes: PNG.as_ptr() length: PNG.len()];
        let image: *mut Object = msg_send![class!(NSImage), alloc];
        let image: *mut Object = msg_send![image, initWithData: data];
        if image.is_null() {
            return;
        }
        let app: *mut Object = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, setApplicationIconImage: image];
        let _: () = msg_send![image, release];
    }
}

const FONTS: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
];

fn main() {
    storage::migrate_on_startup();
    i18n::set_locale(settings::initial_language(&settings::Settings::load()));
    let arg = std::env::args().nth(1);
    if matches!(arg.as_deref(), Some("-h" | "--help")) {
        println!("{}", i18n::t!("app.cli.usage"));
        return;
    }
    let initial: Option<PathBuf> = match arg.as_deref().and_then(otherwise::arg_action) {
        Some(otherwise::Action::Repo(p)) => Some(p),
        Some(a) => {
            otherwise::push(a);
            None
        }
        None => None,
    };
    otherwise::register_scheme();
    // hooks need the login shell's PATH (node, npx, pnpm…); resolve it while the window opens
    repo::shell_env::init();
    let application = gpui_platform::application().with_assets(AppAssets);
    application.on_open_urls(otherwise::push_urls);
    application.run(move |cx| {
        if let Err(e) = cx.text_system().add_fonts(FONTS.iter().map(|f| Cow::Borrowed(*f)).collect()) {
            eprintln!("fonts: {e}");
        }
        #[cfg(target_os = "macos")]
        set_dock_icon();
        gpui_component::init(cx);
        app::sync_theme(None, cx);
        app::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("secondary-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &OpenWebsite, cx| cx.open_url(COMPANY_URL));
        cx.on_action(|_: &OpenVcity, cx| vcity::open(cx));
        cx.on_action(|_: &RevealData, cx| storage::reveal_data_dir(cx));
        gpui_component::set_locale(i18n::locale());
        set_app_menus(cx);
        // "Open in rama" opens more windows; quit when the last one closes
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                otherwise::clear_today();
                cx.quit();
            }
        })
        .detach();
        open_window(initial.clone(), cx);
        pump_links(cx);
    });
}

/// RamaApp in each open window.
fn views(cx: &mut App) -> Vec<(AnyWindowHandle, Entity<app::RamaApp>)> {
    cx.windows()
        .into_iter()
        .filter_map(|h| {
            let root = h.downcast::<Root>()?;
            let view = root.read(cx).ok()?.view().clone().downcast::<app::RamaApp>().ok()?;
            Some((h, view))
        })
        .collect()
}

/// Handles links from the OS every 300 ms, and lets windows finish a pending "open this
/// agent's conversation" once their agent poll found it.
fn pump_links(cx: &mut App) {
    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(std::time::Duration::from_millis(300)).await;
            let actions = otherwise::take();
            cx.update(|cx| {
                for a in actions {
                    handle_link(a, cx);
                }
                for (h, view) in views(cx) {
                    h.update(cx, |_, window, cx| view.update(cx, |this, cx| this.try_pending_convo(window, cx))).ok();
                }
            });
        }
    })
    .detach();
}

fn handle_link(a: otherwise::Action, cx: &mut App) {
    let target = match &a {
        otherwise::Action::Focus => None,
        otherwise::Action::Repo(p) => Some(p.clone()),
        otherwise::Action::Agent { worktree, .. } => Some(worktree.clone()),
    };
    let windows = views(cx);
    let hit = match &target {
        Some(p) => windows.iter().find(|(_, v)| v.read(cx).covers(p)).cloned(),
        None => windows.first().cloned(),
    };
    cx.activate(true);
    match hit {
        Some((h, view)) => {
            h.update(cx, |_, window, cx| {
                window.activate_window();
                if let otherwise::Action::Agent { worktree, session } = a {
                    view.update(cx, |this, cx| this.request_convo(worktree, session, cx));
                }
            })
            .ok();
        }
        None => match a {
            otherwise::Action::Agent { worktree, session } => open_window_with(Some(worktree.clone()), Some((worktree, session)), cx),
            _ => open_window(target, cx),
        },
    }
}

/// A new window, on `path`'s repository if given.
pub(crate) fn open_window(path: Option<PathBuf>, cx: &mut App) {
    open_window_with(path, None, cx);
}

/// …and then the conversation of the agent in `convo`'s worktree.
fn open_window_with(path: Option<PathBuf>, convo: Option<(PathBuf, Option<String>)>, cx: &mut App) {
    let window_options = WindowOptions {
        window_bounds: Some(WindowBounds::centered(size(px(1440.), px(900.)), cx)),
        titlebar: Some(TitlebarOptions { title: Some(APP_NAME.into()), ..Default::default() }),
        ..Default::default()
    };
    cx.spawn(async move |cx| {
        cx.open_window(window_options, |window, cx| {
            let view = cx.new(|cx| {
                let mut this = app::RamaApp::new(path, window, cx);
                if let Some((worktree, session)) = convo {
                    this.request_convo(worktree, session, cx);
                }
                this
            });
            cx.new(|cx| Root::new(view, window, cx))
        })
        .expect("open window");
    })
    .detach();
}
