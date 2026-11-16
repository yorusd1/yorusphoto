//! Local slash commands: controls never become accidental model prompts.

use super::{CodexWorkspaceAction, MAX_MESSAGE_BYTES, PhotocraftApp, Value, account, chats, json, request_workspace, save_conversation, status, stop};

pub(super) const HELP: &[(&str, &str)] = &[
    ("/account", "Show connected account and sign-in controls"),
    ("/logout", "Sign out of the shared Codex session; keep saved chats"),
    ("/help", "Show available commands"),
    ("/status", "Show context, compaction and account usage"),
    ("/usage", "Refresh credit balance, limits and reset times"),
    ("/model [model] [effort]", "Choose an available Codex model and reasoning effort"),
    ("/effort <effort|auto>", "Set reasoning effort for the selected model"),
    ("/compact", "Compact the current Codex context"),
    ("/new", "Save this chat and start a new one"),
    ("/chats", "Search and open saved chats"),
    ("/resume [chat ID]", "Open a local chat or an editor-owned Codex thread"),
    ("/rename <title>", "Rename the current saved chat"),
    ("/archive", "Archive the current saved chat"),
    ("/unarchive <chat ID>", "Restore an archived saved chat"),
    ("/fork", "Create a new Codex chat with the current history"),
    ("/stop", "Cancel the active request"),
    ("/updates", "Open Codex automatic update settings"),
    ("/update", "Check and install the latest stable Codex runtime"),
];

fn split(input: &str) -> Option<(&str, &str)> {
    let input = input.trim();
    if !input.starts_with('/') {
        return None;
    }
    Some(input.split_once(char::is_whitespace).map_or((input, ""), |(command, args)| (command, args.trim())))
}

pub(super) fn set_model(app: &mut PhotocraftApp, model: Option<String>, effort: Option<String>) -> Result<(), String> {
    if !chats::can_switch(app) {
        return Err("Finish the active task before changing the model.".into());
    }
    if model.as_ref().is_some_and(|value| value.is_empty() || value.len() > 256) || effort.as_ref().is_some_and(|value| value.is_empty() || value.len() > 64) {
        return Err("Invalid model or reasoning effort.".into());
    }
    app.codex_runtime.workspace.validate_selection(model.as_deref(), effort.as_deref())?;
    let previous_model = app.ui.codex.model.clone();
    let previous_effort = app.ui.codex.reasoning_effort.clone();
    app.ui.codex.model = model;
    app.ui.codex.reasoning_effort = effort;
    save_conversation(app);
    if let Some(error) = app.codex_runtime.history_error.clone() {
        app.ui.codex.model = previous_model;
        app.ui.codex.reasoning_effort = previous_effort;
        return Err(error);
    }
    status(app, "Model settings saved for this conversation. They apply to the next turn.", false);
    Ok(())
}

pub(super) fn current_thread(app: &PhotocraftApp) -> Result<String, String> {
    app.ui.codex.thread_id.clone().ok_or_else(|| "Send a message before using this command; this chat has no Codex thread yet.".into())
}

pub(super) fn try_run(app: &mut PhotocraftApp) -> Result<bool, String> {
    let input = app.ui.codex.chat_prompt.clone();
    let Some((command, args)) = split(&input) else {
        return Ok(false);
    };
    if input.len() > MAX_MESSAGE_BYTES {
        return Err("The command exceeds the 128 KiB limit.".into());
    }
    if !args.is_empty()
        && matches!(
            command,
            "/account"
                | "/logout"
                | "/help"
                | "/?"
                | "/status"
                | "/usage"
                | "/limits"
                | "/models"
                | "/compact"
                | "/new"
                | "/clear"
                | "/chats"
                | "/archive"
                | "/fork"
                | "/stop"
                | "/cancel"
                | "/updates"
                | "/update"
        )
    {
        return Err(format!("{command} takes no arguments. Use /help for command syntax."));
    }
    app.ui.codex.chat_prompt.clear();
    let result = (|| -> Result<(), String> {
        match command {
            "/help" | "/?" => {
                app.codex_runtime.show_workspace = true;
                status(app, "Slash commands control this panel directly. Choose a command from Help.", false);
            }
            "/account" => app.codex_runtime.show_account = true,
            "/logout" => account::logout(app, false)?,
            "/status" => app.codex_runtime.show_workspace = true,
            "/usage" | "/limits" => {
                app.codex_runtime.show_workspace = true;
                request_workspace(app, CodexWorkspaceAction::Refresh)?;
            }
            "/model" | "/models" if args.is_empty() => app.codex_runtime.show_models = true,
            "/model" => {
                let mut values = args.split_whitespace();
                let model = values.next().filter(|value| *value != "auto").map(str::to_owned);
                let effort = values.next().filter(|value| *value != "auto").map(str::to_owned);
                if values.next().is_some() {
                    return Err("Use /model <model> [effort], or /model auto.".into());
                }
                set_model(app, model, effort)?;
            }
            "/effort" => {
                if args.is_empty() || args.split_whitespace().count() != 1 {
                    return Err("Use /effort <effort|auto>.".into());
                }
                set_model(app, app.ui.codex.model.clone(), (args != "auto").then(|| args.to_owned()))?;
            }
            "/compact" => request_workspace(app, CodexWorkspaceAction::CompactThread { thread_id: current_thread(app)? })?,
            "/new" | "/clear" => chats::new_chat(app)?,
            "/chats" | "/resume" if args.is_empty() => app.codex_runtime.chats.open = true,
            "/resume" => {
                if args.len() > 256 || args.split_whitespace().count() != 1 {
                    return Err("Use /resume <saved chat ID>.".into());
                }
                if app.codex_runtime.chats.entries.iter().any(|chat| chat.id == args || chat.thread_id.as_deref() == Some(args)) {
                    let local_id = app
                        .codex_runtime
                        .chats
                        .entries
                        .iter()
                        .find(|chat| chat.id == args || chat.thread_id.as_deref() == Some(args))
                        .map(|chat| chat.id.clone())
                        .ok_or("Saved chat is missing.")?;
                    chats::activate(app, &local_id)?;
                } else {
                    request_workspace(app, CodexWorkspaceAction::ReadThread { thread_id: args.into() })?;
                }
            }
            "/rename" => {
                if args.is_empty() {
                    return Err("Use /rename <chat title>.".into());
                }
                let id = app.codex_runtime.chats.current_id.clone();
                chats::rename(app, &id, args)?;
            }
            "/archive" => {
                let id = app.codex_runtime.chats.current_id.clone();
                chats::archive(app, &id, true)?;
            }
            "/unarchive" => {
                if args.is_empty() {
                    return Err("Use /unarchive <saved chat ID>.".into());
                }
                chats::archive(app, args, false)?;
            }
            "/fork" => request_workspace(app, CodexWorkspaceAction::ForkThread { thread_id: current_thread(app)? })?,
            "/stop" | "/cancel" => stop(app),
            "/updates" => {
                super::updates::menu(app, "codex.updates", &json!({"open":true})).ok_or("Update controls are unavailable.")??;
            }
            "/update" => {
                super::updates::menu(app, "codex.update", &json!({})).ok_or("Update controls are unavailable.")??;
            }
            _ => return Err(format!("Unknown slash command {command}. Use /help to see the supported commands.")),
        }
        Ok(())
    })();
    if let Err(error) = result {
        app.ui.codex.chat_prompt = input;
        return Err(error);
    }
    save_conversation(app);
    Ok(true)
}

pub(super) fn help_ui(ui: &mut egui::Ui) {
    ui.strong("Commands");
    for (command, description) in HELP {
        ui.horizontal_wrapped(|ui| {
            ui.code(*command);
            ui.small(*description);
        });
    }
    ui.small("Unknown slash commands are rejected locally. They are never sent as prompts.");
}

pub(super) fn specs(app: &PhotocraftApp) -> Vec<Value> {
    let idle = chats::can_switch(app);
    let native = app.services.codex_workspace.is_some();
    [
        ("codex.login", "Connect a ChatGPT account through Codex", "{}", account::can_change(app) && app.services.codex_login.is_some()),
        ("codex.account", "Show the connected Codex account", "{}", true),
        ("codex.logout", "Sign out of the shared Codex session, keeping saved chats", "{}", native && account::can_change(app)),
        ("codex.account.switch", "Sign out and connect another ChatGPT account", "{}", native && account::can_change(app) && app.services.codex_login.is_some()),
        ("codex.status", "Inspect Codex context, limits and saved chats", "{}", true),
        ("codex.refresh", "Refresh Codex models and account usage", "{}", native),
        ("codex.compact", "Compact current Codex conversation", "{}", native && idle && app.ui.codex.thread_id.is_some()),
        ("codex.model", "Select Codex model and reasoning", "{model:string|null,reasoningEffort?:string|null}; IDs must come from codex.status workspace.models", idle),
        ("codex.chats", "List and search saved editor chats", "{search?:string,archived?:bool,open?:bool}; returns browser.visibleIds", true),
        ("codex.chat.new", "Save current chat and start another", "{}", idle),
        ("codex.chat.open", "Open saved editor chat", "{id:string}", idle),
        ("codex.chat.rename", "Rename saved editor chat", "{id?:string,title:string}", idle),
        ("codex.chat.archive", "Archive or restore saved editor chat", "{id?:string,archived?:bool}", idle),
        ("codex.threads.list", "Find durable editor-owned Codex threads", "{cursor?:string,archived?:bool,search?:string}; asynchronous; inspect codexWorkspace.remoteThreads", native),
        ("codex.thread.open", "Import saved Codex thread", "{threadId:string}", native && idle),
        ("codex.thread.fork", "Fork saved Codex history", "{threadId?:string}", native && idle),
    ].into_iter().map(|(id,label,params,enabled)| json!({"id":id,"label":label,"params":params,"menu":[],"shortcut":null,
        "enabled":enabled,"disabledReason":if enabled {Value::Null} else {json!("Codex is busy or this operation requires a native session")},"needsDocument":false})).collect()
}

fn optional_text(params: &Value, key: &str, limit: usize) -> Result<Option<String>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) if !value.is_empty() && value.len() <= limit && !value.chars().any(char::is_control) => Ok(Some(value.clone())),
        _ => Err(format!("{key} must be nonempty text of at most {limit} bytes, or null.")),
    }
}

fn required_text(params: &Value, key: &str, limit: usize) -> Result<String, String> {
    optional_text(params, key, limit)?.ok_or_else(|| format!("{key} is required."))
}

fn boolean(params: &Value, key: &str, default: bool) -> Result<bool, String> {
    match params.get(key) {
        None => Ok(default),
        Some(Value::Bool(value)) => Ok(*value),
        _ => Err(format!("{key} must be boolean.")),
    }
}

pub(super) fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let result = (|| -> Result<Value, String> {
        match id {
            "codex.status" => Ok(super::workspace_inspect(app)),
            "codex.chats" => chats::configure_browser(app, params),
            "codex.refresh" => request_workspace(app, CodexWorkspaceAction::Refresh).map(|()| json!({"started":true})),
            "codex.compact" => {
                request_workspace(app, CodexWorkspaceAction::CompactThread { thread_id: current_thread(app)? }).map(|()| json!({"started":true}))
            }
            "codex.model" => set_model(app, optional_text(params, "model", 256)?, optional_text(params, "reasoningEffort", 64)?)
                .map(|()| json!({"model":app.ui.codex.model,"reasoningEffort":app.ui.codex.reasoning_effort})),
            "codex.chat.new" => chats::new_chat(app).map(|()| chats::inspect(app)),
            "codex.chat.open" => chats::activate(app, &required_text(params, "id", 64)?).map(|()| chats::inspect(app)),
            "codex.chat.rename" => {
                let chat = optional_text(params, "id", 64)?.unwrap_or_else(|| app.codex_runtime.chats.current_id.clone());
                chats::rename(app, &chat, &required_text(params, "title", 320)?)?;
                Ok(chats::inspect(app))
            }
            "codex.chat.archive" => {
                let chat = optional_text(params, "id", 64)?.unwrap_or_else(|| app.codex_runtime.chats.current_id.clone());
                chats::archive(app, &chat, boolean(params, "archived", true)?)?;
                Ok(chats::inspect(app))
            }
            "codex.threads.list" => request_workspace(
                app,
                CodexWorkspaceAction::ListThreads {
                    cursor: optional_text(params, "cursor", 4096)?,
                    archived: boolean(params, "archived", false)?,
                    search: optional_text(params, "search", 256)?,
                },
            )
            .map(|()| json!({"started":true})),
            "codex.thread.open" => request_workspace(app, CodexWorkspaceAction::ReadThread { thread_id: required_text(params, "threadId", 256)? })
                .map(|()| json!({"started":true})),
            "codex.thread.fork" => request_workspace(
                app,
                CodexWorkspaceAction::ForkThread { thread_id: optional_text(params, "threadId", 256)?.map_or_else(|| current_thread(app), Ok)? },
            )
            .map(|()| json!({"started":true})),
            _ => Err("Unknown Codex workspace command.".into()),
        }
    })();
    id.starts_with("codex.").then_some(result).filter(|_| !matches!(id, "codex.send" | "codex.cancel" | "codex.login"))
}

#[cfg(test)]
mod tests {
    use super::super::{Arc, AtomicBool, Ordering, send_chat};
    use super::*;

    #[test]
    fn parser_distinguishes_prompts_from_local_commands() {
        assert_eq!(split("hello /model"), None);
        assert_eq!(split("  /model\tgpt-example high  "), Some(("/model", "gpt-example high")));
        assert_eq!(split("/status"), Some(("/status", "")));
    }

    #[test]
    fn unknown_commands_never_capture_pixels_or_start_a_turn() {
        let mut app = PhotocraftApp::new(
            Default::default(),
            crate::Services { codex_chat: Some(Box::new(|_, _, _| panic!("command must not become a prompt"))), ..Default::default() },
        );
        app.ui.codex.chat_prompt = "/unsupported-command".into();
        assert!(super::super::send_chat(&mut app).unwrap_err().contains("Unknown slash command"));
        assert_eq!(app.ui.codex.chat_prompt, "/unsupported-command");
        assert!(app.codex_runtime.placement.is_none());
        assert!(!app.codex_runtime.busy());
    }

    #[test]
    fn status_and_help_work_without_a_document_model_request_or_login() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        for text in ["/status", "/help", "/model", "/chats"] {
            app.ui.codex.chat_prompt = text.into();
            assert!(try_run(&mut app).unwrap());
            assert!(app.ui.codex.chat_prompt.is_empty());
            assert!(!app.codex_runtime.busy());
        }
    }

    #[test]
    fn compact_without_a_thread_is_a_clear_local_error() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.ui.codex.chat_prompt = "/compact".into();
        assert!(try_run(&mut app).unwrap_err().contains("no Codex thread"));
        assert!(app.codex_runtime.placement.is_none());
    }

    #[test]
    fn slash_stop_cancels_before_the_busy_guard() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let cancel = Arc::new(AtomicBool::new(false));
        app.codex_runtime.cancel = Some(cancel.clone());
        app.ui.codex.chat_prompt = "/stop".into();
        send_chat(&mut app).unwrap();
        assert!(cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn commands_with_extra_arguments_do_not_mutate_a_different_chat() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let before = chats::inspect(&app);
        for input in ["/archive other-chat", "/new do-this-later", "/compact unexpected", "/stop surprise"] {
            app.ui.codex.chat_prompt = input.into();
            assert!(try_run(&mut app).unwrap_err().contains("takes no arguments"));
            assert_eq!(app.ui.codex.chat_prompt, input);
            assert_eq!(chats::inspect(&app), before);
        }
    }

    #[test]
    fn new_command_is_not_saved_as_a_draft_in_the_previous_chat() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        let first = app.codex_runtime.chats.current_id.clone();
        app.ui.codex.chat_prompt = "/new".into();
        try_run(&mut app).unwrap();
        chats::activate(&mut app, &first).unwrap();
        assert!(app.ui.codex.chat_prompt.is_empty());
    }
}
