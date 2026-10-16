//! Native Codex workspace and selection-safe generated image placement.

mod chats;
mod commands;
mod updates;
pub mod workspace;

pub use updates::{CodexUpdateEvent, CodexUpdateFn, CodexUpdateRequest};

use crate::PhotocraftApp;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, Sender, TryRecvError},
};

pub struct ImageGenerationRequest {
    pub reference: Option<Vec<u8>>,
    pub prompt: String,
    pub quality: String,
    pub size: String,
    pub codex_path: String,
    pub cancel: Arc<AtomicBool>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

pub struct CodexChatRequest {
    pub prompt: String,
    pub thread_id: Option<String>,
    pub codex_path: String,
    pub reference: Option<Vec<u8>>,
    pub cancel: Arc<AtomicBool>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
}

pub enum CodexEvent {
    Started(String),
    Text(String),
    Status(String),
    Image(Vec<u8>),
    Finished,
    Error(String),
    Telemetry { kind: String, data: Value },
}

#[derive(Clone, Debug)]
pub enum CodexWorkspaceAction {
    Refresh,
    ListThreads { cursor: Option<String>, archived: bool, search: Option<String> },
    ReadThread { thread_id: String },
    RenameThread { thread_id: String, name: String },
    ArchiveThread { thread_id: String, archived: bool },
    ForkThread { thread_id: String },
    CompactThread { thread_id: String },
}

pub struct CodexWorkspaceRequest {
    pub codex_path: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub cancel: Arc<AtomicBool>,
    pub action: CodexWorkspaceAction,
}

pub enum CodexWorkspaceEvent {
    Result(Value),
    Telemetry { kind: String, data: Value },
    Error(String),
}

pub type CodexWorkspaceFn = Box<dyn Fn(CodexWorkspaceRequest, Sender<CodexWorkspaceEvent>)>;

pub struct AgentCommand {
    pub command: String,
    pub params: Value,
    pub reply: Sender<Result<Value, String>>,
}

pub type GenerateImageFn = Box<dyn Fn(ImageGenerationRequest, Sender<Result<Vec<u8>, String>>)>;
pub type CodexChatFn = Box<dyn Fn(CodexChatRequest, Sender<CodexEvent>, Sender<AgentCommand>)>;
pub enum CodexLoginEvent {
    OpeningBrowser { url: String },
    BrowserError(String),
    Connected,
    Error(String),
}

pub type CodexLoginFn = Box<dyn Fn(String, Arc<AtomicBool>, Sender<CodexLoginEvent>)>;
pub type LoadCodexStateFn = Box<dyn Fn() -> Result<Option<String>, String>>;
pub type SaveCodexStateFn = Box<dyn Fn(&str) -> Result<(), String>>;

const MAX_MESSAGE_BYTES: usize = 128 << 10;

/// Restore only editor-owned conversations, never authentication or OAuth state.
pub(crate) fn restore_conversation(app: &mut PhotocraftApp) {
    updates::restore_preferences(app);
    chats::restore(app);
}

pub(crate) fn save_conversation(app: &mut PhotocraftApp) {
    chats::save(app);
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub open: bool,
    #[serde(default)]
    pub sidebar_version: u8,
    pub images_tab: bool,
    pub entire_image: bool,
    pub prompt: String,
    pub chat_prompt: String,
    pub selection_reference: bool,
    pub quality: String,
    pub width: u32,
    pub height: u32,
    pub ratio: String,
    pub codex_path: String,
    pub thread_id: Option<String>,
    pub automatic_codex_updates: bool,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub messages: Vec<Message>,
    pub status: String,
    pub error: bool,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    pub role: String,
    pub text: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            open: true,
            sidebar_version: 1,
            images_tab: false,
            entire_image: false,
            prompt: String::new(),
            chat_prompt: String::new(),
            selection_reference: true,
            quality: "high".into(),
            width: 1024,
            height: 1024,
            ratio: "Selection".into(),
            codex_path: String::new(),
            automatic_codex_updates: true,
            thread_id: None,
            model: None,
            reasoning_effort: None,
            messages: Vec::new(),
            status: String::new(),
            error: false,
        }
    }
}

struct Placement {
    target: Option<Value>,
    width: u32,
    height: u32,
    name: String,
}

#[derive(Default)]
pub struct Runtime {
    updates: updates::Runtime,
    workspace: workspace::Workspace,
    chats: chats::Runtime,
    workspace_rx: Option<Receiver<CodexWorkspaceEvent>>,
    workspace_action: Option<CodexWorkspaceAction>,
    workspace_cancel: Option<Arc<AtomicBool>>,
    workspace_error: Option<String>,
    next_workspace_refresh: f64,
    show_workspace: bool,
    show_models: bool,
    remote_threads: Vec<Value>,
    remote_cursor: Option<String>,
    remote_browser: bool,
    image_rx: Option<Receiver<Result<Vec<u8>, String>>>,
    chat_rx: Option<Receiver<CodexEvent>>,
    command_rx: Option<Receiver<AgentCommand>>,
    login_rx: Option<Receiver<CodexLoginEvent>>,
    login_cancel: Option<Arc<AtomicBool>>,
    login_url: Option<String>,
    browser_error: Option<String>,
    connected: bool,
    history_writable: bool,
    history_snapshot: Option<String>,
    history_error: Option<String>,
    next_history_check: f64,
    cancel: Option<Arc<AtomicBool>>,
    placement: Option<Placement>,
    pending_image: Option<Vec<u8>>,
    additional_images: std::collections::VecDeque<Vec<u8>>,
    waiting_tools: Vec<(photocraft_engine::jobs::JobId, Sender<Result<Value, String>>)>,
}

impl Runtime {
    pub fn busy(&self) -> bool {
        self.image_rx.is_some()
            || self.chat_rx.is_some()
            || self.workspace_action.as_ref().is_some_and(|action| !matches!(action, CodexWorkspaceAction::Refresh | CodexWorkspaceAction::ListThreads { .. }))
    }
    pub fn connection(&self) -> Value {
        json!({"signingIn":self.login_rx.is_some(),"sessionConfirmed":self.connected,
            "manualLinkAvailable":self.login_url.is_some(),"historyAvailable":self.history_writable,
            "historyError":self.history_error})
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        if let Some(cancel) = &self.login_cancel {
            cancel.store(true, Ordering::Relaxed);
        }
        for cancel in [&self.cancel, &self.workspace_cancel].into_iter().flatten() {
            cancel.store(true, Ordering::Relaxed);
        }
    }
}

fn status(app: &mut PhotocraftApp, text: impl Into<String>, error: bool) {
    app.ui.codex.status = text.into();
    app.ui.codex.error = error;
}

/// Fixed geometry is captured before the model starts; current selection is never read on return.
fn capture(app: &mut PhotocraftApp, use_selection: bool) -> Result<Option<Vec<u8>>, String> {
    let s = &app.ui.codex;
    let mut placement = Placement { target: None, width: s.width, height: s.height, name: "Generated image".into() };
    let reference = if use_selection {
        let d = app.session.active().ok_or("Open a canvas and select an area first")?;
        let selection = d.doc.selection.as_ref().ok_or("Select an area first, or choose Entire image")?;
        if selection.content_bounds().intersect(&d.doc.bounds()).is_empty() {
            return Err("Select a nonempty area first".into());
        }
        let value = app.run("ai.capture", json!({}))?;
        let encoded = value.get("reference").and_then(Value::as_str).ok_or("capture returned no reference image")?;
        let bytes = base64::engine::general_purpose::STANDARD.decode(encoded).map_err(|e| e.to_string())?;
        placement.target = Some(value.get("target").cloned().ok_or("capture returned no target")?);
        Some(bytes)
    } else {
        None
    };
    app.codex_runtime.placement = Some(placement);
    app.codex_runtime.pending_image = None;
    Ok(reference)
}

fn apply_image(app: &mut PhotocraftApp, bytes: &[u8]) -> Result<Value, String> {
    let p = app.codex_runtime.placement.as_ref().ok_or("No saved image destination")?;
    let data = base64::engine::general_purpose::STANDARD.encode(bytes);
    let (command, params, source_document) = if let Some(target) = &p.target {
        ("ai.apply", json!({"target":target, "data":data, "name":p.name}), target.get("document").and_then(Value::as_u64))
    } else {
        ("ai.open", json!({"data":data, "name":p.name, "width":p.width, "height":p.height}), None)
    };
    let previous = app.session.active_index();
    if let Some(id) = source_document {
        let index = app.session.documents().iter().position(|d| d.doc.id.0 == id).ok_or("The original canvas was closed; the generated image is retained")?;
        if !app.session.set_active(index) {
            return Err("Could not select the original canvas".into());
        }
    }
    // Sync only after restoring the visible tab; temporary activation must not cancel
    // an editing preview in the user's currently visible document.
    let result = app.session.execute(command, params).map_err(|e| e.to_string());
    if source_document.is_some()
        && let Some(index) = previous
    {
        app.session.set_active(index);
    }
    app.sync_views();
    result
}

fn receive_image(app: &mut PhotocraftApp, bytes: Vec<u8>) {
    if app.codex_runtime.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
        return;
    }
    if app.codex_runtime.pending_image.is_some() {
        app.codex_runtime.additional_images.push_back(bytes);
        status(app, "Generated results are retained until you choose their destination.", true);
        return;
    }
    match apply_image(app, &bytes) {
        Ok(_) => {
            let selected = app.codex_runtime.placement.as_ref().is_some_and(|p| p.target.is_some());
            status(app, if selected { "Image added as a separate layer at the captured selection." } else { "Image opened in a new canvas." }, false);
        }
        Err(e) => {
            app.codex_runtime.pending_image = Some(bytes);
            status(app, format!("Image ready; placement needs attention: {e}"), true);
        }
    }
}

fn recover_next_image(app: &mut PhotocraftApp) {
    if app.codex_runtime.pending_image.is_none() {
        app.codex_runtime.pending_image = app.codex_runtime.additional_images.pop_front();
        if app.codex_runtime.pending_image.is_some() {
            status(app, "Another generated result is waiting for placement.", true);
        }
    }
}

fn stop_tool(app: &mut PhotocraftApp) {
    for (job, reply) in std::mem::take(&mut app.codex_runtime.waiting_tools) {
        crate::jobs_ui::cancel(app, job);
        let _ = reply.send(Err("Codex task stopped".into()));
    }
}

fn preferred_size(app: &PhotocraftApp) -> String {
    let s = &app.ui.codex;
    if let Some(rect) = app.codex_runtime.placement.as_ref().and_then(|p| p.target.as_ref()).and_then(|t| t.get("rect")).and_then(Value::as_array) {
        let coordinates: Option<Vec<i64>> = rect.iter().map(Value::as_i64).collect();
        if let Some(c) = coordinates
            && let [x0, y0, x1, y1] = c.as_slice()
        {
            let w = (x1 - x0).max(1) as f64;
            let h = (y1 - y0).max(1) as f64;
            let k = f64::from(s.width) / w.max(h);
            return format!("{}x{}", (w * k).round().max(1.0) as u32, (h * k).round().max(1.0) as u32);
        }
    }
    format!("{}x{}", s.width, s.height)
}

pub fn generate(app: &mut PhotocraftApp) -> Result<(), String> {
    if app.codex_runtime.busy() || app.codex_runtime.login_rx.is_some() || app.yorus_ai_runtime.busy() {
        return Err("A Codex task is already running".into());
    }
    if app.codex_runtime.pending_image.is_some() {
        return Err("Place or discard the previous generated image first".into());
    }
    if app.ui.codex.prompt.trim().is_empty() {
        return Err("Enter an image prompt".into());
    }
    if app.services.generate_image.is_none() {
        return Err("Image generation requires the native yorusphoto app".into());
    }
    let reference = capture(app, !app.ui.codex.entire_image)?;
    let cancel = Arc::new(AtomicBool::new(false));
    let s = &app.ui.codex;
    let req = ImageGenerationRequest {
        reference,
        prompt: s.prompt.clone(),
        quality: s.quality.clone(),
        size: preferred_size(app),
        codex_path: s.codex_path.clone(),
        cancel: cancel.clone(),
        model: s.model.clone(),
        reasoning_effort: s.reasoning_effort.clone(),
    };
    let (tx, rx) = mpsc::channel();
    if let Some(run) = &app.services.generate_image {
        run(req, tx);
    }
    app.codex_runtime.image_rx = Some(rx);
    app.codex_runtime.cancel = Some(cancel);
    status(app, "Generating with Codex using your ChatGPT accountâ€¦", false);
    Ok(())
}

pub fn send_chat(app: &mut PhotocraftApp) -> Result<(), String> {
    if commands::try_run(app)? {
        return Ok(());
    }
    if app.codex_runtime.busy() || app.codex_runtime.login_rx.is_some() || app.yorus_ai_runtime.busy() {
        return Err("A Codex task is already running".into());
    }
    if app.codex_runtime.pending_image.is_some() {
        return Err("Place or discard the previous generated image first".into());
    }
    if app.ui.codex.chat_prompt.trim().is_empty() {
        return Err("Enter a task for Codex".into());
    }
    if app.ui.codex.chat_prompt.len() > MAX_MESSAGE_BYTES {
        return Err("The Codex task exceeds the 128 KiB limit.".into());
    }
    if app.services.codex_chat.is_none() {
        return Err("Codex requires the native yorusphoto app".into());
    }
    let reference = if app.ui.codex.selection_reference && !app.session.active().is_some_and(|d| d.doc.selection.is_some()) {
        // Text editing tasks can begin without an image destination. Image results wait for
        // an explicit destination instead of silently opening a new canvas.
        app.codex_runtime.placement = None;
        None
    } else {
        capture(app, app.ui.codex.selection_reference)?
    };
    let cancel = Arc::new(AtomicBool::new(false));
    let prompt = app.ui.codex.chat_prompt.clone();
    let req = CodexChatRequest {
        prompt: prompt.clone(),
        thread_id: app.ui.codex.thread_id.clone(),
        codex_path: app.ui.codex.codex_path.clone(),
        reference,
        cancel: cancel.clone(),
        model: app.ui.codex.model.clone(),
        reasoning_effort: app.ui.codex.reasoning_effort.clone(),
    };
    let (event_tx, event_rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();
    if let Some(run) = &app.services.codex_chat {
        run(req, event_tx, command_tx);
    }
    app.codex_runtime.chat_rx = Some(event_rx);
    app.codex_runtime.command_rx = Some(command_rx);
    app.codex_runtime.cancel = Some(cancel);
    app.ui.codex.chat_prompt.clear();
    app.ui.codex.messages.push(Message { role: "You".into(), text: prompt });
    app.ui.codex.messages.push(Message { role: "Codex".into(), text: String::new() });
    if app.ui.codex.messages.len() > 100 {
        app.ui.codex.messages.drain(..2);
    }
    status(app, "Codex is working on your taskâ€¦", false);
    Ok(())
}

fn poll(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.codex_runtime.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
        stop_tool(app);
    }
    let jobs = app.session.jobs_with_recent();
    for (job, reply) in std::mem::take(&mut app.codex_runtime.waiting_tools) {
        let info = jobs.iter().find(|info| info.id == job);
        if let Some(info) = info {
            if info.state != "running" {
                let result = match info.state {
                    "done" => Ok(info.result.clone().unwrap_or(Value::Null)),
                    _ => Err(info.error.clone().unwrap_or_else(|| format!("Editor job {}", info.state))),
                };
                let _ = reply.send(result);
            } else {
                app.codex_runtime.waiting_tools.push((job, reply));
            }
        } else {
            let _ = reply.send(Err("The editor job is no longer available".into()));
        }
    }
    // All live document operations run on the UI/engine thread, never on the transport worker.
    for _ in 0..8 {
        let request = app.codex_runtime.command_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        let Some(request) = request else {
            break;
        };
        let previous_automation = std::mem::replace(&mut app.automation_input, true);
        let result = if app.codex_runtime.cancel.as_ref().is_some_and(|c| c.load(Ordering::Relaxed)) {
            Err("Task stopped".into())
        } else if request.command == "__list_commands" {
            crate::control::command_list(app).map(|commands| {
                let document = if app.session.active().is_some() { app.run("document.inspect", json!({})).unwrap_or(Value::Null) } else { Value::Null };
                json!({"commands":commands.get("commands").cloned().unwrap_or(commands),"document":document,
                    "fileTools":[{"id":"app.open","params":{"path":"relative file inside the granted workspace"}},
                        {"id":"app.save","params":{"path":"relative output file inside the granted workspace"}}]})
            })
        } else if matches!(request.command.as_str(), "app.open" | "app.save") {
            let (control, _) = crate::control::ControlRequest::new(&request.command, request.params);
            match crate::control::handle(app, ctx, &control) {
                crate::control::Outcome::Done(value) if value.get("ok").and_then(Value::as_bool) == Some(true) => {
                    Ok(value.get("result").cloned().unwrap_or(Value::Null))
                }
                crate::control::Outcome::Done(value) => Err(value.get("error").and_then(Value::as_str).unwrap_or("File operation failed").into()),
                crate::control::Outcome::AfterJob(job) => Ok(json!({"job":job.0,"pending":true})),
                _ => Err("The file operation needs interactive input".into()),
            }
        } else if photocraft_engine::commands::find(&request.command).is_some() {
            app.run(&request.command, request.params)
        } else {
            crate::menus::invoke(app, ctx, &request.command, request.params)
        };
        app.automation_input = previous_automation;
        if let Ok(value) = &result
            && value.get("pending").and_then(Value::as_bool) == Some(true)
            && let Some(job) = value.get("job").and_then(Value::as_u64)
        {
            app.codex_runtime.waiting_tools.push((photocraft_engine::jobs::JobId(job), request.reply));
            continue;
        }
        let _ = request.reply.send(result);
    }
    let image = app.codex_runtime.image_rx.as_ref().map(Receiver::try_recv);
    match image {
        Some(Ok(result)) => {
            app.codex_runtime.image_rx = None;
            match result {
                Ok(bytes) => receive_image(app, bytes),
                Err(e) => status(app, e, true),
            }
        }
        Some(Err(TryRecvError::Disconnected)) => {
            app.codex_runtime.image_rx = None;
            status(app, "Codex image worker ended without a result", true);
        }
        _ => {}
    }
    for _ in 0..64 {
        let event = app.codex_runtime.chat_rx.as_ref().map(Receiver::try_recv);
        match event {
            Some(Ok(CodexEvent::Started(id))) => app.ui.codex.thread_id = Some(id),
            Some(Ok(CodexEvent::Telemetry { kind, data })) => apply_telemetry(app, &kind, &data),
            Some(Ok(CodexEvent::Text(text))) => {
                if let Some(m) = app.ui.codex.messages.last_mut()
                    && m.text.len().saturating_add(text.len()) <= 128 * 1024
                {
                    m.text.push_str(&text);
                }
            }
            Some(Ok(CodexEvent::Status(text))) => status(app, text, false),
            Some(Ok(CodexEvent::Image(bytes))) => receive_image(app, bytes),
            Some(Ok(CodexEvent::Finished)) => {
                app.codex_runtime.chat_rx = None;
                app.codex_runtime.command_rx = None;
                if !app.ui.codex.error {
                    status(app, "Task finished.", false);
                }
                break;
            }
            Some(Ok(CodexEvent::Error(text))) => {
                if app.codex_runtime.workspace.compaction == workspace::Compaction::Running {
                    app.codex_runtime.workspace.apply_compaction(workspace::Compaction::Failed(text.clone()), unix_now());
                }
                stop_tool(app);
                app.codex_runtime.chat_rx = None;
                app.codex_runtime.command_rx = None;
                status(app, text, true);
                break;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                stop_tool(app);
                app.codex_runtime.chat_rx = None;
                app.codex_runtime.command_rx = None;
                if app.codex_runtime.workspace.compaction == workspace::Compaction::Running {
                    app.codex_runtime
                        .workspace
                        .apply_compaction(workspace::Compaction::Failed("The Codex connection closed during compaction.".into()), unix_now());
                }
                status(app, "Codex connection closed", true);
                break;
            }
            _ => break,
        }
    }
    for _ in 0..8 {
        let event = app.codex_runtime.login_rx.as_ref().map(Receiver::try_recv);
        match event {
            Some(Ok(CodexLoginEvent::OpeningBrowser { url })) => {
                app.codex_runtime.login_url = Some(url);
                status(app, "Opening the sign-in browser. You can also use the link above.", false);
            }
            Some(Ok(CodexLoginEvent::BrowserError(error))) => {
                app.codex_runtime.browser_error = Some(error);
                status(app, "The browser did not open automatically. Use Open sign-in page or Copy link above.", true);
            }
            Some(Ok(CodexLoginEvent::Connected)) => {
                app.codex_runtime.login_rx = None;
                app.codex_runtime.login_cancel = None;
                app.codex_runtime.login_url = None;
                app.codex_runtime.browser_error = None;
                app.codex_runtime.connected = true;
                app.codex_runtime.next_workspace_refresh = 0.0;
                status(app, "Connected to your ChatGPT account through Codex. Your existing session can be reused.", false);
                break;
            }
            Some(Ok(CodexLoginEvent::Error(error))) => {
                app.codex_runtime.login_rx = None;
                app.codex_runtime.login_cancel = None;
                app.codex_runtime.login_url = None;
                app.codex_runtime.browser_error = None;
                status(app, error, true);
                break;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                app.codex_runtime.login_rx = None;
                app.codex_runtime.login_cancel = None;
                app.codex_runtime.login_url = None;
                app.codex_runtime.browser_error = None;
                status(app, "Sign-in connection closed. Try connecting again.", true);
                break;
            }
            _ => break,
        }
    }
    poll_workspace(app);
}

fn unix_now() -> u64 {
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |time| time.as_secs())
    }
    #[cfg(target_arch = "wasm32")]
    {
        0
    }
}

fn apply_telemetry(app: &mut PhotocraftApp, kind: &str, data: &Value) {
    if let Some(thread) = data.get("threadId").and_then(Value::as_str)
        && app.ui.codex.thread_id.as_deref().is_some_and(|current| current != thread)
    {
        return;
    }
    let now = unix_now();
    let result = match kind {
        "tokenUsage" => app.codex_runtime.workspace.apply_token_usage(data, now),
        "rateLimits" => app.codex_runtime.workspace.apply_rate_limits(data, now),
        "compaction" => {
            let state = match data.get("state").and_then(Value::as_str) {
                Some("running") => workspace::Compaction::Running,
                Some("complete") => workspace::Compaction::Complete,
                Some("failed") => workspace::Compaction::Failed("Codex could not finish compaction.".into()),
                _ => return,
            };
            app.codex_runtime.workspace.apply_compaction(state, now);
            Ok(())
        }
        "model" => {
            if let Some(model) = data.get("model").and_then(Value::as_str).filter(|value| value.len() <= 256) {
                app.ui.codex.model = Some(model.into());
            }
            if let Some(effort) = data.get("reasoningEffort").and_then(Value::as_str).filter(|value| value.len() <= 64) {
                app.ui.codex.reasoning_effort = Some(effort.into());
            }
            Ok(())
        }
        _ => return,
    };
    if let Err(error) = result {
        app.codex_runtime.workspace_error = Some(error);
    }
}

fn request_workspace(app: &mut PhotocraftApp, action: CodexWorkspaceAction) -> Result<(), String> {
    if app.codex_runtime.workspace_rx.is_some() || app.codex_runtime.login_rx.is_some() {
        return Err("Wait for the current account or chat operation to finish.".into());
    }
    if !matches!(action, CodexWorkspaceAction::Refresh | CodexWorkspaceAction::ListThreads { .. }) && !chats::can_switch(app) {
        return Err("Finish the active task and place any retained image before changing chats.".into());
    }
    let run = app.services.codex_workspace.as_ref().ok_or("Codex workspace controls require the native app.")?;
    let cancel = Arc::new(AtomicBool::new(false));
    let request = CodexWorkspaceRequest {
        codex_path: app.ui.codex.codex_path.clone(),
        model: app.ui.codex.model.clone(),
        reasoning_effort: app.ui.codex.reasoning_effort.clone(),
        cancel: cancel.clone(),
        action: action.clone(),
    };
    let (reply, receiver) = mpsc::channel();
    run(request, reply);
    if matches!(action, CodexWorkspaceAction::CompactThread { .. }) {
        app.codex_runtime.workspace.apply_compaction(workspace::Compaction::Running, unix_now());
        status(app, "Compacting the current conversation…", false);
    }
    app.codex_runtime.workspace_rx = Some(receiver);
    app.codex_runtime.workspace_action = Some(action);
    app.codex_runtime.workspace_cancel = Some(cancel);
    app.codex_runtime.workspace_error = None;
    Ok(())
}

fn poll_workspace(app: &mut PhotocraftApp) {
    for _ in 0..32 {
        let event = app.codex_runtime.workspace_rx.as_ref().map(Receiver::try_recv);
        match event {
            Some(Ok(CodexWorkspaceEvent::Telemetry { kind, data })) => apply_telemetry(app, &kind, &data),
            Some(Ok(CodexWorkspaceEvent::Result(data))) => {
                app.codex_runtime.workspace_rx = None;
                app.codex_runtime.workspace_cancel = None;
                let action = app.codex_runtime.workspace_action.take();
                if let Err(error) = apply_workspace_result(app, action, &data) {
                    app.codex_runtime.workspace_error = Some(error.clone());
                    status(app, error, true);
                }
                break;
            }
            Some(Ok(CodexWorkspaceEvent::Error(error))) => {
                let compact = matches!(app.codex_runtime.workspace_action, Some(CodexWorkspaceAction::CompactThread { .. }));
                app.codex_runtime.workspace_rx = None;
                app.codex_runtime.workspace_action = None;
                app.codex_runtime.workspace_cancel = None;
                if compact {
                    app.codex_runtime.workspace.apply_compaction(workspace::Compaction::Failed(error.clone()), unix_now());
                }
                app.codex_runtime.workspace_error = Some(error.clone());
                status(app, error, true);
                break;
            }
            Some(Err(TryRecvError::Disconnected)) => {
                if matches!(app.codex_runtime.workspace_action, Some(CodexWorkspaceAction::CompactThread { .. })) {
                    app.codex_runtime
                        .workspace
                        .apply_compaction(workspace::Compaction::Failed("The Codex connection closed during compaction.".into()), unix_now());
                }
                app.codex_runtime.workspace_rx = None;
                app.codex_runtime.workspace_action = None;
                app.codex_runtime.workspace_cancel = None;
                app.codex_runtime.workspace_error = Some("Codex closed the workspace connection. Refresh to try again.".into());
                break;
            }
            _ => break,
        }
    }
}

fn apply_workspace_result(app: &mut PhotocraftApp, action: Option<CodexWorkspaceAction>, data: &Value) -> Result<(), String> {
    match action {
        Some(CodexWorkspaceAction::Refresh) => {
            let now = unix_now();
            let mut errors = Vec::new();
            if let Some(models) = data.get("models").filter(|value| !value.is_null())
                && let Err(error) = app.codex_runtime.workspace.apply_models(models, now)
            {
                errors.push(error);
            }
            if let Some(account) = data.get("account").filter(|value| !value.is_null())
                && let Err(error) = app.codex_runtime.workspace.apply_account(account, now)
            {
                errors.push(error);
            }
            if let Some(limits) = data.get("rateLimits").filter(|value| !value.is_null())
                && let Err(error) = app.codex_runtime.workspace.apply_rate_limits(limits, now)
            {
                errors.push(error);
            }
            errors.extend(data.get("errors").and_then(Value::as_array).into_iter().flatten().take(8).filter_map(Value::as_str).map(str::to_owned));
            app.codex_runtime.workspace_error = (!errors.is_empty()).then(|| errors.join("\n"));
        }
        Some(CodexWorkspaceAction::ReadThread { .. } | CodexWorkspaceAction::ForkThread { .. }) => {
            chats::import_backend(app, data)?;
            status(app, "Saved Codex conversation opened.", false);
        }
        Some(CodexWorkspaceAction::ListThreads { cursor, .. }) => {
            if cursor.is_none() {
                app.codex_runtime.remote_threads.clear();
            }
            if let Some(threads) = data.get("data").and_then(Value::as_array) {
                app.codex_runtime.remote_threads.extend(threads.iter().take(100).cloned());
                app.codex_runtime.remote_threads.truncate(200);
            }
            app.codex_runtime.remote_cursor = data.get("nextCursor").and_then(Value::as_str).map(str::to_owned);
            app.codex_runtime.remote_browser = true;
        }
        Some(CodexWorkspaceAction::CompactThread { .. }) => {
            if app.codex_runtime.workspace.compaction != workspace::Compaction::Complete {
                app.codex_runtime.workspace.apply_compaction(workspace::Compaction::Complete, unix_now());
            }
            status(app, "Conversation compacted. The saved transcript is preserved.", false);
            save_conversation(app);
        }
        Some(CodexWorkspaceAction::RenameThread { .. } | CodexWorkspaceAction::ArchiveThread { .. }) | None => {}
    }
    Ok(())
}

fn open_remote_chats(app: &mut PhotocraftApp) {
    app.codex_runtime.remote_browser = true;
    if let Err(error) = request_workspace(app, CodexWorkspaceAction::ListThreads { cursor: None, archived: false, search: None }) {
        status(app, error, true);
    }
}

fn workspace_actions(app: &mut PhotocraftApp, actions: Vec<workspace::Action>) {
    for action in actions {
        let action = match action {
            workspace::Action::Refresh => Ok(CodexWorkspaceAction::Refresh),
            workspace::Action::Compact => commands::current_thread(app).map(|thread_id| CodexWorkspaceAction::CompactThread { thread_id }),
        };
        if let Err(error) = action.and_then(|action| request_workspace(app, action)) {
            status(app, error, true);
        }
    }
}

fn workspace_windows(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.codex_runtime.show_workspace {
        let mut open = true;
        let mut actions = Vec::new();
        egui::Window::new("Codex status and usage").id(egui::Id::new("codex-workspace-status")).open(&mut open).default_width(430.0).show(ctx, |ui| {
            egui::ScrollArea::vertical().max_height(560.0).show(ui, |ui| {
                actions = workspace::usage_ui(ui, &app.codex_runtime.workspace, unix_now());
                if let Some(error) = &app.codex_runtime.workspace_error {
                    ui.colored_label(crate::ai_sidebar::palette(ui).warning, error);
                }
                ui.separator();
                commands::help_ui(ui);
            });
        });
        app.codex_runtime.show_workspace = open;
        workspace_actions(app, actions);
    }
    if app.codex_runtime.show_models {
        let mut open = true;
        let mut model = app.ui.codex.model.clone();
        let mut effort = app.ui.codex.reasoning_effort.clone();
        let mut changed = false;
        let mut refresh = false;
        egui::Window::new("Codex model").id(egui::Id::new("codex-model-picker")).open(&mut open).default_width(360.0).show(ctx, |ui| {
            changed = workspace::model_ui(ui, &app.codex_runtime.workspace, &mut model, &mut effort, app.codex_runtime.busy());
            refresh = ui.button("Refresh available models").clicked();
        });
        app.codex_runtime.show_models = open;
        if changed && let Err(error) = commands::set_model(app, model, effort) {
            status(app, error, true);
        }
        if refresh && let Err(error) = request_workspace(app, CodexWorkspaceAction::Refresh) {
            status(app, error, true);
        }
    }
    if app.codex_runtime.remote_browser {
        let mut open = true;
        let mut action = None;
        egui::Window::new("Saved Codex threads").id(egui::Id::new("codex-remote-threads")).open(&mut open).default_width(450.0).show(ctx, |ui| {
            ui.small("Threads created by this editor. Opening imports their saved text and resumes the same Codex thread.");
            if ui.add_enabled(app.codex_runtime.workspace_rx.is_none(), egui::Button::new("Refresh threads")).clicked() {
                action = Some(CodexWorkspaceAction::ListThreads { cursor: None, archived: false, search: None });
            }
            egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                if app.codex_runtime.remote_threads.is_empty() {
                    ui.label("No saved threads loaded.");
                }
                for thread in &app.codex_runtime.remote_threads {
                    let Some(id) = thread.get("id").and_then(Value::as_str) else {
                        continue;
                    };
                    ui.horizontal(|ui| {
                        let name = thread
                            .get("name")
                            .and_then(Value::as_str)
                            .filter(|name| !name.is_empty())
                            .or_else(|| thread.get("preview").and_then(Value::as_str))
                            .unwrap_or(id);
                        ui.add(egui::Label::new(name).truncate()).on_hover_text(id);
                        if ui.add_enabled(chats::can_switch(app) && app.codex_runtime.workspace_rx.is_none(), egui::Button::new("Open")).clicked() {
                            action = Some(CodexWorkspaceAction::ReadThread { thread_id: id.into() });
                        }
                    });
                }
            });
            if let Some(cursor) = &app.codex_runtime.remote_cursor
                && ui.add_enabled(app.codex_runtime.workspace_rx.is_none(), egui::Button::new("Load more")).clicked()
            {
                action = Some(CodexWorkspaceAction::ListThreads { cursor: Some(cursor.clone()), archived: false, search: None });
            }
            if app.codex_runtime.workspace_rx.is_some() {
                ui.spinner();
            }
            if let Some(error) = &app.codex_runtime.workspace_error {
                ui.label(error);
            }
        });
        app.codex_runtime.remote_browser = open;
        if let Some(action) = action
            && let Err(error) = request_workspace(app, action)
        {
            status(app, error, true);
        }
    }
}

fn apply_ratio(app: &mut PhotocraftApp) -> Result<(), String> {
    let doc = app.session.active().ok_or("Open a canvas and select an area first")?;
    let area = doc.doc.selection.as_ref().ok_or("Select an area first")?.content_bounds().intersect(&doc.doc.bounds());
    if area.is_empty() {
        return Err("Selection is empty".into());
    }
    let aspect = match app.ui.codex.ratio.as_str() {
        "1:1" => 1.0,
        "4:3" => 4.0 / 3.0,
        "3:4" => 3.0 / 4.0,
        "16:9" => 16.0 / 9.0,
        "9:16" => 9.0 / 16.0,
        _ => return Ok(()),
    };
    let mut w = f64::from(area.width());
    let mut h = w / aspect;
    if h < f64::from(area.height()) {
        h = f64::from(area.height());
        w = h * aspect;
    }
    let k = (f64::from(doc.doc.size.width) / w).min(f64::from(doc.doc.size.height) / h).min(1.0);
    let w = (w * k).round().max(1.0);
    let h = (h * k).round().max(1.0);
    let x = ((f64::from(area.x0) + f64::from(area.x1) - w) / 2.0).round().clamp(0.0, f64::from(doc.doc.size.width) - w);
    let y = ((f64::from(area.y0) + f64::from(area.y1) - h) / 2.0).round().clamp(0.0, f64::from(doc.doc.size.height) - h);
    app.run("select.rect", json!({"x":x,"y":y,"width":w,"height":h,"mode":"replace"}))?;
    Ok(())
}

pub(crate) fn command_specs(app: &PhotocraftApp) -> Vec<Value> {
    let available = app.services.generate_image.is_some();
    let idle = !app.codex_runtime.busy() && !app.yorus_ai_runtime.busy() && app.codex_runtime.login_rx.is_none();
    let mut specs: Vec<Value> = [
        ("ai.generate", "Generate image with Codex", r#"{prompt?:string,mode?:"selection"|"canvas",quality?:"low"|"medium"|"high",width?:1..3840,height?:1..3840}; omit prompt to open Images. Quality and generation size are preferences; final placement is exact."#, available && idle),
        ("codex.send", "Send an editor task to Codex", r#"{prompt:string}; uses current Selected area setting. Starts asynchronously; inspect codexBusy and codex status via ui_inspect."#, app.services.codex_chat.is_some() && idle),
        ("codex.cancel", "Stop Codex task", "{}", app.codex_runtime.busy()),
    ].into_iter().map(|(id,label,params,enabled)| json!({"id":id,"label":label,"params":params,"menu":[],"shortcut":null,
        "enabled":enabled,"disabledReason":if enabled {Value::Null} else {json!("Codex service unavailable, busy, or no active task to stop")},"needsDocument":false})).collect();
    specs.extend(commands::specs(app));
    specs.extend(updates::specs(app));
    specs
}

pub fn menu(app: &mut PhotocraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == "ai.generate" && params.get("prompt").is_some() {
        return Some(configure_generation(app, params).and_then(|()| generate(app)).map(|()| json!({"started":true})));
    }
    match id {
        "window.codex" | "ai.generate" => {
            crate::ai_sidebar::open(app, crate::ai_sidebar::Tab::Codex);
            app.ui.codex.images_tab = id == "ai.generate";
            Some(Ok(json!({"open":true})))
        }
        "codex.login" => Some(start_login(app).map(|()| json!({"started":true}))),
        "codex.cancel" => {
            stop(app);
            Some(Ok(json!({"cancelled":true})))
        }
        "codex.send" => {
            if let Some(prompt) = params.get("prompt").and_then(Value::as_str) {
                app.ui.codex.chat_prompt = prompt.into();
            }
            Some(send_chat(app).map(|_| json!({"started":true})))
        }
        _ => updates::menu(app, id, params).or_else(|| commands::menu(app, id, params)),
    }
}

pub(crate) fn workspace_inspect(app: &PhotocraftApp) -> Value {
    json!({"workspace":app.codex_runtime.workspace.snapshot(),"model":app.ui.codex.model,"reasoningEffort":app.ui.codex.reasoning_effort,
        "chats":chats::inspect(app),"refreshing":app.codex_runtime.workspace_rx.is_some(),"error":app.codex_runtime.workspace_error,
        "remoteThreads":app.codex_runtime.remote_threads,"nextCursor":app.codex_runtime.remote_cursor,"updates":updates::inspect(app)})
}

fn configure_generation(app: &mut PhotocraftApp, params: &Value) -> Result<(), String> {
    let mut settings = app.ui.codex.clone();
    settings.prompt = params.get("prompt").and_then(Value::as_str).ok_or("prompt must be text")?.into();
    if let Some(mode) = params.get("mode") {
        settings.entire_image = match mode.as_str() {
            Some("selection") => false,
            Some("canvas") => true,
            _ => return Err("mode must be selection or canvas".into()),
        };
    }
    if let Some(quality) = params.get("quality") {
        settings.quality = match quality.as_str() {
            Some(q @ ("low" | "medium" | "high")) => q.into(),
            _ => return Err("quality must be low, medium or high".into()),
        };
    }
    for (key, output) in [("width", &mut settings.width), ("height", &mut settings.height)] {
        if let Some(value) = params.get(key) {
            *output = value.as_u64().filter(|v| (1..=3840).contains(v)).ok_or("width and height must be integers between 1 and 3840")? as u32;
        }
    }
    settings.open = true;
    settings.images_tab = true;
    app.ui.codex = settings;
    Ok(())
}

pub fn show(app: &mut PhotocraftApp, ctx: &egui::Context) {
    if app.ui.codex.sidebar_version == 0 {
        app.ui.codex.sidebar_version = 1;
        app.ui.codex.open = true;
        app.ui.codex.images_tab = false;
        app.ui.panels.rail = true;
        app.ui.panels.dock = true;
    }
    let was_busy = app.codex_runtime.busy();
    poll(app, ctx);
    let now = ctx.input(|input| input.time);
    updates::poll(app, now);
    if now >= app.codex_runtime.next_history_check || (was_busy && !app.codex_runtime.busy()) {
        save_conversation(app);
        app.codex_runtime.next_history_check = now + 1.0;
    }
    if was_busy && !app.codex_runtime.busy() {
        app.codex_runtime.next_workspace_refresh = 0.0;
    }
    if app.services.codex_workspace.is_some()
        && app.codex_runtime.workspace_rx.is_none()
        && app.codex_runtime.login_rx.is_none()
        && !app.codex_runtime.busy()
        && now >= app.codex_runtime.next_workspace_refresh
    {
        app.codex_runtime.next_workspace_refresh = now + 60.0;
        if let Err(error) = request_workspace(app, CodexWorkspaceAction::Refresh) {
            app.codex_runtime.workspace_error = Some(error);
        }
    }
    if app.codex_runtime.history_writable || app.services.codex_workspace.is_some() || app.services.codex_update.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_secs(1));
    }
    if app.codex_runtime.busy() || app.codex_runtime.login_rx.is_some() || app.codex_runtime.workspace_rx.is_some() || app.codex_runtime.updates.running() {
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
    }
    workspace_windows(app, ctx);
    updates::window(app, ctx);
}

/// The docked assistant body. Async work is polled independently of panel visibility.
pub fn panel(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::ai_sidebar::palette(ui);
    let busy = app.codex_runtime.busy();
    ui.horizontal(|ui| {
        ui.selectable_value(&mut app.ui.codex.images_tab, false, "Assistant");
        ui.selectable_value(&mut app.ui.codex.images_tab, true, "Images");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let settings = crate::icons::button(ui, "settings", 24.0, false, "ChatGPT connection");
            egui::Popup::menu(&settings).show(|ui| connection_ui(app, ui));
            if crate::icons::button(ui, "clock", 24.0, app.codex_runtime.chats.open, "Saved chats").clicked() {
                app.codex_runtime.chats.open = !app.codex_runtime.chats.open;
                if app.codex_runtime.chats.open {
                    app.ui.codex.images_tab = false;
                }
            }
            if ui
                .add_enabled(chats::can_switch(app), egui::Button::image(crate::icons::image("square-pen", 17.0, t.text_dim)).frame(false))
                .on_hover_text("New conversation")
                .clicked()
                && let Err(error) = chats::new_chat(app)
            {
                status(app, error, true);
            }
        });
    });
    ui.add_space(4.0);
    login_ui(app, ui);
    let previous_model = app.ui.codex.model.clone();
    let previous_effort = app.ui.codex.reasoning_effort.clone();
    if workspace::model_ui(ui, &app.codex_runtime.workspace, &mut app.ui.codex.model, &mut app.ui.codex.reasoning_effort, busy) {
        let model = app.ui.codex.model.clone();
        let effort = app.ui.codex.reasoning_effort.clone();
        app.ui.codex.model = previous_model;
        app.ui.codex.reasoning_effort = previous_effort;
        if let Err(error) = commands::set_model(app, model, effort) {
            status(app, error, true);
        }
    }
    let actions = workspace::status_ui(ui, &app.codex_runtime.workspace, unix_now());
    workspace_actions(app, actions);
    let body_height = ui.available_height().max(0.0);
    let footer_height = if app.ui.codex.status.is_empty() { 0.0 } else { 90.0 } + if app.codex_runtime.pending_image.is_some() { 145.0 } else { 0.0 };
    egui::ScrollArea::vertical().id_salt("codex-panel-body").max_height(body_height).auto_shrink([false, false]).show(ui, |ui| {
        if app.ui.codex.images_tab {
            images_ui(app, ui);
        } else {
            chat_ui(app, ui, (body_height - footer_height - 205.0).max(0.0));
        }
        if !app.ui.codex.status.is_empty() {
            egui::ScrollArea::vertical().id_salt("codex-status").max_height(78.0).show(ui, |ui| {
                ui.label(egui::RichText::new(&app.ui.codex.status).size(11.5).color(if app.ui.codex.error { t.warning } else { t.text_dim }));
            });
        }
        recovery_ui(app, ui);
        if let Some(error) = &app.codex_runtime.history_error {
            ui.label(egui::RichText::new(format!("Conversation not saved: {error}")).small().color(t.warning));
        } else if app.codex_runtime.history_writable {
            ui.label(egui::RichText::new("Chat history saves on this device.").small().color(t.text_faint));
        }
    });
}

fn start_login(app: &mut PhotocraftApp) -> Result<(), String> {
    if app.codex_runtime.busy() || app.codex_runtime.login_rx.is_some() {
        return Err("Wait for the current Codex operation to finish.".into());
    }
    let login = app.services.codex_login.as_ref().ok_or("The Codex sign-in service is unavailable.")?;
    let (tx, rx) = mpsc::channel();
    let cancel = Arc::new(AtomicBool::new(false));
    login(app.ui.codex.codex_path.clone(), cancel.clone(), tx);
    app.codex_runtime.login_rx = Some(rx);
    app.codex_runtime.login_cancel = Some(cancel);
    app.codex_runtime.login_url = None;
    app.codex_runtime.browser_error = None;
    status(app, "Checking your Codex session and preparing sign-in…", false);
    Ok(())
}

fn login_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.codex_runtime.login_rx.is_none() {
        return;
    }
    ui.horizontal(|ui| {
        ui.spinner();
        ui.strong("Connecting to ChatGPT…");
    });
    if let Some(error) = &app.codex_runtime.browser_error {
        ui.small(error);
    }
    let url = app.codex_runtime.login_url.clone();
    ui.horizontal_wrapped(|ui| {
        if let Some(url) = &url {
            ui.hyperlink_to("Open sign-in page", url);
            if ui.small_button("Copy link").clicked() {
                ui.ctx().copy_text(url.clone());
            }
        } else {
            ui.small("Waiting for the Codex sign-in page…");
        }
        if ui.small_button("Cancel sign-in").clicked() {
            if let Some(cancel) = &app.codex_runtime.login_cancel {
                cancel.store(true, Ordering::Relaxed);
            }
            status(app, "Cancelling sign-in…", false);
        }
    });
    ui.separator();
}

fn connection_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    ui.set_min_width(235.0);
    ui.strong("ChatGPT connection");
    ui.label("Uses your Codex / ChatGPT sign-in.");
    if app.codex_runtime.connected {
        ui.strong("ChatGPT session confirmed");
    }
    if ui
        .add_enabled(
            !app.codex_runtime.busy() && app.codex_runtime.login_rx.is_none() && app.services.codex_login.is_some(),
            egui::Button::new("Continue with ChatGPT"),
        )
        .clicked()
        && let Err(error) = start_login(app)
    {
        status(app, error, true);
    }
    ui.collapsing("Advanced", |ui| {
        ui.small("Codex executable; leave empty for automatic detection.");
        ui.text_edit_singleline(&mut app.ui.codex.codex_path);
    });
    ui.separator();
    updates::settings_ui(app, ui);
}

fn stop(app: &mut PhotocraftApp) {
    if let Some(cancel) = &app.codex_runtime.workspace_cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    if let Some(cancel) = &app.codex_runtime.login_cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    if let Some(cancel) = &app.codex_runtime.cancel {
        cancel.store(true, Ordering::Relaxed);
    }
    status(app, "Stopping Codex…", false);
}

fn recovery_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    if app.codex_runtime.pending_image.is_none() {
        return;
    }
    ui.separator();
    ui.label(format!("{} result(s) ready to place", app.codex_runtime.additional_images.len() + 1));
    if ui.button("Retry placement in original canvas").clicked()
        && let Some(bytes) = app.codex_runtime.pending_image.take()
    {
        receive_image(app, bytes);
        recover_next_image(app);
    }
    if ui.button("Open retained results in new canvases").clicked() {
        app.codex_runtime.placement = Some(Placement { target: None, width: app.ui.codex.width, height: app.ui.codex.height, name: "Generated image".into() });
        let mut images = std::mem::take(&mut app.codex_runtime.additional_images);
        if let Some(bytes) = app.codex_runtime.pending_image.take() {
            images.push_front(bytes);
        }
        for bytes in images {
            receive_image(app, bytes);
        }
        recover_next_image(app);
    }
    if ui.small_button("Discard retained results").clicked() {
        app.codex_runtime.pending_image = None;
        app.codex_runtime.additional_images.clear();
    }
}

fn images_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui) {
    let t = crate::ai_sidebar::palette(ui);
    let busy = app.codex_runtime.busy();
    ui.horizontal_wrapped(|ui| {
        ui.selectable_value(&mut app.ui.codex.entire_image, false, "Selected area");
        ui.selectable_value(&mut app.ui.codex.entire_image, true, "New canvas");
    });
    let selected = !app.ui.codex.entire_image;
    if selected {
        let rect = app.session.active().and_then(|d| d.doc.selection.as_ref().map(|s| s.content_bounds().intersect(&d.doc.bounds())));
        if let Some(r) = rect.filter(|r| !r.is_empty()) {
            ui.label(egui::RichText::new(format!("New layer · {} × {} px at {}, {}", r.width(), r.height(), r.x0, r.y0)).small().color(t.text_dim));
        } else {
            ui.label(egui::RichText::new("Select an area on the canvas first.").small().color(t.text_dim));
        }
    } else {
        ui.label(egui::RichText::new("The result opens in a separate canvas.").small().color(t.text_dim));
    }
    ui.add_space(4.0);
    egui::Frame::new().fill(t.field).corner_radius(18.0).inner_margin(12).show(ui, |ui| {
        ui.add(
            egui::TextEdit::multiline(&mut app.ui.codex.prompt)
                .hint_text("Describe the image you want to create…")
                .frame(egui::Frame::NONE)
                .desired_rows(5)
                .desired_width(f32::INFINITY)
                .char_limit(16000),
        );
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new(if selected { "Selection attached" } else { "Create an image" }).small().color(t.text_dim));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if busy {
                    if crate::ai_sidebar::send_button(ui, "square", true, "Stop generation").clicked() {
                        stop(app);
                    }
                } else {
                    let enabled = app.services.generate_image.is_some()
                        && !app.ui.codex.prompt.trim().is_empty()
                        && !app.yorus_ai_runtime.busy()
                        && (!selected || app.session.active().is_some_and(|d| d.doc.selection.is_some()));
                    if crate::ai_sidebar::send_button(ui, "arrow-up", enabled, "Generate image").clicked()
                        && let Err(error) = generate(app)
                    {
                        status(app, error, true);
                    }
                }
            });
        });
    });
    ui.add_space(4.0);
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Ratio").small().color(t.text_dim));
        egui::ComboBox::from_id_salt("generation-ratio").width(84.0).selected_text(&app.ui.codex.ratio).show_ui(ui, |ui| {
            for ratio in ["Selection", "1:1", "4:3", "3:4", "16:9", "9:16"] {
                ui.selectable_value(&mut app.ui.codex.ratio, ratio.into(), ratio);
            }
        });
        if ui.small_button("Apply ratio").clicked() {
            if selected {
                if let Err(error) = apply_ratio(app) {
                    status(app, error, true);
                }
            } else {
                let ratio = match app.ui.codex.ratio.as_str() {
                    "4:3" => 4.0 / 3.0,
                    "3:4" => 3.0 / 4.0,
                    "16:9" => 16.0 / 9.0,
                    "9:16" => 9.0 / 16.0,
                    _ => 1.0,
                };
                let width = f64::from(app.ui.codex.width);
                let height = width / ratio;
                let scale = (3840.0 / height.max(width)).min(1.0);
                app.ui.codex.width = (width * scale).round().max(1.0) as u32;
                app.ui.codex.height = (height * scale).round().max(1.0) as u32;
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new("Quality preference").small().color(t.text_dim));
        egui::ComboBox::from_id_salt("generation-quality").width(76.0).selected_text(&app.ui.codex.quality).show_ui(ui, |ui| {
            for quality in ["low", "medium", "high"] {
                ui.selectable_value(&mut app.ui.codex.quality, quality.into(), quality);
            }
        });
    });
    ui.horizontal_wrapped(|ui| {
        ui.label(egui::RichText::new(if selected { "Preferred long edge" } else { "Canvas size" }).small().color(t.text_dim));
        ui.add(egui::DragValue::new(&mut app.ui.codex.width).range(64..=3840).suffix(" px"));
        if !selected {
            ui.label("×");
            ui.add(egui::DragValue::new(&mut app.ui.codex.height).range(64..=3840).suffix(" px"));
        }
    });
    ui.small(egui::RichText::new("Codex chooses native quality and size automatically. Final layer and canvas dimensions stay exact.").color(t.text_faint));
    if busy {
        ui.horizontal(|ui| {
            ui.spinner();
            ui.label("Creating your image…");
        });
    }
}

fn chat_ui(app: &mut PhotocraftApp, ui: &mut egui::Ui, height: f32) {
    let t = crate::ai_sidebar::palette(ui);
    let busy = app.codex_runtime.busy();
    egui::ScrollArea::vertical()
        .id_salt(if app.codex_runtime.chats.open { "codex-chat-browser" } else { "codex-messages" })
        .max_height(height)
        .min_scrolled_height(height)
        .auto_shrink([false, false])
        .stick_to_bottom(!app.codex_runtime.chats.open)
        .show(ui, |ui| {
            ui.set_min_height(height);
            if app.codex_runtime.chats.open {
                chats::show_browser(app, ui);
            } else if app.ui.codex.messages.is_empty() {
                ui.add_space((height * 0.16).min(70.0));
                ui.vertical_centered(|ui| {
                    let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(68.0), egui::Sense::hover());
                    crate::ai_sidebar::gpt_mark(ui, rect);
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new("What would you like to create?").size(19.0).strong());
                    ui.add_space(4.0);
                    ui.label(egui::RichText::new("Your canvas, with a little help from Codex.").size(12.0).color(t.text_dim));
                    ui.add_space(18.0);
                    for (label, prompt) in [
                        ("Improve a selection", "Improve the selected area while keeping its composition."),
                        ("Help edit this canvas", "Inspect this canvas and help me improve it."),
                    ] {
                        if ui.add(egui::Button::new(label).fill(t.field).corner_radius(14.0).min_size(egui::vec2(205.0, 34.0))).clicked() {
                            app.ui.codex.chat_prompt = prompt.into();
                        }
                    }
                    if ui.add(egui::Button::new("Create a new image").fill(t.field).corner_radius(14.0).min_size(egui::vec2(205.0, 34.0))).clicked() {
                        app.ui.codex.images_tab = true;
                        app.ui.codex.entire_image = true;
                    }
                });
            } else {
                for message in &app.ui.codex.messages {
                    if message.role == "You" {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            egui::Frame::new().fill(t.field).corner_radius(16.0).inner_margin(12).show(ui, |ui| {
                                ui.set_max_width((ui.available_width() * 0.88).max(120.0));
                                ui.label(&message.text);
                            });
                        });
                    } else {
                        ui.horizontal(|ui| {
                            let (rect, _) = ui.allocate_exact_size(egui::Vec2::splat(24.0), egui::Sense::hover());
                            crate::ai_sidebar::gpt_mark(ui, rect.expand(3.0));
                            ui.label(egui::RichText::new("Codex").strong().size(12.0));
                        });
                        ui.label(if message.text.is_empty() { "Working…" } else { &message.text });
                    }
                    ui.add_space(16.0);
                }
            }
        });
    egui::Frame::new().fill(t.field).corner_radius(20.0).inner_margin(12).show(ui, |ui| {
        let response = ui.add(
            egui::TextEdit::multiline(&mut app.ui.codex.chat_prompt)
                .hint_text("Ask Codex anything about your canvas…")
                .frame(egui::Frame::NONE)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .char_limit(16000),
        );
        let submit = response.has_focus() && ui.input_mut(|input| input.consume_key(egui::Modifiers::COMMAND, egui::Key::Enter));
        let slash = app.ui.codex.chat_prompt.trim_start().starts_with('/');
        let enabled = app.services.codex_chat.is_some()
            && !app.ui.codex.chat_prompt.trim().is_empty()
            && (!busy || slash)
            && !app.yorus_ai_runtime.busy()
            && app.codex_runtime.login_rx.is_none();
        let mut send = submit && enabled;
        ui.horizontal(|ui| {
            ui.menu_button("/", |ui| {
                for (command, description) in commands::HELP {
                    if ui.button(*command).on_hover_text(*description).clicked() {
                        app.ui.codex.chat_prompt = command.split_whitespace().next().unwrap_or("/help").to_owned();
                        ui.close();
                    }
                }
            })
            .response
            .on_hover_text("Codex commands");
            ui.checkbox(&mut app.ui.codex.selection_reference, egui::RichText::new("Selected area").small())
                .on_hover_text("Use selected pixels as the reference and fixed layer destination. Uncheck to open generated images in a new canvas.");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if busy && !slash {
                    if crate::ai_sidebar::send_button(ui, "square", true, "Stop task").clicked() {
                        stop(app);
                    }
                } else {
                    send |= crate::ai_sidebar::send_button(ui, "arrow-up", enabled, "Send task · Ctrl+Enter").clicked();
                }
            });
        });
        if send && let Err(error) = send_chat(app) {
            status(app, error, true);
        }
    });
    ui.label(egui::RichText::new("Codex · your ChatGPT session").small().color(t.text_faint));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token_event(thread: &str, tokens: u64) -> Value {
        json!({"threadId":thread,"tokenUsage":{"last":{"totalTokens":tokens,"inputTokens":tokens,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0},
            "total":{"totalTokens":tokens,"inputTokens":tokens,"cachedInputTokens":0,"outputTokens":0,"reasoningOutputTokens":0},"modelContextWindow":10000}})
    }

    #[test]
    fn token_events_are_scoped_to_the_current_thread() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        app.ui.codex.thread_id = Some("current".into());
        apply_telemetry(&mut app, "tokenUsage", &token_event("other", 9000));
        assert!(app.codex_runtime.workspace.token_usage.is_none());
        apply_telemetry(&mut app, "tokenUsage", &token_event("current", 1500));
        assert_eq!(app.codex_runtime.workspace.token_usage.as_ref().unwrap().last.total_tokens, Some(1500));
    }

    #[test]
    fn saved_chat_browser_keeps_the_composer_visible_in_the_message_region() {
        for size in [egui::vec2(370.0, 650.0), egui::vec2(300.0, 520.0)] {
            let mut app = PhotocraftApp::new(Default::default(), Default::default());
            app.codex_runtime.chats.open = true;
            let ctx = egui::Context::default();
            PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Studio);
            let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, size);
            let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(screen), ..Default::default() }, |ui| panel(&mut app, ui));
            output.textures_delta.clear();
            let hints: Vec<_> = output
                .shapes
                .iter()
                .filter_map(|shape| match &shape.shape {
                    egui::Shape::Text(text) if text.galley.text().contains("Ask Codex anything") => Some((text.visual_bounding_rect(), shape.clip_rect)),
                    _ => None,
                })
                .collect();
            assert!(!hints.is_empty(), "composer hint must be rendered");
            for (hint, clip) in hints {
                assert!(hint.intersects(clip) && hint.bottom() <= screen.bottom(), "composer outsideviewport at{size:?}: {hint:?}");
            }
        }
    }

    #[test]
    fn manual_compaction_keeps_newer_token_events_and_polls_without_a_panel() {
        let services = crate::Services {
            codex_workspace: Some(Box::new(|request, reply| {
                assert!(matches!(request.action, CodexWorkspaceAction::CompactThread { .. }));
                reply.send(CodexWorkspaceEvent::Telemetry { kind: "compaction".into(), data: json!({"threadId":"current","state":"complete"}) }).unwrap();
                reply.send(CodexWorkspaceEvent::Telemetry { kind: "tokenUsage".into(), data: token_event("current", 123) }).unwrap();
                reply.send(CodexWorkspaceEvent::Result(json!({}))).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(Default::default(), services);
        app.ui.codex.thread_id = Some("current".into());
        app.ui.codex.open = false;
        request_workspace(&mut app, CodexWorkspaceAction::CompactThread { thread_id: "current".into() }).unwrap();
        assert!(app.codex_runtime.busy());
        poll(&mut app, &egui::Context::default());
        assert!(!app.codex_runtime.busy());
        assert_eq!(app.codex_runtime.workspace.compaction, workspace::Compaction::Complete);
        assert_eq!(app.codex_runtime.workspace.token_usage.as_ref().unwrap().last.total_tokens, Some(123));
        assert!(app.codex_runtime.workspace.tokens_observed_at_unix.is_some());
    }

    #[test]
    fn disconnected_compaction_does_not_leave_a_running_spinner() {
        let mut app = PhotocraftApp::new(Default::default(), crate::Services { codex_workspace: Some(Box::new(|_, _| {})), ..Default::default() });
        request_workspace(&mut app, CodexWorkspaceAction::CompactThread { thread_id: "current".into() }).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(matches!(app.codex_runtime.workspace.compaction, workspace::Compaction::Failed(_)));
        assert!(!app.codex_runtime.busy());
    }

    #[test]
    fn failed_catalog_refresh_does_not_discard_valid_usage_data() {
        let mut app = PhotocraftApp::new(Default::default(), Default::default());
        apply_workspace_result(&mut app,Some(CodexWorkspaceAction::Refresh),&json!({"models":{"data":"invalid"},"account":{"type":"chatgpt","planType":"pro"},"rateLimits":{"rateLimits":{"limitId":"codex","primary":{"usedPercent":95,"resetsAt":1900000000}}}})).unwrap();
        assert!(app.codex_runtime.workspace.account.is_some());
        assert!(!app.codex_runtime.workspace.rate_limits.is_empty());
        assert!(!app.codex_runtime.workspace.warnings().is_empty());
        assert!(app.codex_runtime.workspace_error.is_some());
    }

    #[test]
    fn selected_model_and_reasoning_reach_the_real_turn_request_seam() {
        let services = crate::Services {
            codex_chat: Some(Box::new(|request, events, _| {
                assert_eq!(request.model.as_deref(), Some("chosen-model"));
                assert_eq!(request.reasoning_effort.as_deref(), Some("high"));
                events.send(CodexEvent::Finished).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(Default::default(), services);
        app.ui.codex.model = Some("chosen-model".into());
        app.ui.codex.reasoning_effort = Some("high".into());
        app.ui.codex.chat_prompt = "Say hello".into();
        send_chat(&mut app).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(!app.codex_runtime.busy());
        assert!(!app.ui.codex.error);
    }

    #[test]
    fn sign_in_browser_failure_keeps_manual_link_and_cancel_runtime_only() {
        let saved = std::sync::Arc::new(std::sync::Mutex::new(None));
        let held = saved.clone();
        let services = crate::Services {
            codex_login: Some(Box::new(move |_, cancel, reply| {
                *held.lock().unwrap() = Some((cancel, reply.clone()));
                reply.send(CodexLoginEvent::OpeningBrowser { url: "https://auth.openai.com/authorize?state=runtime-only-state".into() }).unwrap();
                reply.send(CodexLoginEvent::BrowserError("No default browser is configured.".into())).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        start_login(&mut app).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(app.codex_runtime.login_rx.is_some());
        assert!(app.codex_runtime.login_url.is_some());
        assert!(app.ui.codex.status.contains("Copy link"));
        assert!(!serde_json::to_string(&app.ui).unwrap().contains("runtime-only-state"));
        assert!(!app.codex_runtime.connection().to_string().contains("runtime-only-state"));
        stop(&mut app);
        let state = saved.lock().unwrap();
        assert!(state.as_ref().unwrap().0.load(Ordering::Relaxed));
    }

    #[test]
    fn sign_in_confirms_existing_session_and_clears_transient_login_state() {
        let services = crate::Services {
            codex_login: Some(Box::new(|_, _, reply| {
                reply.send(CodexLoginEvent::Connected).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        start_login(&mut app).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(app.codex_runtime.connected);
        assert!(app.codex_runtime.login_rx.is_none());
        assert!(app.codex_runtime.login_url.is_none());
        assert!(app.ui.codex.status.contains("Connected"));
    }

    #[test]
    fn current_conversation_roundtrips_across_app_restart_with_no_credentials() {
        let disk = Arc::new(std::sync::Mutex::new(None::<String>));
        let services = |disk: Arc<std::sync::Mutex<Option<String>>>| {
            let read = disk.clone();
            crate::Services {
                load_codex_state: Some(Box::new(move || Ok(read.lock().unwrap().clone()))),
                save_codex_state: Some(Box::new(move |text| {
                    *disk.lock().unwrap() = Some(text.to_owned());
                    Ok(())
                })),
                ..Default::default()
            }
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services(disk.clone()));
        app.ui.codex.thread_id = Some("durable-thread".into());
        app.ui.codex.messages.push(Message { role: "You".into(), text: "Preserve the selected area.".into() });
        app.ui.codex.messages.push(Message { role: "Codex".into(), text: "Done.".into() });
        app.ui.codex.chat_prompt = "A draft for later".into();
        app.codex_runtime.login_url = Some("https://auth.openai.com/?state=must-not-save".into());
        save_conversation(&mut app);
        let text = disk.lock().unwrap().clone().unwrap();
        assert!(!text.contains("must-not-save"));
        let restored = PhotocraftApp::new(photocraft_engine::Session::new(), services(disk));
        assert_eq!(restored.ui.codex.thread_id.as_deref(), Some("durable-thread"));
        assert_eq!(restored.ui.codex.messages, app.ui.codex.messages);
        assert_eq!(restored.ui.codex.chat_prompt, "A draft for later");
        assert!(restored.codex_runtime.login_url.is_none());
    }

    #[test]
    fn malformed_history_is_not_replaced_and_write_failures_keep_last_snapshot() {
        let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writes = calls.clone();
        let services = crate::Services {
            load_codex_state: Some(Box::new(|| Ok(Some("bad history".into())))),
            save_codex_state: Some(Box::new(move |_| {
                writes.fetch_add(1, Ordering::Relaxed);
                Ok(())
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.ui.codex.chat_prompt = "new text".into();
        save_conversation(&mut app);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
        assert!(app.codex_runtime.history_error.is_some());
        app.codex_runtime.history_writable = true;
        let old = app.codex_runtime.history_snapshot.clone();
        app.services.save_codex_state = Some(Box::new(|_| Err("disk full".into())));
        save_conversation(&mut app);
        assert_eq!(app.codex_runtime.history_snapshot, old);
        assert_eq!(app.codex_runtime.history_error.as_deref(), Some("disk full"));
    }

    #[test]
    fn old_floating_panel_preferences_migrate_once_to_the_open_sidebar() {
        let old: State = serde_json::from_value(json!({"open":false,"images_tab":true,"prompt":"kept prompt"})).unwrap();
        assert_eq!(old.sidebar_version, 0);
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.ui.codex = old;
        let context = egui::Context::default();
        show(&mut app, &context);
        assert!(app.ui.codex.open);
        assert!(!app.ui.codex.images_tab);
        assert_eq!(app.ui.codex.prompt, "kept prompt");
        app.ui.codex.open = false;
        show(&mut app, &context);
        assert!(!app.ui.codex.open, "an intentional close survives later frames");
    }

    #[test]
    fn fresh_app_starts_with_assistant_visible_in_the_right_sidebar() {
        let state = State::default();
        assert!(state.open);
        assert!(!state.images_tab);
        assert_eq!(state.sidebar_version, 1);
    }

    #[test]
    fn short_narrow_assistant_keeps_status_and_recovery_inside_a_scrollable_panel() {
        for images in [false, true] {
            let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
            app.ui.codex.images_tab = images;
            app.ui.codex.status = "Result retained: the source canvas changed. Retry placement or open a new canvas.".repeat(4);
            app.codex_runtime.pending_image = Some(png());
            let ctx = egui::Context::default();
            PhotocraftApp::setup_context(&ctx, crate::theme::ThemeKind::Studio);
            let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(300.0, 370.0));
            let mut bounds = egui::Rect::NOTHING;
            let mut out = ctx.run_ui(egui::RawInput { screen_rect: Some(rect), ..Default::default() }, |ui| {
                panel(&mut app, ui);
                bounds = ui.min_rect();
            });
            out.textures_delta.clear();
            assert!(!out.shapes.is_empty());
            assert!(bounds.right() <= rect.right() + 1.0, "images={images}: {bounds:?}");
            assert!(bounds.bottom() <= rect.bottom() + 1.0, "images={images}: {bounds:?}");
            assert!(app.codex_runtime.pending_image.is_some(), "rendering must preserve recoverable artifacts");
        }
    }

    fn png() -> Vec<u8> {
        let image = photocraft_codecs::Image::from_raw(4, 4, photocraft_codecs::ChannelLayout::Rgba, photocraft_codecs::SampleType::U8, vec![255; 64]).unwrap();
        photocraft_codecs::encode(&image, photocraft_codecs::Format::Png, &Default::default()).unwrap()
    }

    #[test]
    fn async_image_uses_original_selection_and_new_layer() {
        let services = crate::Services {
            generate_image: Some(Box::new(|req, reply| {
                assert!(req.reference.is_some());
                reply.send(Ok(png())).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":40,"height":30})).unwrap();
        app.run("select.rect", json!({"x":5,"y":7,"width":12,"height":10})).unwrap();
        app.ui.codex.prompt = "a leaf".into();
        generate(&mut app).unwrap();
        app.run("select.rect", json!({"x":25,"y":20,"width":2,"height":2})).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.codex.error, "{}", app.ui.codex.status);
        let d = app.session.active().unwrap();
        assert_eq!(d.doc.layers.len(), 2);
        let l = d.doc.layer(d.active_layer.unwrap()).unwrap();
        assert_eq!(l.surface().unwrap().content_bounds(), photocraft_geom::Rect::new(5, 7, 17, 17));
    }

    #[test]
    fn entire_image_opens_an_independent_canvas_without_selection() {
        let services = crate::Services {
            generate_image: Some(Box::new(|req, reply| {
                assert!(req.reference.is_none());
                reply.send(Ok(png())).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":40,"height":30})).unwrap();
        let original = app.session.active().unwrap().doc.id;
        app.ui.codex.entire_image = true;
        app.ui.codex.width = 128;
        app.ui.codex.height = 96;
        app.ui.codex.prompt = "a leaf".into();
        generate(&mut app).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.codex.error, "{}", app.ui.codex.status);
        assert_eq!(app.session.documents().len(), 2);
        let d = app.session.active().unwrap();
        assert_ne!(d.doc.id, original);
        assert_eq!((d.doc.size.width, d.doc.size.height), (128, 96));
    }

    #[test]
    fn selected_area_requires_a_selection_even_for_programmatic_requests() {
        let services = crate::Services { generate_image: Some(Box::new(|_, _| panic!("must not start"))), ..Default::default() };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":40,"height":30})).unwrap();
        app.ui.codex.prompt = "a leaf".into();
        assert!(generate(&mut app).is_err());
        assert!(!app.codex_runtime.busy());
    }

    #[test]
    fn image_returns_to_original_document_after_tab_switch() {
        let services = crate::Services {
            generate_image: Some(Box::new(|_, reply| {
                reply.send(Ok(png())).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":40,"height":30})).unwrap();
        app.run("select.rect", json!({"x":5,"y":7,"width":12,"height":10})).unwrap();
        app.ui.codex.prompt = "a leaf".into();
        generate(&mut app).unwrap();
        app.run("file.new", json!({"width":20,"height":20})).unwrap();
        let other = app.session.active().unwrap().doc.id;
        poll(&mut app, &egui::Context::default());
        assert!(!app.ui.codex.error, "{}", app.ui.codex.status);
        assert_eq!(app.session.active().unwrap().doc.id, other);
        assert_eq!(app.session.documents().first().unwrap().doc.layers.len(), 2);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
    }

    #[test]
    fn placement_failure_retains_image_and_blocks_overwriting_it() {
        let services = crate::Services {
            generate_image: Some(Box::new(|_, reply| {
                reply.send(Ok(png())).unwrap();
            })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":40,"height":30})).unwrap();
        app.run("select.rect", json!({"x":5,"y":7,"width":12,"height":10})).unwrap();
        app.ui.codex.prompt = "a leaf".into();
        generate(&mut app).unwrap();
        app.run("image.canvasSize", json!({"width":50,"height":40})).unwrap();
        poll(&mut app, &egui::Context::default());
        assert!(app.ui.codex.error);
        assert!(app.codex_runtime.pending_image.is_some());
        assert!(generate(&mut app).is_err());
        app.run("edit.undo", json!({})).unwrap();
        let image = app.codex_runtime.pending_image.take().unwrap();
        receive_image(&mut app, image);
        assert!(!app.ui.codex.error, "{}", app.ui.codex.status);
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 2);
    }

    #[test]
    fn embedded_agent_obeys_the_same_command_gate_as_mcp() {
        let services = crate::Services {
            automation_command: Some(Box::new(|command, _| if command == "layer.new.layer" { Err("Denied by workspace policy".into()) } else { Ok(()) })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        app.run("file.new", json!({"width":20,"height":20})).unwrap();
        let (sender, receiver) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        sender.send(AgentCommand { command: "layer.new.layer".into(), params: json!({"name":"agent layer"}), reply }).unwrap();
        app.codex_runtime.command_rx = Some(receiver);
        poll(&mut app, &egui::Context::default());
        assert!(result.try_recv().unwrap().is_err());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 1);
        assert!(!app.automation_input);
        assert!(app.session.authorize.is_none());
    }

    #[test]
    fn embedded_agent_can_inspect_discovered_native_tools_and_obeys_shell_policy() {
        let services = crate::Services {
            automation_command: Some(Box::new(|command, _| if command == "yorusai.cancel" { Err("Denied by workspace policy".into()) } else { Ok(()) })),
            ..Default::default()
        };
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), services);
        let (sender, receiver) = mpsc::channel();
        let (inspect_reply, inspect_result) = mpsc::channel();
        let (cancel_reply, cancel_result) = mpsc::channel();
        sender.send(AgentCommand { command: "yorusai.inspect".into(), params: json!({}), reply: inspect_reply }).unwrap();
        sender.send(AgentCommand { command: "yorusai.cancel".into(), params: json!({}), reply: cancel_reply }).unwrap();
        app.codex_runtime.command_rx = Some(receiver);
        poll(&mut app, &egui::Context::default());
        let state = inspect_result.try_recv().unwrap().unwrap();
        assert_eq!(state["runtime"]["busy"], false);
        assert_eq!(cancel_result.try_recv().unwrap().unwrap_err(), "Denied by workspace policy");
        assert!(!app.automation_input);
    }

    #[test]
    fn all_variants_are_retained_when_placement_needs_recovery() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width":20,"height":20})).unwrap();
        app.run("select.rect", json!({"x":3,"y":4,"width":5,"height":6})).unwrap();
        capture(&mut app, true).unwrap();
        app.run("image.canvasSize", json!({"width":30,"height":30})).unwrap();
        receive_image(&mut app, png());
        receive_image(&mut app, png());
        receive_image(&mut app, png());
        assert!(app.codex_runtime.pending_image.is_some());
        assert_eq!(app.codex_runtime.additional_images.len(), 2);
        app.run("edit.undo", json!({})).unwrap();
        for _ in 0..3 {
            let bytes = app.codex_runtime.pending_image.take().unwrap();
            receive_image(&mut app, bytes);
            recover_next_image(&mut app);
        }
        assert!(app.codex_runtime.pending_image.is_none());
        assert!(app.codex_runtime.additional_images.is_empty());
        assert_eq!(app.session.active().unwrap().doc.layers.len(), 4);
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn agent_waits_for_background_edits_to_commit() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width":32,"height":24})).unwrap();
        app.background_jobs = true;
        let (sender, receiver) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        sender.send(AgentCommand { command: "filter.blur.gaussianBlur".into(), params: json!({"radius":2}), reply }).unwrap();
        app.codex_runtime.command_rx = Some(receiver);
        let ctx = egui::Context::default();
        poll(&mut app, &ctx);
        assert!(result.try_recv().is_err(), "a pending job must not be reported as completed");
        assert_eq!(app.codex_runtime.waiting_tools.len(), 1);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            crate::jobs_ui::tick(&mut app, &ctx);
            poll(&mut app, &ctx);
            if let Ok(result) = result.try_recv() {
                assert!(result.is_ok());
                break;
            }
            assert!(std::time::Instant::now() < deadline, "editor job did not finish");
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        assert!(app.codex_runtime.waiting_tools.is_empty());
    }

    #[test]
    fn agent_file_tools_fail_closed_without_workspace_authority() {
        let mut app = PhotocraftApp::new(photocraft_engine::Session::new(), Default::default());
        app.run("file.new", json!({"width":20,"height":20})).unwrap();
        let (sender, receiver) = mpsc::channel();
        let (reply, result) = mpsc::channel();
        sender.send(AgentCommand { command: "app.save".into(), params: json!({"path":"result.png"}), reply }).unwrap();
        app.codex_runtime.command_rx = Some(receiver);
        poll(&mut app, &egui::Context::default());
        assert!(result.try_recv().unwrap().is_err());
    }
}
