//! A short invitation to Vcity (online stores for businesses), in «Acerca de» and the Help menu.
//! The link only says which app it came from; nothing about the user.
use gpui::App;

const CAMPAIGN: &str = "otherwise-apps"; // i18n-ok: URL parameter

/// `https://vcity.io/?utm_source=<app>&utm_medium=app&utm_campaign=otherwise-apps`.
pub fn url(app: &str) -> String {
    format!("https://vcity.io/?utm_source={app}&utm_medium=app&utm_campaign={CAMPAIGN}") // i18n-ok: URL
}

pub fn open(cx: &mut App) {
    cx.open_url(&url(crate::otherwise::APP_ID));
}

/// «Crea tu tienda en Vcity», for the Help menu and the About link.
pub fn menu_label() -> String {
    i18n::t!("app.vcity.menu")
}

#[cfg(test)]
mod tests {
    #[test]
    fn link_names_only_the_app() {
        assert_eq!(super::url("rama"), "https://vcity.io/?utm_source=rama&utm_medium=app&utm_campaign=otherwise-apps");
    }

    #[test]
    fn copy_in_every_language_keeps_the_name() {
        for lang in ["es", "en", "pt", "fr"] {
            let ftl = std::fs::read_to_string(format!("{}/../i18n/locales/{lang}/app.ftl", env!("CARGO_MANIFEST_DIR"))).unwrap();
            for key in ["app.vcity.menu", "app.vcity.pitch"] {
                let line = ftl.lines().find(|l| l.starts_with(&format!("{key} ="))).unwrap_or_else(|| panic!("{lang}: {key}"));
                assert!(line.contains("Vcity"), "{lang}: {line}");
            }
        }
    }
}
