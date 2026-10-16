//! Browser navigation for explicit links, such as original contributor credits.
//! Product help is local: this build has no configured product website or community URL.

use serde_json::{Value, json};

use crate::PhotocraftApp;

/// Open an explicit URL through the platform service, falling back to egui's browser request.
/// This helper is also used by the legal contributor credits; it supplies no product URLs.
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

    #[test]
    fn explicit_credit_links_open_through_the_platform_service() {
        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let rec = opened.clone();
        let services = crate::Services {
            open_url: Some(Box::new(move |url| {
                rec.lock().unwrap().push(url.to_owned());
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let url = "https://example.org/licenses";
        open(&mut app, &egui::Context::default(), url);
        assert_eq!(*opened.lock().unwrap(), [url]);
        assert_eq!(app.ui.status, format!("Opened {url}"));
    }

    #[test]
    fn help_menu_contains_only_local_product_information() {
        let app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        let help: Vec<_> = crate::menus::menu_items(&app).into_iter().filter(|item| item.path == ["Help"]).map(|item| item.id).collect();
        assert_eq!(help, ["help.systemInfo", "help.about"]);
        assert!(crate::menus::is_enabled(&app, "help.about"));
        assert!(crate::menus::is_enabled(&app, "help.systemInfo"));
    }

    #[test]
    fn removed_community_commands_cannot_open_an_external_destination() {
        let opened = Arc::new(Mutex::new(Vec::<String>::new()));
        let rec = opened.clone();
        let services = crate::Services {
            open_url: Some(Box::new(move |url| {
                rec.lock().unwrap().push(url.to_owned());
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let ctx = egui::Context::default();
        for id in ["help.discord", "help.website", "help.artcraftWebsite", "help.github", "help.reportIssue"] {
            assert!(!crate::menus::is_live(id), "{id}");
            assert!(crate::menus::invoke(&mut app, &ctx, id, json!({})).is_err(), "{id}");
        }
        assert!(opened.lock().unwrap().is_empty());
    }
}
