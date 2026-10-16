//! Runtime updates use native workers; versioned activation never cancels an editor task.

use super::*;

pub struct CodexUpdateRequest {
    pub force: bool,
    pub configured_path: String,
    pub cancel: Arc<AtomicBool>,
}

pub enum CodexUpdateEvent {
    Status(Value),
    Error(String),
}

pub type CodexUpdateFn = Box<dyn Fn(CodexUpdateRequest, Sender<CodexUpdateEvent>)>;

#[derive(Default)]
pub(super) struct Runtime {
    rx: Option<Receiver<CodexUpdateEvent>>,
    cancel: Option<Arc<AtomicBool>>,
    status: Value,
    error: Option<String>,
    next_check: f64,
    show: bool,
}

impl Runtime {
    pub(super) fn running(&self) -> bool {
        self.rx.is_some()
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

pub(super) fn inspect(app: &PhotocraftApp) -> Value {
    json!({"automatic":app.ui.codex.automatic_codex_updates,"running":app.codex_runtime.updates.running(),
        "status":app.codex_runtime.updates.status,"error":app.codex_runtime.updates.error,
        "customExecutable":!app.ui.codex.codex_path.trim().is_empty(),"available":app.services.codex_update.is_some()})
}

fn request(app: &mut PhotocraftApp, force: bool) -> Result<(), String> {
    if app.codex_runtime.updates.running() {
        return Err("A Codex update check is already running.".into());
    }
    let run = app.services.codex_update.as_ref().ok_or("Codex updates require the native app.")?;
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    run(CodexUpdateRequest { force, configured_path: app.ui.codex.codex_path.clone(), cancel: cancel.clone() }, tx);
    app.codex_runtime.updates.rx = Some(rx);
    app.codex_runtime.updates.cancel = Some(cancel);
    app.codex_runtime.updates.error = None;
    Ok(())
}

pub(super) fn restore_preferences(app: &mut PhotocraftApp) {
    let Some(text) = app.services.load_prefs.as_mut().and_then(|load| load()) else { return };
    let saved = serde_json::from_str::<Value>(&text).ok().filter(Value::is_object);
    let setting = saved.as_ref().and_then(|prefs| prefs.pointer("/yorusphotoCodexUpdates/automatic"));
    if let Some(enabled) = setting.and_then(Value::as_bool) {
        app.ui.codex.automatic_codex_updates = enabled;
    } else if saved.is_none() || setting.is_some() {
        app.ui.codex.automatic_codex_updates = false;
        app.codex_runtime.updates.error = Some("Automatic update preferences could not be read. Use Check now or choose the setting again.".into());
    }
}

fn set_automatic(app: &mut PhotocraftApp, enabled: bool) -> Result<(), String> {
    if app.services.save_prefs.is_some() {
        let stored = app.services.load_prefs.as_mut().and_then(|load| load());
        let mut value = match stored {
            Some(text) => serde_json::from_str::<Value>(&text).map_err(|_| "The existing preference file is invalid; it was not replaced.")?,
            None => app.session.prefs_value(),
        };
        if !value.is_object() {
            return Err("The existing preference file is invalid; it was not replaced.".into());
        }
        value["yorusphotoCodexUpdates"] = json!({"automatic":enabled});
        let text = serde_json::to_string_pretty(&value).map_err(|_| "Could not encode automatic update preferences.")?;
        if let Some(save) = app.services.save_prefs.as_mut() {
            save(&text)?;
        }
    }
    app.ui.codex.automatic_codex_updates = enabled;
    app.codex_runtime.updates.next_check = 0.0;
    if !enabled && let Some(cancel) = &app.codex_runtime.updates.cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    Ok(())
}

pub(super) fn poll(app: &mut PhotocraftApp, now: f64) {
    // Each worker sends a bounded number of status transitions before disconnecting.
    for _ in 0..32 {
        let event = app.codex_runtime.updates.rx.as_ref().map(Receiver::try_recv);
        match event {
            Some(Ok(CodexUpdateEvent::Status(value))) => {
                if value.is_object() && value.to_string().len() <= 32 << 10 {
                    if value.get("updated").and_then(Value::as_bool) == Some(true) {
                        app.codex_runtime.next_workspace_refresh = 0.0;
                    }
                    app.codex_runtime.updates.status = value;
                } else {
                    app.codex_runtime.updates.error = Some("Codex returned invalid update status.".into());
                }
            }
            Some(Ok(CodexUpdateEvent::Error(error))) => {
                app.codex_runtime.updates.error = Some(error.chars().take(2048).collect());
            }
            Some(Err(TryRecvError::Disconnected)) => {
                app.codex_runtime.updates.rx = None;
                app.codex_runtime.updates.cancel = None;
                // Disk-backed throttling in the native worker also survives app restarts.
                app.codex_runtime.updates.next_check = now + 3600.0;
                break;
            }
            Some(Err(TryRecvError::Empty)) | None => break,
        }
    }
    if app.ui.codex.automatic_codex_updates
        && app.services.codex_update.is_some()
        && !app.codex_runtime.updates.running()
        && !app.codex_runtime.busy()
        && app.codex_runtime.login_rx.is_none()
        && now >= app.codex_runtime.updates.next_check
    {
        app.codex_runtime.updates.next_check = now + 3600.0;
        if let Err(error) = request(app, false) {
            app.codex_runtime.updates.error = Some(error);
        }
    }
}

pub(super) fn settings_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.strong("Codex runtime updates");
    let mut enabled = app.ui.codex.automatic_codex_updates;
    if ui.checkbox(&mut enabled, "Automatically install stable Codex updates").changed()
        && let Err(error) = set_automatic(app, enabled)
    {
        app.codex_runtime.updates.error = Some(error);
    }
    ui.small("Checks daily. New versions apply to the next Codex task.");
    let report = &app.codex_runtime.updates.status;
    if let Some(version) = report.get("activeVersion").and_then(Value::as_str) {
        ui.label(format!("Active Codex: {version}"));
    }
    if let Some(version) = report.get("latestVersion").and_then(Value::as_str) {
        ui.small(format!("Latest stable: {version}"));
    }
    if let Some(checked) = report.get("lastCheckUnix").and_then(Value::as_u64) {
        ui.small(format!("Last check: {}", workspace::freshness(Some(checked), unix_now())));
    }
    if let Some(message) = report.get("message").and_then(Value::as_str) {
        ui.add(egui::Label::new(message).wrap());
    }
    if let Some(error) = &app.codex_runtime.updates.error {
        ui.colored_label(crate::ai_sidebar::palette(ui).warning, error);
        ui.small("Your previous runtime remains available.");
    }
    ui.horizontal(|ui| {
        if ui.add_enabled(!app.codex_runtime.updates.running() && app.services.codex_update.is_some(), egui::Button::new("Check now")).clicked()
            && let Err(error) = request(app, true)
        {
            app.codex_runtime.updates.error = Some(error);
        }
        if app.codex_runtime.updates.running() {
            ui.spinner();
        }
    });
    if !app.ui.codex.codex_path.trim().is_empty() {
        ui.small("A custom executable is selected. Managed updates will not replace it.");
    }
}

pub(super) fn window(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if !app.codex_runtime.updates.show {
        return;
    }
    let mut open = true;
    egui::Window::new("Codex updates").id(egui::Id::new("codex-updates")).open(&mut open).default_width(420.0).show(ctx, |ui| {
        settings_ui(app, ui);
    });
    app.codex_runtime.updates.show = open;
}

pub(super) fn specs(app: &PhotocraftApp) -> Vec<Value> {
    [
        ("codex.updates", "Inspect or configure automatic Codex runtime updates", "{automatic?:bool,open?:bool}", true),
        (
            "codex.update",
            "Check and install the latest stable Codex runtime",
            "{}; asynchronous; inspect codexWorkspace.updates",
            app.services.codex_update.is_some() && !app.codex_runtime.updates.running(),
        ),
    ]
    .into_iter()
    .map(|(id, label, params, enabled)| {
        json!({"id":id,"label":label,"params":params,"menu":[],"shortcut":null,
        "enabled":enabled,"disabledReason":if enabled {Value::Null} else {json!("A native update service must be available and idle")},"needsDocument":false})
    })
    .collect()
}

pub(super) fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(id, "codex.updates" | "codex.update") {
        return None;
    }
    Some((|| {
        let values = params.as_object().ok_or("Update parameters must be an object.")?;
        if id == "codex.update" {
            if !values.is_empty() {
                return Err("codex.update takes no parameters.".into());
            }
            request(app, true)?;
            return Ok(inspect(app));
        }
        // Validate every field before changing preferences.
        for (key, value) in values {
            if !matches!(key.as_str(), "automatic" | "open") || !value.is_boolean() {
                return Err("Use optional boolean automatic and open parameters.".into());
            }
        }
        if let Some(enabled) = params.get("automatic").and_then(Value::as_bool) {
            set_automatic(app, enabled)?;
        }
        if let Some(open) = params.get("open").and_then(Value::as_bool) {
            app.codex_runtime.updates.show = open;
        }
        Ok(inspect(app))
    })())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn automatic_default_migrates_and_explicit_disable_persists() {
        let state: State = serde_json::from_value(json!({"open":true})).unwrap();
        assert!(state.automatic_codex_updates);
        let state: State = serde_json::from_value(json!({"automatic_codex_updates":false})).unwrap();
        assert!(!state.automatic_codex_updates);
    }

    #[test]
    fn invalid_update_parameters_do_not_mutate_preferences() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        assert!(menu(&mut app, "codex.updates", &json!({"automatic":false,"open":"yes"})).unwrap().is_err());
        assert!(app.ui.codex.automatic_codex_updates);
        assert!(menu(&mut app, "codex.update", &json!({"url":"https://example.invalid"})).unwrap().is_err());
    }

    #[test]
    fn disabling_cancels_check_without_changing_chats_or_image_destination() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.ui.codex.thread_id = Some("saved-thread".into());
        app.ui.codex.entire_image = true;
        let cancel = Arc::new(AtomicBool::new(false));
        app.codex_runtime.updates.cancel = Some(cancel.clone());
        menu(&mut app, "codex.updates", &json!({"automatic":false})).unwrap().unwrap();
        assert!(cancel.load(Ordering::Relaxed));
        assert_eq!(app.ui.codex.thread_id.as_deref(), Some("saved-thread"));
        assert!(app.ui.codex.entire_image);
    }

    #[test]
    fn finished_update_refreshes_catalog_and_never_cancels_active_turn() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let (tx, rx) = mpsc::channel();
        let cancel = Arc::new(AtomicBool::new(false));
        app.codex_runtime.cancel = Some(cancel.clone());
        app.codex_runtime.updates.rx = Some(rx);
        app.codex_runtime.next_workspace_refresh = 1000.0;
        tx.send(CodexUpdateEvent::Status(json!({"phase":"ready","updated":true,"activeVersion":"0.163.0"}))).unwrap();
        drop(tx);
        poll(&mut app, 10.0);
        assert_eq!(app.codex_runtime.next_workspace_refresh, 0.0);
        assert!(!cancel.load(Ordering::Relaxed));
        assert!(!app.codex_runtime.updates.running());
        assert_eq!(inspect(&app)["status"]["activeVersion"], "0.163.0");
    }

    #[test]
    fn updater_disconnect_is_retryable_and_does_not_block_chat_switching() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let (tx, rx) = mpsc::channel();
        app.codex_runtime.updates.rx = Some(rx);
        drop(tx);
        poll(&mut app, 4.0);
        assert!(!app.codex_runtime.updates.running());
        assert!(!app.codex_runtime.busy());
        assert_eq!(app.codex_runtime.updates.next_check, 3604.0);
    }

    #[test]
    fn automatic_checks_are_throttled_and_disabled_preferences_are_respected() {
        let calls = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = calls.clone();
        let mut app = PhotocraftApp::new(
            Default::default(),
            crate::Services {
                codex_update: Some(Box::new(move |request, tx| {
                    assert!(!request.force);
                    observed.set(observed.get() + 1);
                    tx.send(CodexUpdateEvent::Status(json!({"phase":"current","activeVersion":"0.163.0"}))).unwrap();
                })),
                ..Default::default()
            },
        );
        poll(&mut app, 0.0);
        poll(&mut app, 0.1);
        poll(&mut app, 60.0);
        assert_eq!(calls.get(), 1);
        set_automatic(&mut app, false).unwrap();
        poll(&mut app, 7200.0);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn update_preference_is_saved_immediately_and_restored_separately_from_chats() {
        let store = std::rc::Rc::new(std::cell::RefCell::new(json!({"unrelated":{"keep":42}}).to_string()));
        let read = store.clone();
        let write = store.clone();
        let mut app = PhotocraftApp::new(
            Default::default(),
            crate::Services {
                load_prefs: Some(Box::new(move || Some(read.borrow().clone()))),
                save_prefs: Some(Box::new(move |text| {
                    *write.borrow_mut() = text.into();
                    Ok(())
                })),
                ..Default::default()
            },
        );
        set_automatic(&mut app, false).unwrap();
        let saved: Value = serde_json::from_str(&store.borrow()).unwrap();
        assert_eq!(saved["unrelated"]["keep"], 42);
        assert_eq!(saved["yorusphotoCodexUpdates"]["automatic"], false);
        let read = store.clone();
        let mut restarted =
            PhotocraftApp::new(Default::default(), crate::Services { load_prefs: Some(Box::new(move || Some(read.borrow().clone()))), ..Default::default() });
        restore_preferences(&mut restarted);
        assert!(!restarted.ui.codex.automatic_codex_updates);
    }

    #[test]
    fn failed_preference_save_does_not_change_the_previous_setting() {
        let mut app = PhotocraftApp::new(Default::default(), crate::Services { save_prefs: Some(Box::new(|_| Err("disk full".into()))), ..Default::default() });
        assert!(set_automatic(&mut app, false).is_err());
        assert!(app.ui.codex.automatic_codex_updates);
    }
}
