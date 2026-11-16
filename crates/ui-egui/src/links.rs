//! Browser links: the Help menu's project pages and the start screen's links. yorusphoto links to
//! its own repository and community; PhotoCraft, the open-source editor it is forked from, keeps a
//! link of its own (Help › PhotoCraft (upstream) on GitHub).

use serde_json::{Value, json};

use crate::PhotocraftApp;

/// The yorusphoto repository: source, releases and the guide.
pub const GITHUB: &str = "https://github.com/yorusd1/yorusphoto";
pub const ISSUES: &str = "https://github.com/yorusd1/yorusphoto/issues";
/// The project page the start screen's Website link and Help › yorusphoto Website open.
pub const APP_PAGE: &str = "https://github.com/yorusd1/yorusphoto#readme";
/// How to set up Codex, YorusAi Direct and the MCP bridge.
pub const TUTORIALS: &str = "https://github.com/yorusd1/yorusphoto/blob/main/docs/yorusphoto.md";
/// The YorusDevs community, where yorusphoto's AI workflows are shared.
pub const DISCORD: &str = "https://discord.gg/P9zKXJdWpj";
/// PhotoCraft, the project yorusphoto is forked from.
pub const UPSTREAM: &str = "https://github.com/storytold/photocraft";
/// Upstream's id for its sibling site; yorusphoto uses the slot for the upstream project itself.
pub const ARTCRAFT_WEBSITE: &str = UPSTREAM;

/// Help-menu link commands: (id, url). Labels live in the engine's shell command table.
pub const COMMANDS: &[(&str, &str)] =
    &[("help.discord", DISCORD), ("help.website", APP_PAGE), ("help.artcraftWebsite", UPSTREAM), ("help.github", GITHUB), ("help.reportIssue", ISSUES)];

pub fn url_for(id: &str) -> Option<&'static str> {
    COMMANDS.iter().find(|c| c.0 == id).map(|c| c.1)
}

/// Open `url` in the system browser (a new tab on the web) and note it in the status bar. Prefers
/// the platform `open_url` service (reliable on Windows/macOS/Linux); falls back to `ctx.open_url`.
pub fn open(app: &mut PhotocraftApp, ctx: &egui::Context, url: &str) -> Value {
    let opened = app.services.open_url.as_ref().map(|f| f(url).is_ok()).unwrap_or(false);
    if !opened {
        ctx.open_url(egui::OpenUrl::new_tab(url));
    }
    let template = "Opened {url}";
    app.ui.status = crate::i18n::fmt(tl!(template), &[("url", url)]);
    json!({"url": url})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn recording_app() -> (PhotocraftApp, Arc<Mutex<Vec<String>>>) {
        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let rec = opened.clone();
        let services = crate::Services {
            open_url: Some(Box::new(move |url| {
                rec.lock().unwrap().push(url.to_owned());
                Ok(())
            })),
            ..Default::default()
        };
        (PhotocraftApp::new(photocraft_engine::Session::new(), services), opened)
    }

    #[test]
    fn every_help_link_opens_the_fork_or_its_upstream_through_the_platform_service() {
        let (mut app, opened) = recording_app();
        let ctx = egui::Context::default();
        for (id, url) in COMMANDS {
            assert!(crate::menus::is_live(id) && crate::menus::is_enabled(&app, id), "{id}");
            crate::menus::invoke(&mut app, &ctx, id, json!({})).unwrap();
            assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some(*url), "{id}");
            assert!(url.contains("yorusphoto") || *url == UPSTREAM || *url == DISCORD, "{id}: {url}");
        }
        assert!(!COMMANDS.iter().any(|(_, url)| url.contains("artcraft")), "no upstream community promotion");
        open(&mut app, &ctx, TUTORIALS);
        assert_eq!(opened.lock().unwrap().last().map(String::as_str), Some(TUTORIALS));
        assert_eq!(app.ui.status, format!("Opened {TUTORIALS}"));
    }
}
