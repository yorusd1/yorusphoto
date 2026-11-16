//! Bounded, app-owned conversations. Account data and authentication never enter this file.

use super::{Message, PhotocraftApp, Value, json};
use serde::{Deserialize, Serialize};

pub(super) const MAX_BYTES: usize = 16 << 20;
const MAX_TEXT_BYTES: usize = 128 << 10;
const MAX_CHATS: usize = 100;
const MAX_MESSAGES: usize = 100;
const DEFAULT_TITLE: &str = "New chat";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct SavedChat {
    pub(super) id: String,
    pub(super) title: String,
    pub(super) renamed: bool,
    pub(super) archived: bool,
    pub(super) thread_id: Option<String>,
    pub(super) messages: Vec<Message>,
    pub(super) draft: String,
    pub(super) model: Option<String>,
    pub(super) reasoning: Option<String>,
    pub(super) token_stats: Option<Value>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedChats {
    version: u8,
    current_id: String,
    chats: Vec<SavedChat>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyConversation {
    version: u8,
    thread_id: Option<String>,
    messages: Vec<Message>,
    draft: String,
}

#[derive(Default)]
pub(super) struct Runtime {
    pub(super) current_id: String,
    pub(super) entries: Vec<SavedChat>,
    pub(super) open: bool,
    search: String,
    show_archived: bool,
    rename_id: Option<String>,
    rename_title: String,
    filter_key: Option<(String, bool, u64)>,
    filtered_ids: Vec<String>,
    revision: u64,
}

fn blank(id: String, model: Option<String>, reasoning: Option<String>) -> SavedChat {
    SavedChat {
        id,
        title: DEFAULT_TITLE.into(),
        renamed: false,
        archived: false,
        thread_id: None,
        messages: Vec::new(),
        draft: String::new(),
        model,
        reasoning,
        token_stats: None,
    }
}

fn initialize(app: &mut PhotocraftApp) {
    if app.codex_runtime.chats.entries.is_empty() {
        let first = blank("chat-1".into(), app.ui.codex.model.clone(), app.ui.codex.reasoning_effort.clone());
        app.codex_runtime.chats.current_id = first.id.clone();
        app.codex_runtime.chats.entries.push(first);
    }
}

fn generated_title(messages: &[Message]) -> String {
    let title: String = messages
        .iter()
        .find(|message| message.role == "You" && !message.text.trim().is_empty())
        .map(|message| message.text.split_whitespace().collect::<Vec<_>>().join(" ").chars().filter(|character| !character.is_control()).take(80).collect())
        .unwrap_or_default();
    if title.trim().is_empty() { DEFAULT_TITLE.into() } else { title }
}

fn valid_short(value: &Option<String>) -> bool {
    value.as_ref().is_none_or(|value| !value.trim().is_empty() && value.len() <= 256 && !value.chars().any(char::is_control))
}

fn validate(saved: &SavedChats) -> Result<(), String> {
    let error = || "Saved chats have an unsupported or damaged format. The original file was kept.".to_owned();
    if saved.version != 2 || saved.chats.is_empty() || saved.chats.len() > MAX_CHATS || saved.current_id.len() > 64 {
        return Err(error());
    }
    let mut ids = std::collections::HashSet::new();
    for chat in &saved.chats {
        if chat.id.is_empty()
            || chat.id.len() > 64
            || !chat.id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            || !ids.insert(chat.id.as_str())
            || chat.title.trim().is_empty()
            || chat.title.chars().count() > 80
            || chat.title.chars().any(char::is_control)
            || !valid_short(&chat.thread_id)
            || !valid_short(&chat.model)
            || !valid_short(&chat.reasoning)
            || chat.messages.len() > MAX_MESSAGES
            || chat.draft.len() > MAX_TEXT_BYTES
            || chat.messages.iter().any(|message| !matches!(message.role.as_str(), "You" | "Codex") || message.text.len() > MAX_TEXT_BYTES)
        {
            return Err(error());
        }
        if let Some(stats) = &chat.token_stats {
            let usage = serde_json::from_value::<super::workspace::TokenUsage>(stats.clone()).map_err(|_| error())?;
            if !usage.valid() {
                return Err(error());
            }
        }
    }
    if !ids.contains(saved.current_id.as_str()) {
        return Err(error());
    }
    Ok(())
}

fn decode(text: &str) -> Result<(SavedChats, bool), String> {
    if text.len() > MAX_BYTES {
        return Err("Saved chats exceed the 16 MiB local history limit. The original file was kept.".into());
    }
    let value: Value = serde_json::from_str(text).map_err(|_| "Saved chats could not be decoded. The original file was kept.")?;
    let (saved, migrated) = if value.get("version").and_then(Value::as_u64) == Some(1) {
        let old: LegacyConversation = serde_json::from_value(value).map_err(|_| "The previous conversation has an invalid format. Its file was kept.")?;
        if old.version != 1 {
            return Err("Unsupported conversation version. Its file was kept.".into());
        }
        let mut first = blank("chat-1".into(), None, None);
        first.title = generated_title(&old.messages);
        first.thread_id = old.thread_id;
        first.messages = old.messages;
        first.draft = old.draft;
        (SavedChats { version: 2, current_id: first.id.clone(), chats: vec![first] }, true)
    } else {
        let saved = serde_json::from_value(value).map_err(|_| "Saved chats have an invalid format. The original file was kept.")?;
        (saved, false)
    };
    validate(&saved)?;
    Ok((saved, migrated))
}

fn encode(saved: &SavedChats) -> Result<String, String> {
    validate(saved)?;
    let text = serde_json::to_string(saved).map_err(|_| "Could not encode saved chats.")?;
    if text.len() > MAX_BYTES {
        return Err("Saved chats exceed the 16 MiB local history limit. No chats were deleted.".into());
    }
    Ok(text)
}

fn current_snapshot(app: &PhotocraftApp) -> Result<SavedChats, String> {
    let mut saved = SavedChats { version: 2, current_id: app.codex_runtime.chats.current_id.clone(), chats: app.codex_runtime.chats.entries.clone() };
    if saved.chats.is_empty() {
        let first = blank("chat-1".into(), app.ui.codex.model.clone(), app.ui.codex.reasoning_effort.clone());
        saved.current_id = first.id.clone();
        saved.chats.push(first);
    }
    let chat = saved.chats.iter_mut().find(|chat| chat.id == saved.current_id).ok_or("The active saved chat is missing. No history was changed.")?;
    chat.thread_id = app.ui.codex.thread_id.clone();
    chat.messages = app.ui.codex.messages.clone();
    chat.draft = app.ui.codex.chat_prompt.clone();
    chat.model = app.ui.codex.model.clone();
    chat.reasoning = app.ui.codex.reasoning_effort.clone();
    chat.token_stats =
        app.codex_runtime.workspace.token_usage.as_ref().map(serde_json::to_value).transpose().map_err(|_| "Could not encode chat token usage.")?;
    if !chat.renamed {
        chat.title = generated_title(&chat.messages);
    }
    validate(&saved)?;
    Ok(saved)
}

fn commit(app: &mut PhotocraftApp, saved: SavedChats) -> Result<(), String> {
    let text = encode(&saved)?;
    if app.services.save_codex_state.is_some() && !app.codex_runtime.history_writable {
        return Err(app.codex_runtime.history_error.clone().unwrap_or_else(|| "Saved chats are currently read-only. The existing file was kept.".into()));
    }
    if app.codex_runtime.history_snapshot.as_ref() != Some(&text) {
        if let Some(write) = app.services.save_codex_state.as_ref() {
            write(&text)?;
        }
        app.codex_runtime.history_snapshot = Some(text);
        app.codex_runtime.chats.filter_key = None;
        app.codex_runtime.chats.revision = app.codex_runtime.chats.revision.saturating_add(1);
    }
    app.codex_runtime.chats.current_id = saved.current_id;
    app.codex_runtime.chats.entries = saved.chats;
    app.codex_runtime.history_error = None;
    Ok(())
}

fn hydrate(app: &mut PhotocraftApp) -> Result<(), String> {
    let chat =
        app.codex_runtime.chats.entries.iter().find(|chat| chat.id == app.codex_runtime.chats.current_id).cloned().ok_or("The saved chat is missing.")?;
    app.ui.codex.thread_id = chat.thread_id;
    app.ui.codex.messages = chat.messages;
    app.ui.codex.chat_prompt = chat.draft;
    app.ui.codex.model = chat.model;
    app.ui.codex.reasoning_effort = chat.reasoning;
    app.ui.codex.status.clear();
    app.ui.codex.error = false;
    app.codex_runtime.workspace.token_usage = chat.token_stats.and_then(|stats| serde_json::from_value(stats).ok());
    app.codex_runtime.workspace.tokens_observed_at_unix = None;
    app.codex_runtime.workspace.compaction = super::workspace::Compaction::Idle;
    app.codex_runtime.workspace.compaction_observed_at_unix = None;
    app.codex_runtime.placement = None;
    Ok(())
}

/// v1 is migrated in memory; the native writer retains it as the previous snapshot.
pub(crate) fn restore(app: &mut PhotocraftApp) {
    initialize(app);
    app.codex_runtime.history_writable = app.services.save_codex_state.is_some();
    let result = app.services.load_codex_state.as_ref().map_or(Ok(None), |load| load()).and_then(|text| text.map(|text| decode(&text)).transpose());
    match result {
        Ok(Some((saved, migrated))) => {
            app.codex_runtime.history_snapshot = if migrated { None } else { encode(&saved).ok() };
            app.codex_runtime.chats.current_id = saved.current_id;
            app.codex_runtime.chats.entries = saved.chats;
            if let Err(error) = hydrate(app) {
                app.codex_runtime.history_writable = false;
                app.codex_runtime.history_error = Some(error);
            }
        }
        Ok(None) => {}
        Err(error) => {
            app.codex_runtime.history_writable = false;
            app.codex_runtime.history_error = Some(error);
        }
    }
}

pub(crate) fn save(app: &mut PhotocraftApp) {
    if !app.codex_runtime.history_writable && app.services.save_codex_state.is_some() {
        return;
    }
    if let Err(error) = current_snapshot(app).and_then(|saved| commit(app, saved)) {
        app.codex_runtime.history_error = Some(error);
    }
}

pub(super) fn can_switch(app: &PhotocraftApp) -> bool {
    !app.codex_runtime.busy()
        && app.codex_runtime.login_rx.is_none()
        && !app.yorus_ai_runtime.busy()
        && app.codex_runtime.pending_image.is_none()
        && app.codex_runtime.additional_images.is_empty()
        && app.codex_runtime.waiting_tools.is_empty()
}

fn editable_snapshot(app: &mut PhotocraftApp) -> Result<SavedChats, String> {
    if !can_switch(app) {
        return Err("Finish the current operation and place or discard retained images before changing chats.".into());
    }
    let saved = current_snapshot(app)?;
    commit(app, saved.clone())?;
    Ok(saved)
}

fn next_id(entries: &[SavedChat]) -> Result<String, String> {
    if entries.len() >= MAX_CHATS {
        return Err("The saved chat limit (100) was reached. Existing chats were kept.".into());
    }
    (1..=MAX_CHATS + 1)
        .map(|index| format!("chat-{index}"))
        .find(|id| entries.iter().all(|chat| chat.id != *id))
        .ok_or_else(|| "Could not allocate a saved chat identifier.".into())
}

pub(super) fn new_chat(app: &mut PhotocraftApp) -> Result<(), String> {
    let mut saved = editable_snapshot(app)?;
    let id = next_id(&saved.chats)?;
    saved.chats.push(blank(id.clone(), app.ui.codex.model.clone(), app.ui.codex.reasoning_effort.clone()));
    saved.current_id = id;
    commit(app, saved)?;
    hydrate(app)?;
    app.codex_runtime.chats.open = false;
    Ok(())
}

pub(super) fn activate(app: &mut PhotocraftApp, id: &str) -> Result<(), String> {
    let mut saved = editable_snapshot(app)?;
    if !saved.chats.iter().any(|chat| chat.id == id) {
        return Err("The requested saved chat was not found.".into());
    }
    saved.current_id = id.into();
    commit(app, saved)?;
    hydrate(app)?;
    app.codex_runtime.chats.open = false;
    Ok(())
}

pub(super) fn rename(app: &mut PhotocraftApp, id: &str, title: &str) -> Result<(), String> {
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 80 || title.chars().any(char::is_control) {
        return Err("Use a chat name between 1 and 80 characters, without control characters.".into());
    }
    let mut saved = editable_snapshot(app)?;
    let chat = saved.chats.iter_mut().find(|chat| chat.id == id).ok_or("The requested saved chat was not found.")?;
    chat.title = title.into();
    chat.renamed = true;
    let thread = chat.thread_id.clone();
    commit(app, saved)?;
    if let Some(thread_id) = thread {
        super::sync_thread(app, super::CodexWorkspaceAction::RenameThread { thread_id, name: title.into() });
    }
    Ok(())
}

pub(super) fn archive(app: &mut PhotocraftApp, id: &str, archived: bool) -> Result<(), String> {
    let mut saved = editable_snapshot(app)?;
    let chat = saved.chats.iter_mut().find(|chat| chat.id == id).ok_or("The requested saved chat was not found.")?;
    chat.archived = archived;
    let thread = chat.thread_id.clone();
    commit(app, saved)?;
    if let Some(thread_id) = thread {
        super::sync_thread(app, super::CodexWorkspaceAction::ArchiveThread { thread_id, archived });
    }
    Ok(())
}

pub(super) fn inspect(app: &PhotocraftApp) -> Value {
    let search = app.codex_runtime.chats.search.trim().to_lowercase();
    let visible_ids: Vec<_> = app
        .codex_runtime
        .chats
        .entries
        .iter()
        .rev()
        .filter(|chat| matches_search(chat, &search, app.codex_runtime.chats.show_archived))
        .map(|chat| chat.id.as_str())
        .collect();
    json!({"currentId":app.codex_runtime.chats.current_id,"count":app.codex_runtime.chats.entries.len(),
        "canSwitch":can_switch(app),"chats":app.codex_runtime.chats.entries.iter().map(|chat|
            json!({"id":chat.id,"title":chat.title,"archived":chat.archived,"threadId":chat.thread_id,"messages":chat.messages.len()})).collect::<Vec<_>>(),
        "browser":{"open":app.codex_runtime.chats.open,"search":app.codex_runtime.chats.search,
            "archived":app.codex_runtime.chats.show_archived,"visibleIds":visible_ids}})
}

/// Browser controls are pure view state. Validate the entire request before changing it.
pub(super) fn configure_browser(app: &mut PhotocraftApp, params: &Value) -> Result<Value, String> {
    let object = params.as_object().ok_or("Chat browser parameters must be an object.")?;
    if object.keys().any(|key| !matches!(key.as_str(), "search" | "archived" | "open")) {
        return Err("Unknown chat browser parameter. Use search, archived or open.".into());
    }
    let search = match object.get("search") {
        None => None,
        Some(Value::String(value)) if value.chars().count() <= 160 && !value.chars().any(char::is_control) => Some(value.clone()),
        _ => return Err("search must be text of at most 160 characters, without control characters.".into()),
    };
    let flag = |key| match object.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(format!("{key} must be boolean.")),
    };
    let archived = flag("archived")?;
    let open = flag("open")?;
    if let Some(search) = search {
        app.codex_runtime.chats.search = search;
        app.codex_runtime.chats.filter_key = None;
    }
    if let Some(archived) = archived {
        app.codex_runtime.chats.show_archived = archived;
        app.codex_runtime.chats.filter_key = None;
    }
    if let Some(open) = open {
        app.codex_runtime.chats.open = open;
        if open {
            app.ui.codex.images_tab = false;
        }
    }
    Ok(inspect(app))
}

fn matches_search(chat: &SavedChat, search: &str, archived: bool) -> bool {
    chat.archived == archived
        && (search.is_empty() || chat.title.to_lowercase().contains(search) || chat.messages.iter().any(|message| message.text.to_lowercase().contains(search)))
}

pub(super) fn show_browser(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if !app.codex_runtime.chats.open {
        return;
    }
    initialize(app);
    let palette = crate::ai_sidebar::palette(ui);
    ui.separator();
    ui.horizontal_wrapped(|ui| {
        ui.strong("Saved chats");
        ui.checkbox(&mut app.codex_runtime.chats.show_archived, "Archived");
        if ui.small_button("Close").clicked() {
            app.codex_runtime.chats.open = false;
        }
    });
    ui.add(egui::TextEdit::singleline(&mut app.codex_runtime.chats.search).hint_text("Search chats…").desired_width(f32::INFINITY).char_limit(160));
    if ui.add_enabled(enabled_for_remote(app), egui::Button::new("Find saved Codex threads")).clicked() {
        super::open_remote_chats(app);
    }
    let key = (app.codex_runtime.chats.search.trim().to_lowercase(), app.codex_runtime.chats.show_archived, app.codex_runtime.chats.revision);
    if app.codex_runtime.chats.filter_key.as_ref() != Some(&key) {
        app.codex_runtime.chats.filtered_ids =
            app.codex_runtime.chats.entries.iter().rev().filter(|chat| matches_search(chat, &key.0, key.1)).map(|chat| chat.id.clone()).collect();
        app.codex_runtime.chats.filter_key = Some(key);
    }
    let rows: Vec<_> = app
        .codex_runtime
        .chats
        .filtered_ids
        .iter()
        .filter_map(|id| app.codex_runtime.chats.entries.iter().find(|chat| chat.id == *id))
        .map(|chat| (chat.id.clone(), chat.title.clone(), chat.archived))
        .collect();
    let enabled = can_switch(app);
    let mut action = None;
    egui::ScrollArea::vertical().id_salt("codex-saved-chats").max_height(205.0).auto_shrink([false, true]).show(ui, |ui| {
        if rows.is_empty() {
            ui.small("No matching chats.");
        }
        for (id, title, archived) in rows {
            ui.horizontal(|ui| {
                let active = app.codex_runtime.chats.current_id == id;
                let width = (ui.available_width() - 65.0).max(60.0);
                if ui
                    .add_enabled(enabled, egui::Button::new(&title).selected(active).truncate().min_size(egui::vec2(width, 25.0)))
                    .on_hover_text(&title)
                    .clicked()
                {
                    action = Some(("activate", id.clone()));
                }
                ui.add_enabled_ui(enabled, |ui| {
                    ui.menu_button("...", |ui| {
                        if ui.button("Rename").clicked() {
                            action = Some(("rename", id.clone()));
                            ui.close();
                        }
                        if ui.button(if archived { "Restore from archive" } else { "Archive" }).clicked() {
                            action = Some((if archived { "unarchive" } else { "archive" }, id.clone()));
                            ui.close();
                        }
                    });
                });
            });
        }
    });
    if let Some((action, id)) = action {
        let result = match action {
            "activate" => activate(app, &id),
            "archive" => archive(app, &id, true),
            "unarchive" => archive(app, &id, false),
            _ => {
                app.codex_runtime.chats.rename_title =
                    app.codex_runtime.chats.entries.iter().find(|chat| chat.id == id).map(|chat| chat.title.clone()).unwrap_or_default();
                app.codex_runtime.chats.rename_id = Some(id);
                Ok(())
            }
        };
        if let Err(error) = result {
            super::status(app, error, true);
        }
    }
    if let Some(id) = app.codex_runtime.chats.rename_id.clone() {
        ui.add(egui::TextEdit::singleline(&mut app.codex_runtime.chats.rename_title).hint_text("Chat name").char_limit(80).desired_width(f32::INFINITY));
        ui.horizontal(|ui| {
            if ui.add_enabled(enabled, egui::Button::new("Save name")).clicked() {
                let title = app.codex_runtime.chats.rename_title.clone();
                match rename(app, &id, &title) {
                    Ok(()) => app.codex_runtime.chats.rename_id = None,
                    Err(error) => super::status(app, error, true),
                }
            }
            if ui.small_button("Cancel").clicked() {
                app.codex_runtime.chats.rename_id = None;
            }
        });
    }
    ui.label(egui::RichText::new("Saved on this device · archived chats are retained").small().color(palette.text_faint));
    ui.separator();
}

fn enabled_for_remote(app: &PhotocraftApp) -> bool {
    can_switch(app) && app.services.codex_workspace.is_some()
}

fn metadata_text(value: Option<&Value>, max_bytes: usize) -> Option<String> {
    value.and_then(Value::as_str).filter(|text| !text.trim().is_empty() && text.len() <= max_bytes && !text.chars().any(char::is_control)).map(str::to_owned)
}

/// Only visible text is imported; image data, tool arguments, account fields and paths are excluded.
pub(super) fn import_backend(app: &mut PhotocraftApp, result: &Value) -> Result<(), String> {
    let thread = result.get("thread").unwrap_or(result);
    let id = thread.get("id").and_then(Value::as_str).ok_or("The Codex thread has no identifier.")?;
    if !valid_short(&Some(id.into())) {
        return Err("The Codex thread identifier is invalid.".into());
    }
    if let Some(existing) = app.codex_runtime.chats.entries.iter().find(|chat| chat.thread_id.as_deref() == Some(id)).map(|chat| chat.id.clone()) {
        return activate(app, &existing);
    }
    let mut saved = editable_snapshot(app)?;
    let local_id = next_id(&saved.chats)?;
    let mut chat = blank(local_id.clone(), None, None);
    chat.thread_id = Some(id.into());
    chat.model = metadata_text(thread.get("model").or_else(|| result.get("model")), 256);
    chat.reasoning = metadata_text(thread.get("reasoningEffort").or_else(|| result.get("reasoningEffort")), 32);
    if let Some(turns) = thread.get("turns").and_then(Value::as_array) {
        for turn in turns.iter().rev() {
            if let Some(items) = turn.get("items").and_then(Value::as_array) {
                for item in items.iter().rev() {
                    let (role, text) = match item.get("type").and_then(Value::as_str) {
                        Some("agentMessage") => ("Codex", item.get("text").and_then(Value::as_str).unwrap_or_default().to_owned()),
                        Some("userMessage") => (
                            "You",
                            item.get("content")
                                .and_then(Value::as_array)
                                .map(|content| {
                                    content
                                        .iter()
                                        .filter(|part| matches!(part.get("type").and_then(Value::as_str), Some("text" | "inputText")))
                                        .filter_map(|part| part.get("text").and_then(Value::as_str))
                                        .collect::<Vec<_>>()
                                        .join("\n")
                                })
                                .unwrap_or_default(),
                        ),
                        _ => continue,
                    };
                    if !text.is_empty() {
                        if text.len() > MAX_TEXT_BYTES {
                            return Err("A Codex message exceeds the local chat display limit. Existing chats were kept.".into());
                        }
                        chat.messages.push(Message { role: role.into(), text });
                    }
                    if chat.messages.len() >= MAX_MESSAGES {
                        break;
                    }
                }
            }
            if chat.messages.len() >= MAX_MESSAGES {
                break;
            }
        }
        chat.messages.reverse();
    }
    chat.title = generated_title(&chat.messages);
    if let Some(name) = metadata_text(thread.get("name"), 4096) {
        chat.title = name.trim().chars().take(80).collect();
        chat.renamed = true;
    }
    saved.chats.push(chat);
    saved.current_id = local_id;
    commit(app, saved)?;
    hydrate(app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn services(disk: Arc<Mutex<Option<String>>>) -> crate::Services {
        let reader = disk.clone();
        crate::Services {
            load_codex_state: Some(Box::new(move || Ok(reader.lock().unwrap().clone()))),
            save_codex_state: Some(Box::new(move |text| {
                *disk.lock().unwrap() = Some(text.into());
                Ok(())
            })),
            ..Default::default()
        }
    }

    fn app(disk: Arc<Mutex<Option<String>>>) -> PhotocraftApp {
        PhotocraftApp::new(photocraft_engine::Session::new(), services(disk))
    }

    #[test]
    fn migrates_v4_current_chat_without_rewriting_on_load() {
        let old = json!({"version":1,"thread_id":"thread-old","messages":[{"role":"You","text":"Fix the blue sky"}],"draft":"later"}).to_string();
        let disk = Arc::new(Mutex::new(Some(old.clone())));
        let mut app = app(disk.clone());
        assert_eq!(app.ui.codex.thread_id.as_deref(), Some("thread-old"));
        assert_eq!(app.ui.codex.chat_prompt, "later");
        assert_eq!(app.codex_runtime.chats.entries.first().unwrap().title, "Fix the blue sky");
        assert_eq!(disk.lock().unwrap().as_ref(), Some(&old));
        save(&mut app);
        let saved: Value = serde_json::from_str(disk.lock().unwrap().as_ref().unwrap()).unwrap();
        assert_eq!(saved["version"], 2);
        assert_eq!(saved["chats"][0]["thread_id"], "thread-old");
    }

    #[test]
    fn multiple_chats_restore_drafts_models_and_threads_without_auth_data() {
        let disk = Arc::new(Mutex::new(None));
        let mut first = app(disk.clone());
        first.ui.codex.thread_id = Some("thread-one".into());
        first.ui.codex.messages.push(Message { role: "You".into(), text: "Paint a sun".into() });
        first.ui.codex.chat_prompt = "first unsent draft".into();
        first.ui.codex.model = Some("model-one".into());
        first.ui.codex.reasoning_effort = Some("high".into());
        first.codex_runtime.workspace.token_usage = Some(super::super::workspace::TokenUsage {
            last: super::super::workspace::TokenCounts { total_tokens: Some(42), ..Default::default() },
            model_context_window: Some(1000),
            ..Default::default()
        });
        first.codex_runtime.workspace.tokens_observed_at_unix = Some(123);
        first.codex_runtime.login_url = Some("https://chatgpt.com/?state=secret-not-saved".into());
        new_chat(&mut first).unwrap();
        first.ui.codex.thread_id = Some("thread-two".into());
        first.ui.codex.chat_prompt = "second unsent draft".into();
        first.ui.codex.model = Some("model-two".into());
        activate(&mut first, "chat-1").unwrap();
        assert_eq!(first.ui.codex.chat_prompt, "first unsent draft");
        assert_eq!(first.ui.codex.model.as_deref(), Some("model-one"));
        assert_eq!(first.ui.codex.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(first.ui.codex.thread_id.as_deref(), Some("thread-one"));
        assert_eq!(first.codex_runtime.workspace.token_usage.as_ref().unwrap().last.total_tokens, Some(42));
        assert_eq!(first.codex_runtime.workspace.tokens_observed_at_unix, None, "restored token counts are last known, never reported as fresh");
        let mut restored = app(disk.clone());
        assert_eq!(restored.codex_runtime.chats.entries.len(), 2);
        assert_eq!(restored.ui.codex.chat_prompt, "first unsent draft");
        activate(&mut restored, "chat-2").unwrap();
        assert_eq!(restored.ui.codex.chat_prompt, "second unsent draft");
        assert_eq!(restored.ui.codex.thread_id.as_deref(), Some("thread-two"));
        assert!(!disk.lock().unwrap().as_ref().unwrap().contains("secret-not-saved"));
    }

    #[test]
    fn rename_search_and_archive_preserve_messages() {
        let mut app = app(Arc::new(Mutex::new(None)));
        app.ui.codex.messages.push(Message { role: "You".into(), text: "An orange bird".into() });
        rename(&mut app, "chat-1", "Bird study").unwrap();
        archive(&mut app, "chat-1", true).unwrap();
        let chat = app.codex_runtime.chats.entries.first().unwrap();
        assert!(matches_search(chat, "orange", true));
        assert!(matches_search(chat, "bird study", true));
        assert!(!matches_search(chat, "bird", false));
        assert_eq!(chat.messages.len(), 1);
        archive(&mut app, "chat-1", false).unwrap();
        save(&mut app);
        assert_eq!(app.codex_runtime.chats.entries.first().unwrap().title, "Bird study");
        assert!(rename(&mut app, "chat-1", "\n").is_err());
        assert!(activate(&mut app, "unknown").is_err());
    }

    #[test]
    fn browser_commands_search_content_archive_and_validate_before_mutation() {
        let mut app = app(Arc::new(Mutex::new(None)));
        app.ui.codex.messages.push(Message { role: "You".into(), text: "An orange bird".into() });
        new_chat(&mut app).unwrap();
        let result = configure_browser(&mut app, &json!({"search":"ORANGE","open":true})).unwrap();
        assert_eq!(result["browser"]["visibleIds"], json!(["chat-1"]));
        archive(&mut app, "chat-1", true).unwrap();
        assert_eq!(inspect(&app)["browser"]["visibleIds"], json!([]));
        let archived = configure_browser(&mut app, &json!({"archived":true})).unwrap();
        assert_eq!(archived["browser"]["visibleIds"], json!(["chat-1"]));
        for invalid in [
            json!({"search":42,"open":false}),
            json!({"search":"","archived":"yes"}),
            json!({"open":false,"token":"reject"}),
            json!({"search":"x".repeat(161)}),
        ] {
            let before = inspect(&app);
            assert!(configure_browser(&mut app, &invalid).is_err());
            assert_eq!(inspect(&app), before);
        }
        assert_eq!(configure_browser(&mut app, &json!({})).unwrap(), inspect(&app));
        let all = configure_browser(&mut app, &json!({"search":"","archived":false,"open":false})).unwrap();
        assert_eq!(all["browser"]["visibleIds"], json!(["chat-2"]));
    }

    #[test]
    fn switch_is_blocked_during_work_and_for_recoverable_images() {
        let mut app = app(Arc::new(Mutex::new(None)));
        new_chat(&mut app).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        app.codex_runtime.chat_rx = Some(rx);
        assert!(activate(&mut app, "chat-1").is_err());
        drop(tx);
        app.codex_runtime.chat_rx = None;
        app.codex_runtime.pending_image = Some(vec![1, 2, 3]);
        assert!(new_chat(&mut app).is_err());
        assert_eq!(app.codex_runtime.pending_image.as_deref(), Some([1, 2, 3].as_slice()));
        assert_eq!(app.codex_runtime.chats.current_id, "chat-2");
    }

    #[test]
    fn corrupted_or_failed_history_never_replaces_a_saved_chat() {
        let disk = Arc::new(Mutex::new(Some("broken".into())));
        let mut broken = app(disk.clone());
        broken.ui.codex.chat_prompt = "kept draft".into();
        assert!(new_chat(&mut broken).is_err());
        save(&mut broken);
        assert_eq!(disk.lock().unwrap().as_deref(), Some("broken"));
        assert_eq!(broken.ui.codex.chat_prompt, "kept draft");
        let disk = Arc::new(Mutex::new(None));
        let mut full = app(disk.clone());
        full.ui.codex.chat_prompt = "keep this draft".into();
        save(&mut full);
        let previous = disk.lock().unwrap().clone();
        full.services.save_codex_state = Some(Box::new(|_| Err("disk full".into())));
        assert!(new_chat(&mut full).is_err());
        assert_eq!(full.codex_runtime.chats.current_id, "chat-1");
        assert_eq!(full.ui.codex.chat_prompt, "keep this draft");
        assert_eq!(*disk.lock().unwrap(), previous);
    }

    #[test]
    fn history_limits_reject_without_evicting_chats() {
        let mut app = app(Arc::new(Mutex::new(None)));
        for _ in 1..MAX_CHATS {
            new_chat(&mut app).unwrap();
        }
        assert!(new_chat(&mut app).is_err());
        assert_eq!(app.codex_runtime.chats.entries.len(), MAX_CHATS);
        let saved = current_snapshot(&app).unwrap();
        let mut value = serde_json::to_value(&saved).unwrap();
        value["chats"][0]["access_token"] = "reject".into();
        assert!(decode(&value.to_string()).is_err());
        let mut duplicate = saved.clone();
        let first_id = duplicate.chats.first().unwrap().id.clone();
        duplicate.chats.get_mut(1).unwrap().id = first_id;
        assert!(encode(&duplicate).is_err());
        let mut invalid_tokens = saved;
        invalid_tokens.chats.first_mut().unwrap().token_stats = Some(json!({"last":{},"total":{},"model_context_window":0}));
        assert!(encode(&invalid_tokens).is_err());
    }

    #[test]
    fn backend_import_only_retains_text_and_reuses_known_threads() {
        let mut app = app(Arc::new(Mutex::new(None)));
        import_backend(
            &mut app,
            &json!({"thread":{"id":"backend-one","name":"A named garden","model":"model-one","reasoningEffort":"high","access_token":"never-store","turns":[{"items":[
                {"type":"userMessage","content":[{"type":"text","text":"A blue garden"},{"type":"image","url":"data:never-store"}]},
                {"type":"agentMessage","text":"Ready."},{"type":"toolCall","arguments":{"token":"never-store"}}
            ]}]}}),
        )
        .unwrap();
        assert_eq!(app.ui.codex.messages.len(), 2);
        assert_eq!(app.ui.codex.thread_id.as_deref(), Some("backend-one"));
        assert_eq!(app.ui.codex.model.as_deref(), Some("model-one"));
        assert_eq!(app.ui.codex.reasoning_effort.as_deref(), Some("high"));
        let imported = app.codex_runtime.chats.entries.iter().find(|chat| chat.thread_id.as_deref() == Some("backend-one")).unwrap();
        assert_eq!(imported.title, "A named garden");
        assert!(imported.renamed);
        let count = app.codex_runtime.chats.entries.len();
        import_backend(&mut app, &json!({"id":"backend-one"})).unwrap();
        assert_eq!(app.codex_runtime.chats.entries.len(), count);
        assert!(!encode(&current_snapshot(&app).unwrap()).unwrap().contains("never-store"));
        import_backend(&mut app, &json!({"thread":{"id":"backend-two","name":"é".repeat(120)},"model":"model-two","reasoningEffort":"low"})).unwrap();
        assert_eq!(app.ui.codex.model.as_deref(), Some("model-two"));
        assert_eq!(app.ui.codex.reasoning_effort.as_deref(), Some("low"));
        assert_eq!(app.codex_runtime.chats.entries.last().unwrap().title.chars().count(), 80);
    }
}
