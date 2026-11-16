//! Transient account identity. Never serialize this into preferences, chats or inspection.

use super::{CodexWorkspaceAction, Ordering, PhotocraftApp, Value, chats, request_workspace, start_login, status};

#[derive(Default)]
pub(super) struct Session {
    pub observed: bool,
    pub stale: bool,
    pub account_type: Option<String>,
    pub email: Option<String>,
    pub requires_auth: Option<bool>,
}

impl Session {
    pub fn kind(&self) -> &str {
        if !self.observed {
            "unknown"
        } else {
            match self.account_type.as_deref() {
                None => "signed_out",
                Some("chatgpt") => "chatgpt",
                Some("apiKey") => "api_key",
                Some(_) => "other",
            }
        }
    }

    pub fn connected(&self) -> bool {
        self.account_type.as_deref() == Some("chatgpt")
    }

    pub fn confirm_login(&mut self) {
        self.observed = true;
        self.stale = true;
        self.account_type = Some("chatgpt".into());
        self.email = None;
    }

    pub fn read(data: &Value) -> Result<Option<Self>, String> {
        let Some(account) = data.get("account") else { return Ok(None) };
        let text = |value: Option<&Value>, max: usize| -> Result<Option<String>, String> {
            match value {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(value)) if value.len() <= max && !value.chars().any(char::is_control) => Ok((!value.is_empty()).then(|| value.clone())),
                _ => Err("Codex returned invalid account details. Refresh to try again.".into()),
            }
        };
        let account_type = if account.is_null() {
            None
        } else if account.is_object() {
            Some(text(account.get("type"), 64)?.ok_or("Codex did not identify the account type.")?)
        } else {
            return Err("Codex returned invalid account details. Refresh to try again.".into());
        };
        let identity = data.get("accountIdentity");
        if identity.is_some_and(|value| !value.is_null() && !value.is_object()) {
            return Err("Codex returned invalid account details. Refresh to try again.".into());
        }
        let email = text(identity.and_then(|value| value.get("email")), 512)?;
        let requires_auth = match identity.and_then(|value| value.get("requiresOpenaiAuth")) {
            None | Some(Value::Null) => None,
            Some(Value::Bool(value)) => Some(*value),
            _ => return Err("Codex returned invalid account details. Refresh to try again.".into()),
        };
        Ok(Some(Self { observed: true, stale: false, email: account_type.as_ref().and(email), account_type, requires_auth }))
    }
}

pub(super) fn clear_allowances(app: &mut PhotocraftApp) {
    let workspace = &mut app.codex_runtime.workspace;
    workspace.rate_limits.clear();
    workspace.usage_observed_at_unix = None;
    workspace.reset_credits_available = None;
    workspace.ordinary_usage_allowed = None;
}

pub(super) fn apply(app: &mut PhotocraftApp, data: &Value, now: u64) -> Result<bool, String> {
    let next = match Session::read(data) {
        Ok(Some(next)) => next,
        Ok(None) => {
            app.codex_runtime.account.stale = true;
            return Ok(false);
        }
        Err(error) => {
            app.codex_runtime.account.stale = true;
            return Err(error);
        }
    };
    // Validate the public account view before committing either half of the observation.
    if let Err(error) = app.codex_runtime.workspace.apply_account(data.get("account").unwrap_or(&Value::Null), now) {
        app.codex_runtime.account.stale = true;
        return Err(error);
    }
    let previous = &app.codex_runtime.account;
    if previous.account_type != next.account_type || previous.email != next.email || next.account_type.is_none() {
        clear_allowances(app);
    }
    let has_account = next.account_type.is_some();
    app.codex_runtime.account = next;
    Ok(has_account)
}

pub(super) fn can_change(app: &PhotocraftApp) -> bool {
    !app.codex_runtime.busy()
        && app.codex_runtime.login_rx.is_none()
        && (app.codex_runtime.workspace_rx.is_none() || matches!(app.codex_runtime.workspace_action, Some(CodexWorkspaceAction::Refresh)))
        && chats::can_switch(app)
}

/// Discard a read-only refresh before changing authentication. Its late replies cannot
/// restore the previous identity or usage after login/logout.
pub(super) fn retire_refresh(app: &mut PhotocraftApp) {
    if matches!(app.codex_runtime.workspace_action, Some(CodexWorkspaceAction::Refresh)) {
        if let Some(cancel) = app.codex_runtime.workspace_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
        app.codex_runtime.workspace_rx = None;
        app.codex_runtime.workspace_action = None;
    }
}

pub(super) fn logout(app: &mut PhotocraftApp, switch: bool) -> Result<(), String> {
    if !can_change(app) {
        return Err("Finish the current Codex task and save or place its results before changing accounts.".into());
    }
    if switch && app.services.codex_login.is_none() {
        return Err("The Codex sign-in service is unavailable.".into());
    }
    retire_refresh(app);
    request_workspace(app, CodexWorkspaceAction::Logout)?;
    app.codex_runtime.switch_after_logout = switch;
    status(app, if switch { "Switching ChatGPT account…" } else { "Signing out of Codex…" }, false);
    Ok(())
}

pub(super) fn ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.set_min_width(265.0);
    ui.set_max_width(340.0);
    ui.strong("ChatGPT account");
    let session = &app.codex_runtime.account;
    match session.kind() {
        "chatgpt" => {
            ui.strong("Signed in with ChatGPT");
            if let Some(email) = &session.email {
                ui.add(egui::Label::new(email).wrap());
            } else {
                ui.small("Codex has not provided an email address.");
            }
            if let Some(plan) = app.codex_runtime.workspace.account.as_ref().and_then(|account| account.plan_type.as_deref()) {
                ui.label(format!("Plan: {plan}"));
            }
        }
        "signed_out" => {
            ui.label(if session.requires_auth == Some(false) {
                "No ChatGPT account connected. The active provider does not require OpenAI sign-in."
            } else {
                "Signed out of ChatGPT"
            });
        }
        "api_key" => {
            ui.label("Connected with an API key");
        }
        "other" => {
            ui.label("Connected through another Codex provider");
        }
        _ => {
            ui.label(if app.codex_runtime.workspace_rx.is_some() { "Checking Codex account…" } else { "Account status unavailable" });
        }
    }
    if session.stale && session.observed {
        ui.small("Last confirmed account. Refresh to verify its current status.");
    }
    if let Some(error) = &app.codex_runtime.workspace_error {
        ui.add(egui::Label::new(error).wrap());
    }
    let signed_in = session.account_type.is_some();
    let enabled = can_change(app) && app.services.codex_workspace.is_some();
    if signed_in {
        ui.horizontal_wrapped(|ui| {
            if ui.add_enabled(enabled && app.services.codex_login.is_some(), egui::Button::new("Switch account")).clicked()
                && let Err(error) = logout(app, true)
            {
                status(app, error, true);
            }
            if ui.add_enabled(enabled, egui::Button::new("Sign out")).clicked()
                && let Err(error) = logout(app, false)
            {
                status(app, error, true);
            }
        });
        ui.small("Sign-out also affects other Codex clients using the same Codex home. Saved chats stay on this device.");
    } else if ui
        .add_enabled(
            enabled && app.services.codex_login.is_some() && (session.observed || app.codex_runtime.workspace_rx.is_none()),
            egui::Button::new("Continue with ChatGPT"),
        )
        .clicked()
        && let Err(error) = start_login(app)
    {
        status(app, error, true);
    }
    if ui.add_enabled(enabled && app.codex_runtime.workspace_rx.is_none(), egui::Button::new("Refresh account")).clicked()
        && let Err(error) = request_workspace(app, CodexWorkspaceAction::Refresh)
    {
        status(app, error, true);
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Arc, CodexLoginEvent, CodexWorkspaceEvent, apply_workspace_result, json, menu, poll, save_conversation, workspace_inspect};
    use super::*;

    fn signed_in(app: &mut PhotocraftApp) {
        apply_workspace_result(
            app,
            Some(CodexWorkspaceAction::Refresh),
            &json!({
                "account":{"type":"chatgpt","planType":"pro"},"accountIdentity":{"email":"demo@example.invalid","requiresOpenaiAuth":true},
                "rateLimits":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":85}}}
            }),
        )
        .unwrap();
    }

    #[test]
    fn detected_account_is_connected_without_clicking_login_and_identity_is_transient() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        signed_in(&mut app);
        assert!(app.codex_runtime.account.connected());
        assert_eq!(app.codex_runtime.account.email.as_deref(), Some("demo@example.invalid"));
        assert_eq!(app.codex_runtime.workspace.account.as_ref().unwrap().plan_type.as_deref(), Some("pro"));
        assert!(!serde_json::to_string(&app.ui).unwrap().contains("demo@example.invalid"));
        assert!(!workspace_inspect(&app).to_string().contains("demo@example.invalid"));
        assert!(!app.codex_runtime.connection().to_string().contains("demo@example.invalid"));
        save_conversation(&mut app);
        assert!(!app.codex_runtime.history_snapshot.as_deref().unwrap_or_default().contains("demo@example.invalid"));
    }

    #[test]
    fn unavailable_read_keeps_last_account_but_confirmed_null_clears_identity_and_limits() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        signed_in(&mut app);
        apply_workspace_result(&mut app, Some(CodexWorkspaceAction::Refresh), &json!({"errors":["Account unavailable"]})).unwrap();
        assert!(app.codex_runtime.account.connected());
        assert!(app.codex_runtime.account.stale);
        assert!(!app.codex_runtime.workspace.rate_limits.is_empty());
        apply_workspace_result(&mut app, Some(CodexWorkspaceAction::Refresh), &json!({"account":null})).unwrap();
        assert_eq!(app.codex_runtime.account.kind(), "signed_out");
        assert!(app.codex_runtime.account.email.is_none());
        assert!(app.codex_runtime.workspace.account.is_none());
        assert!(app.codex_runtime.workspace.rate_limits.is_empty());
        assert!(app.codex_runtime.workspace.usage_observed_at_unix.is_none());
    }

    #[test]
    fn logout_retires_old_refresh_and_preserves_chat_and_retained_placement_rules() {
        let held = Arc::new(std::sync::Mutex::new(Vec::new()));
        let pending = held.clone();
        let mut app = PhotocraftApp::new(
            Default::default(),
            crate::Services { codex_workspace: Some(Box::new(move |request, reply| pending.lock().unwrap().push((request, reply)))), ..Default::default() },
        );
        signed_in(&mut app);
        app.ui.codex.chat_prompt = "unsent draft".into();
        app.ui.codex.thread_id = Some("saved-thread".into());
        request_workspace(&mut app, CodexWorkspaceAction::Refresh).unwrap();
        logout(&mut app, false).unwrap();
        let mut pending = held.lock().unwrap();
        let (old, old_reply) = pending.remove(0);
        assert!(old.cancel.load(Ordering::Relaxed));
        assert!(old_reply.send(CodexWorkspaceEvent::Result(json!({"account":{"type":"chatgpt"}}))).is_err());
        let (request, reply) = pending.remove(0);
        assert!(matches!(request.action, CodexWorkspaceAction::Logout));
        reply.send(CodexWorkspaceEvent::Result(json!({"loggedOut":true}))).unwrap();
        drop(pending);
        poll(&mut app, &egui::Context::default());
        assert_eq!(app.codex_runtime.account.kind(), "signed_out");
        assert_eq!(app.ui.codex.chat_prompt, "unsent draft");
        assert_eq!(app.ui.codex.thread_id.as_deref(), Some("saved-thread"));
        app.codex_runtime.pending_image = Some(vec![1]);
        assert!(logout(&mut app, false).is_err());
        assert!(app.codex_runtime.pending_image.is_some());
    }

    #[test]
    fn failed_logout_keeps_account_and_never_starts_switch_login() {
        let mut app = PhotocraftApp::new(
            Default::default(),
            crate::Services {
                codex_workspace: Some(Box::new(|_, reply| {
                    reply.send(CodexWorkspaceEvent::Error("Could not sign out".into())).unwrap();
                })),
                codex_login: Some(Box::new(|_, _, _| panic!("failed logout must not start login"))),
                ..Default::default()
            },
        );
        signed_in(&mut app);
        logout(&mut app, true).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(app.codex_runtime.account.connected());
        assert!(app.codex_runtime.account.stale);
        assert!(!app.codex_runtime.switch_after_logout);
        assert!(app.ui.codex.error);
    }

    #[test]
    fn switch_waits_for_logout_confirmation_before_starting_login() {
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let calls = count.clone();
        let mut app = PhotocraftApp::new(
            Default::default(),
            crate::Services {
                codex_workspace: Some(Box::new(|request, reply| {
                    assert!(matches!(request.action, CodexWorkspaceAction::Logout));
                    reply.send(CodexWorkspaceEvent::Result(json!({"loggedOut":true}))).unwrap();
                })),
                codex_login: Some(Box::new(move |_, _, reply| {
                    calls.fetch_add(1, Ordering::Relaxed);
                    reply.send(CodexLoginEvent::Connected).unwrap();
                })),
                ..Default::default()
            },
        );
        signed_in(&mut app);
        logout(&mut app, true).unwrap();
        assert_eq!(count.load(Ordering::Relaxed), 0);
        poll(&mut app, &egui::Context::default());
        assert_eq!(count.load(Ordering::Relaxed), 1);
        assert_eq!(app.codex_runtime.account.kind(), "signed_out");
        poll(&mut app, &egui::Context::default());
        assert!(app.codex_runtime.account.connected());
        assert!(app.codex_runtime.account.email.is_none());
    }

    #[test]
    fn malformed_account_never_replaces_a_confirmed_identity_or_stays_fresh() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        signed_in(&mut app);
        assert!(apply(&mut app, &json!({"account":{"type":"chatgpt","planType":42}}), 100).is_err());
        assert!(app.codex_runtime.account.stale);
        assert_eq!(app.codex_runtime.account.email.as_deref(), Some("demo@example.invalid"));
        assert_eq!(app.codex_runtime.workspace.account.as_ref().unwrap().plan_type.as_deref(), Some("pro"));
    }

    #[test]
    fn malformed_commands_cannot_change_authentication() {
        let mut app = PhotocraftApp::new(
            Default::default(),
            crate::Services {
                codex_workspace: Some(Box::new(|_, _| panic!("invalid command must not reach account service"))),
                codex_login: Some(Box::new(|_, _, _| panic!("invalid command must not reach login service"))),
                ..Default::default()
            },
        );
        for command in ["codex.account", "codex.login", "codex.logout", "codex.account.switch"] {
            for params in [json!(null), json!([]), json!({"unexpected":true})] {
                assert!(menu(&mut app, command, &params).unwrap().is_err());
            }
        }
    }

    #[test]
    fn null_missing_and_other_provider_are_distinct() {
        assert!(Session::read(&json!({})).unwrap().is_none());
        assert_eq!(Session::read(&json!({"account":null})).unwrap().unwrap().kind(), "signed_out");
        assert_eq!(Session::read(&json!({"account":{"type":"apiKey"}})).unwrap().unwrap().kind(), "api_key");
        for data in [
            json!({"account":{"type":"chatgpt"},"accountIdentity":42}),
            json!({"account":42}),
            json!({"account":{}}),
            json!({"account":{"type":"chatgpt"},"accountIdentity":{"email":"bad\nidentity"}}),
        ] {
            assert!(Session::read(&data).is_err());
        }
    }
}
