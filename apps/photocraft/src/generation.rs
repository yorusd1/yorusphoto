//! Native Codex bridge. Codex owns sign-in, credentials, model access and image generation.
//! The editor exchanges documented app-server JSON-RPC messages over private child-process
//! pipes; it never reads authentication files or sends requests to a private service endpoint.

use photocraft_ui_egui::{
    AgentCommand, CodexChatRequest, CodexEvent, CodexLoginEvent, CodexWorkspaceAction, CodexWorkspaceEvent, CodexWorkspaceRequest, ImageGenerationRequest,
};
use base64::Engine as _;
use serde_json::{Value, json};
use std::collections::VecDeque;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

mod workspace;

const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;
const MAX_RPC_BYTES: usize = 48 * 1024 * 1024;
const MAX_PROMPT_BYTES: usize = 128 * 1024;
const TURN_TIMEOUT: Duration = Duration::from_secs(30 * 60);
const SETUP_TIMEOUT: Duration = Duration::from_secs(90);
const TOOL_TIMEOUT: Duration = TURN_TIMEOUT;
const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

const IMAGE_INSTRUCTIONS: &str = "You are the image-generation worker inside yorusphoto. Use the built-in image_gen.imagegen tool exactly once for this request (when a GIF request fails, once more for the still flipbook sheet the turn describes). Use all reference images attached to this turn, with the num_last_images_to_include count specified in the turn. Omit referenced_image_paths: attached images are already available in this conversation, so the image tool must not reread local files. If no image is attached, omit both reference selectors and generate a new image from the prompt. The user's requested size and quality are preferences: the Codex image tool currently chooses both automatically. Do not claim they were enforced. When the turn asks for an animation, pass gif with the requested frame_count to image_gen.imagegen; its frames become the layers of a flipbook, so keep the subject, framing and canvas the same in every frame. When the turn asks for a transparent background, pass transparent_background: true. Do not execute commands, use other services, write code, or synthesize a replacement image using code. Return a concise completion message after the image tool completes. If image generation is unavailable, report that explicitly.";
const CHAT_INSTRUCTIONS: &str = "You are Codex embedded in the yorusphoto image editor. Help the user accomplish their request autonomously in the live document. First use yorusphoto_commands to inspect the available editor commands and current document. Drive editing through yorusphoto_execute, with exact documented command IDs and validated parameters. Every edit is an undoable editor command. For document file I/O, use the app.open and app.save methods listed under fileTools with paths relative to the authorized workspace; if that authority is unavailable, stop and explain it. Do not use filesystem or shell tools to manipulate the user's document. Stop editing when the user cancels the task. Use external apps or send messages only when the user explicitly asks. Use built-in image_gen.imagegen for requested image generation or image editing; returned images are delivered to the editor automatically, using the user's chosen selected-area or new-canvas destination. Request image dimensions and quality in the prompt as preferences; Codex currently chooses them automatically. Do not claim generation succeeded until the tool succeeds. For a sprite, icon, cutout or effect texture with no background, pass transparent_background: true to image_gen.imagegen. Ask before destructive actions beyond the user's request.";
/// Added to the chat instructions while the user has GIF output switched on.
const CHAT_ANIMATION_INSTRUCTIONS: &str = " GIF output is on: for an animation, flipbook or sprite animation the user asks for, pass gif with a frame_count of 2 to 16 to image_gen.imagegen. The editor places each frame as its own layer, so keep the subject, framing and canvas the same in every frame.";
/// Codex's switch for GIF output from its image tool (0.169 and later; older runtimes ignore it
/// with a warning and return still images).
const ANIMATION_FEATURE: &str = "features.image_generation_animation=true";
const ATTACHED_REFERENCE_INSTRUCTIONS: &str = "\nThe image attached to this turn is a reference image. For image editing, pass num_last_images_to_include: 1 to image_gen.imagegen and omit referenced_image_paths. Use the attached conversation image directly; do not look for or reread its local file.";

/// Runs one subscription-backed image turn on a worker thread.
pub fn start(request: ImageGenerationRequest, reply: mpsc::Sender<Result<Vec<u8>, String>>) {
    let failure_reply = reply.clone();
    if let Err(error) = std::thread::Builder::new().name("yorusphoto-image".into()).spawn(move || {
        let result = generate(request);
        let _ = reply.send(result);
    }) {
        let _ = failure_reply.send(Err(format!("Could not start the image worker: {error}")));
    }
}

/// Opens Codex's own browser authorization flow and waits for its completion.
pub fn sign_in(codex_path: String, cancel: Arc<AtomicBool>, reply: mpsc::Sender<CodexLoginEvent>) {
    let failure_reply = reply.clone();
    if let Err(error) = std::thread::Builder::new().name("yorusphoto-login".into()).spawn(move || {
        let result = login(&codex_path, &cancel, &reply);
        let _ = reply.send(match result {
            Ok(()) => CodexLoginEvent::Connected,
            Err(error) => CodexLoginEvent::Error(error),
        });
    }) {
        let _ = failure_reply.send(CodexLoginEvent::Error(format!("Could not start the sign-in worker: {error}")));
    }
}

/// Streams one persistent Codex chat turn and marshals editor actions to the UI thread.
pub fn chat(request: CodexChatRequest, events: mpsc::Sender<CodexEvent>, commands: mpsc::Sender<AgentCommand>) {
    let failure_events = events.clone();
    if let Err(error) = std::thread::Builder::new().name("yorusphoto-codex".into()).spawn(move || {
        if let Err(error) = chat_turn(request, &events, &commands) {
            let _ = events.send(CodexEvent::Error(error));
        }
        let _ = events.send(CodexEvent::Finished);
    }) {
        let _ = failure_events.send(CodexEvent::Error(format!("Could not start the Codex worker: {error}")));
        let _ = failure_events.send(CodexEvent::Finished);
    }
}

/// Runs account and saved-chat actions without blocking the editor or reading Codex's files.
pub fn workspace(request: CodexWorkspaceRequest, events: mpsc::Sender<CodexWorkspaceEvent>) {
    let failure_events = events.clone();
    if let Err(error) = std::thread::Builder::new().name("yorusphoto-codex-workspace".into()).spawn(move || {
        let result = (|| {
            let directory = TemporaryWorkspace::persistent()?;
            let mut server = AppServer::start(&request.codex_path, &directory.0, Some(request.cancel.clone()))?;
            workspace::perform(&mut server, &request, &directory.0, &events)
        })();
        let _ = events.send(match result {
            Ok(value) => CodexWorkspaceEvent::Result(value),
            Err(error) => CodexWorkspaceEvent::Error(error),
        });
    }) {
        let _ = failure_events.send(CodexWorkspaceEvent::Error(format!("Could not start the Codex account worker: {error}")));
    }
}

fn generate(request: ImageGenerationRequest) -> Result<Vec<u8>, String> {
    validate_prompt(&request.prompt)?;
    if request.quality.len() > 64 || request.size.len() > 64 {
        return Err("Image quality or size preference is too long.".into());
    }
    if request.animation.is_some_and(|frames| !(2..=16).contains(&frames)) {
        return Err("An animation has 2 to 16 frames.".into());
    }
    let workspace = TemporaryWorkspace::new()?;
    let references = workspace.image_references(request.reference.as_deref(), &request.references)?;
    let settings: &[&str] = if request.animation.is_some() { &[ANIMATION_FEATURE] } else { &[] };
    let mut server = AppServer::start_with(&request.codex_path, &workspace.0, Some(request.cancel.clone()), settings)?;
    server.require_chatgpt()?;
    let capability = server.rpc("modelProvider/capabilities/read", json!({}))?;
    if capability.get("imageGeneration").and_then(Value::as_bool) != Some(true) {
        return Err("This Codex installation does not support built-in image generation. Update Codex and sign in with an eligible ChatGPT plan.".into());
    }
    let selection = workspace::ModelSelection { model: request.model, effort: request.reasoning_effort, requires_image: true };
    let thread = server.start_thread(None, &workspace.0, IMAGE_INSTRUCTIONS, false, &selection)?;
    let reference_roles = if request.reference.is_some() {
        "Reference 1 is the captured selected canvas region: use it as the base composition. Any following images are additional user references; follow the user's prompt for how to use them."
    } else if !references.is_empty() {
        "The attached images are user-provided references for a new image; follow the user's prompt for their content, style or composition."
    } else {
        ""
    };
    let mut options = String::new();
    if let Some(frames) = request.animation {
        let (columns, rows) = photocraft_ui_egui::flipbook_grid(frames);
        let spare = if columns * rows > frames { ", leaving the spare cells empty" } else { "" };
        options.push_str(&format!(
            " Make it a looping animation: pass gif: {{\"frame_count\": {frames}}} to image_gen.imagegen. Each frame becomes a layer of a flipbook, so keep the subject, framing and canvas the same in every frame and change only the motion. If the GIF request fails, call image_gen.imagegen once more without gif and draw the {frames} frames as one image instead: a {columns} x {rows} grid of equal cells in reading order (left to right, then top to bottom), with no gaps, borders, numbers or labels{spare}."
        ));
    }
    if request.transparent {
        options.push_str(" Pass transparent_background: true: the result is a cutout with no background.");
    }
    let prompt = format!(
        "Generate one image. Preferred rendering quality: {}. Preferred output dimensions: {}. These are image-tool prompt preferences.{options} {}\nUser request:\n{}",
        request.quality, request.size, reference_roles, request.prompt
    );
    let turn_id = server.start_turn(&thread.id, &prompt, &references, &thread)?;
    let images = server.run_turn(&thread.id, &turn_id, &request.cancel, None, None, None)?;
    if request.cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    images
        .into_iter()
        .next()
        .ok_or_else(|| "Codex finished without generating an image. Check that your signed-in plan supports image generation, then try again.".into())
}

fn chat_turn(request: CodexChatRequest, events: &mpsc::Sender<CodexEvent>, commands: &mpsc::Sender<AgentCommand>) -> Result<(), String> {
    validate_prompt(&request.prompt)?;
    let workspace = TemporaryWorkspace::persistent()?;
    let reference = workspace.reference(request.reference.as_deref())?;
    let settings: &[&str] = if request.animation { &[ANIMATION_FEATURE] } else { &[] };
    let mut server = AppServer::start_with(&request.codex_path, &workspace.0, Some(request.cancel.clone()), settings)?;
    server.require_chatgpt()?;
    let selection = workspace::ModelSelection { model: request.model, effort: request.reasoning_effort, requires_image: reference.is_some() };
    if let Some(thread_id) = &request.thread_id {
        workspace::verify_thread(&mut server, thread_id, &workspace.0)?;
    }
    let instructions = if request.animation { format!("{CHAT_INSTRUCTIONS}{CHAT_ANIMATION_INSTRUCTIONS}") } else { CHAT_INSTRUCTIONS.to_owned() };
    let thread = server.start_thread(request.thread_id.as_deref(), &workspace.0, &instructions, true, &selection)?;
    let _ = events.send(CodexEvent::Started(thread.id.clone()));
    let _ = events.send(CodexEvent::Telemetry { kind: "model".into(), data: json!({"model":thread.model,"reasoningEffort":thread.effort}) });
    let references: Vec<_> = reference.into_iter().collect();
    let turn_id = server.start_turn(&thread.id, &request.prompt, &references, &thread)?;
    let images = server.run_turn(&thread.id, &turn_id, &request.cancel, Some(events), Some(commands), request.steer.as_ref());
    // Messages typed as the turn ended never reached Codex: give them back.
    while let Some(text) = request.steer.as_ref().and_then(|steer| steer.try_recv().ok()) {
        let _ = events.send(CodexEvent::Unsent(text));
    }
    for bytes in images? {
        if request.cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        let _ = events.send(CodexEvent::Image(bytes));
    }
    Ok(())
}

fn login(codex_path: &str, cancel: &Arc<AtomicBool>, events: &mpsc::Sender<CodexLoginEvent>) -> Result<(), String> {
    let workspace = TemporaryWorkspace::new()?;
    let mut server = AppServer::start(codex_path, &workspace.0, Some(cancel.clone()))?;
    login_flow(&mut server, cancel, events, |url| open::that(url).map_err(|_| ()))
}

/// Keep authorization URLs and provider diagnostics in the transient browser event only.
/// In particular, do not include a URL, authorization code or server error in an error string
/// that can subsequently be saved as an editor notification.
fn approved_login_url(address: &str) -> bool {
    if address.len() > 16 * 1024 || address.chars().any(char::is_control) {
        return false;
    }
    // Url normalizes an empty username away, so also reject a syntactic `@` in
    // the input authority (including https://@chatgpt.com).
    if address.split_once("://").is_some_and(|(_, rest)| rest.split(['/', '?', '#']).next().is_some_and(|authority| authority.contains('@'))) {
        return false;
    }
    reqwest::Url::parse(address).is_ok_and(|url| {
        url.scheme() == "https"
            && matches!(url.host_str(), Some("auth.openai.com" | "chatgpt.com"))
            && url.username().is_empty()
            && url.password().is_none()
            && url.port_or_known_default() == Some(443)
    })
}

fn is_chatgpt_account(result: &Value) -> bool {
    result.pointer("/account/type").and_then(Value::as_str) == Some("chatgpt")
}

trait LoginConnection {
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String>;
    fn receive_login(&mut self, deadline: Instant) -> Result<Value, String>;
    fn reject_request(&mut self, message: &Value) -> Result<(), String>;
    fn cancel_login(&mut self, login_id: &str);
}

fn login_flow(
    server: &mut impl LoginConnection,
    cancel: &AtomicBool,
    events: &mpsc::Sender<CodexLoginEvent>,
    open_browser: impl FnOnce(&str) -> Result<(), ()>,
) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("ChatGPT sign-in cancelled.".into());
    }
    let account =
        server.rpc("account/read", json!({"refreshToken":false})).map_err(|_| "Could not check the Codex sign-in status. Try again or update Codex.")?;
    if cancel.load(Ordering::Relaxed) {
        return Err("ChatGPT sign-in cancelled.".into());
    }
    if is_chatgpt_account(&account) {
        return Ok(());
    }
    let result =
        server.rpc("account/login/start", json!({"type":"chatgpt"})).map_err(|_| "Codex could not start ChatGPT sign-in. Try again or update Codex.")?;
    let login_id = result
        .get("loginId")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 4096)
        .ok_or("Codex did not return a valid sign-in request. Update Codex and try again.")?;
    if cancel.load(Ordering::Relaxed) {
        server.cancel_login(login_id);
        return Err("ChatGPT sign-in cancelled.".into());
    }
    let auth_url = result.get("authUrl").and_then(Value::as_str).unwrap_or_default();
    if !approved_login_url(auth_url) {
        server.cancel_login(login_id);
        return Err("Codex did not return an approved OpenAI authorization address. Update Codex and try again.".into());
    }
    if events.send(CodexLoginEvent::OpeningBrowser { url: auth_url.into() }).is_err() {
        server.cancel_login(login_id);
        return Err("ChatGPT sign-in cancelled.".into());
    }
    if open_browser(auth_url).is_err() {
        // Launch errors can contain the full command/URL. Supply a fixed safe message and
        // continue waiting: the user can still finish through the visible fallback link.
        let _ = events.send(CodexLoginEvent::BrowserError("The browser could not be opened automatically. Use the Open sign-in page link to continue.".into()));
    }
    let deadline = Instant::now() + LOGIN_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            server.cancel_login(login_id);
            return Err("ChatGPT sign-in cancelled.".into());
        }
        let message = match server.receive_login(deadline) {
            Ok(message) => message,
            Err(_) => {
                server.cancel_login(login_id);
                return Err(if cancel.load(Ordering::Relaxed) {
                    "ChatGPT sign-in cancelled."
                } else if Instant::now() >= deadline {
                    "ChatGPT sign-in timed out after 10 minutes. Start sign-in again."
                } else {
                    "The ChatGPT sign-in connection closed. Start sign-in again."
                }
                .into());
            }
        };
        if cancel.load(Ordering::Relaxed) {
            server.cancel_login(login_id);
            return Err("ChatGPT sign-in cancelled.".into());
        }
        if message.get("method").and_then(Value::as_str) == Some("account/login/completed") {
            let params = message.get("params").unwrap_or(&Value::Null);
            if params.get("loginId").and_then(Value::as_str) != Some(login_id) {
                continue;
            }
            if params.get("success").and_then(Value::as_bool) == Some(true) {
                let account =
                    server.rpc("account/read", json!({"refreshToken":false})).map_err(|_| "Could not confirm the ChatGPT account after sign-in. Try again.")?;
                if cancel.load(Ordering::Relaxed) {
                    server.cancel_login(login_id);
                    return Err("ChatGPT sign-in cancelled.".into());
                }
                return if is_chatgpt_account(&account) { Ok(()) } else { Err("Codex did not report a ChatGPT account after sign-in. Try again.".into()) };
            }
            return Err("ChatGPT sign-in was cancelled or failed. Start sign-in again.".into());
        }
        if server.reject_request(&message).is_err() {
            server.cancel_login(login_id);
            return Err("The ChatGPT sign-in connection closed. Start sign-in again.".into());
        }
    }
}

fn validate_prompt(prompt: &str) -> Result<(), String> {
    if prompt.trim().is_empty() {
        return Err("Enter a prompt first.".into());
    }
    if prompt.len() > MAX_PROMPT_BYTES {
        return Err("The prompt exceeds the 128 KiB limit.".into());
    }
    Ok(())
}

struct TemporaryWorkspace(PathBuf, bool);

impl TemporaryWorkspace {
    fn image_references(&self, selection: Option<&[u8]>, uploads: &[Arc<[u8]>]) -> Result<Vec<PathBuf>, String> {
        if uploads.len() > 4 {
            return Err("Use at most four uploaded image references plus the selected canvas region.".into());
        }
        let mut total = 0usize;
        for bytes in selection.into_iter().chain(uploads.iter().map(AsRef::as_ref)) {
            validate_png(bytes)?;
            total = total.checked_add(bytes.len()).ok_or("Image reference size overflow.")?;
            if total > 64 << 20 {
                return Err("The selection and uploaded references together exceed 64 MiB.".into());
            }
        }
        let mut paths = Vec::new();
        if let Some(path) = self.reference(selection)? {
            paths.push(path);
        }
        for bytes in uploads {
            let path = self.0.join(format!("reference-{}.png", paths.len() + 1));
            fs::write(&path, bytes).map_err(|error| format!("Could not prepare an image reference: {error}"))?;
            paths.push(path);
        }
        Ok(paths)
    }

    fn new() -> Result<Self, String> {
        Self::allocate(&std::env::temp_dir(), true)
    }

    fn persistent() -> Result<Self, String> {
        let root = if let Some(config) = std::env::var_os("PHOTOCRAFT_CONFIG_DIR") {
            PathBuf::from(config)
        } else {
            std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .or_else(|| std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from))
                .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".cache")))
                .unwrap_or_else(std::env::temp_dir)
                .join("yorusphoto")
        };
        let directory = root.join("codex-workspace");
        fs::create_dir_all(&directory).map_err(|error| format!("Could not create the Codex chat workspace: {error}"))?;
        let directory = fs::canonicalize(directory).map_err(|error| format!("Could not resolve the Codex chat workspace: {error}"))?;
        Ok(Self(directory, false))
    }

    fn allocate(root: &Path, cleanup: bool) -> Result<Self, String> {
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|error| error.to_string())?.as_nanos();
        for _ in 0..8 {
            let path = root.join(format!("yorusphoto-codex-{}-{now}-{}", std::process::id(), TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path, cleanup)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(format!("Could not create the image request directory: {error}")),
            }
        }
        Err("Could not allocate a unique image request directory.".into())
    }

    fn reference(&self, bytes: Option<&[u8]>) -> Result<Option<PathBuf>, String> {
        let Some(bytes) = bytes else { return Ok(None) };
        validate_png(bytes)?;
        // A stable chat cwd permits listing app-owned chats. Each turn still receives its
        // own reference path so resuming or opening another chat never replaces an image.
        let path = if self.1 {
            self.0.join("reference.png")
        } else {
            let root = self.0.join("codex-references");
            fs::create_dir_all(&root).map_err(|error| format!("Could not create the reference cache: {error}"))?;
            Self::allocate(&root, false)?.0.as_path().join("reference.png")
        };
        fs::write(&path, bytes).map_err(|error| format!("Could not save the selected-area reference: {error}"))?;
        Ok(Some(path))
    }
}

impl Drop for TemporaryWorkspace {
    fn drop(&mut self) {
        // Delete only the exact directory allocated by this instance; never a model-provided path.
        if self.1 {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

struct AppServer {
    child: Child,
    stdin: ChildStdin,
    messages: mpsc::Receiver<Result<Value, String>>,
    next_id: u64,
    image_root: PathBuf,
    workspace: PathBuf,
    pending: VecDeque<Value>,
    pending_bytes: usize,
    cancel: Option<Arc<AtomicBool>>,
    spawned_at: Instant,
}

struct PendingEditorCall {
    id: Value,
    result: mpsc::Receiver<Result<Value, String>>,
    started: Instant,
}

impl AppServer {
    fn start(codex_path: &str, workspace: &Path, cancel: Option<Arc<AtomicBool>>) -> Result<Self, String> {
        Self::start_with(codex_path, workspace, cancel, &[])
    }

    /// Start Codex with `settings`, extra `-c` config overrides for this request.
    fn start_with(codex_path: &str, workspace: &Path, cancel: Option<Arc<AtomicBool>>, settings: &[&str]) -> Result<Self, String> {
        let binary = resolve_codex(codex_path)?;
        let mut command = Command::new(&binary);
        command.args(["app-server", "--listen", "stdio://", "--enable", "image_generation"]);
        for setting in ["model_provider=\"openai\"", "features.shell_tool=false", "features.hooks=false"].iter().chain(settings) {
            command.args(["-c", *setting]);
        }
        command.current_dir(workspace).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        // The subscription bridge must never silently switch to API billing from inherited env.
        command.env_remove("OPENAI_API_KEY").env_remove("CODEX_API_KEY");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        let mut child = command.spawn().map_err(|error| format!("Could not start Codex ({}): {error}", binary.display()))?;
        let spawned_at = Instant::now();
        let Some(stdin) = child.stdin.take() else {
            let _ = child.kill();
            return Err("Codex did not provide its input pipe.".into());
        };
        let Some(stdout) = child.stdout.take() else {
            let _ = child.kill();
            return Err("Codex did not provide its output pipe.".into());
        };
        if let Some(stderr) = child.stderr.take()
            && let Err(error) = std::thread::Builder::new().name("yorusphoto-codex-stderr".into()).spawn(move || {
                let _ = std::io::copy(&mut BufReader::new(stderr), &mut std::io::sink());
            })
        {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Could not start the Codex diagnostic reader: {error}"));
        }
        let (sender, messages) = mpsc::sync_channel(4);
        if let Err(error) = std::thread::Builder::new().name("yorusphoto-codex-reader".into()).spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let parsed = read_message(&mut reader);
                let terminal = parsed.is_err();
                if sender.send(parsed).is_err() || terminal {
                    break;
                }
            }
        }) {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("Could not start the Codex response reader: {error}"));
        }
        let mut server = Self {
            child,
            stdin,
            messages,
            next_id: 1,
            image_root: PathBuf::new(),
            workspace: workspace.to_path_buf(),
            pending: VecDeque::new(),
            pending_bytes: 0,
            cancel,
            spawned_at,
        };
        let initialized = server.rpc(
            "initialize",
            json!({
                "clientInfo":{"name":"yorusphoto","title":"yorusphoto","version":env!("CARGO_PKG_VERSION")},
                "capabilities":{"experimentalApi":true}
            }),
        )?;
        let codex_home = initialized
            .get("codexHome")
            .and_then(Value::as_str)
            .ok_or("This Codex app-server is too old to report its generated-image directory. Update Codex.")?;
        server.image_root = PathBuf::from(codex_home).join("generated_images");
        server.send(&json!({"method":"initialized","params":{}}))?;
        Ok(server)
    }

    fn send(&mut self, message: &Value) -> Result<(), String> {
        serde_json::to_writer(&mut self.stdin, message).map_err(|error| format!("Could not send a Codex request: {error}"))?;
        self.stdin.write_all(b"\n").and_then(|()| self.stdin.flush()).map_err(|error| format!("The Codex connection closed: {error}"))
    }

    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        self.send(&json!({"id":id,"method":method,"params":params}))?;
        let deadline = Instant::now() + SETUP_TIMEOUT;
        loop {
            // A turn/start reply supplies the ID needed to interrupt it. Briefly allow that
            // reply through after Stop instead of killing the process before it can cancel.
            let message = self.receive(deadline, method != "turn/start")?;
            if message.get("id").and_then(Value::as_u64) == Some(id) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    return Err(rpc_error(error));
                }
                return message.get("result").cloned().ok_or_else(|| "Codex returned an incomplete protocol response.".into());
            }
            if message.get("method").and_then(Value::as_str).is_some_and(workspace::queued_notification) {
                let length = message.to_string().len();
                if self.pending.len() >= 128 || self.pending_bytes.saturating_add(length) > MAX_RPC_BYTES {
                    return Err("Codex sent too many events before its protocol response.".into());
                }
                self.pending_bytes = self.pending_bytes.saturating_add(length);
                self.pending.push_back(message);
            } else {
                self.reject_other_request(&message)?;
            }
        }
    }

    fn receive(&self, deadline: Instant, observe_cancel: bool) -> Result<Value, String> {
        let mut cancellation_started = None;
        loop {
            if self.cancel.as_ref().is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                if observe_cancel {
                    return Err("Cancelled.".into());
                }
                let since = cancellation_started.get_or_insert_with(Instant::now);
                if since.elapsed() >= Duration::from_secs(5) {
                    return Err("Cancelled while Codex was starting the request.".into());
                }
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("Codex timed out. Try again or check your connection.".into());
            }
            match self.messages.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(message) => return message,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Err(mpsc::RecvTimeoutError::Disconnected) => return Err("Codex stopped responding. Check your installation and sign-in.".into()),
            }
        }
    }

    fn require_chatgpt(&mut self) -> Result<(), String> {
        let result = self.rpc("account/read", json!({"refreshToken":false}))?;
        let account = result.get("account").unwrap_or(&Value::Null);
        if account.get("type").and_then(Value::as_str) != Some("chatgpt") {
            return Err("Sign in with ChatGPT using the Connect button. This feature uses your Codex subscription account.".into());
        }
        if account.get("planType").and_then(Value::as_str) == Some("free") {
            return Err("Built-in Codex image generation requires an eligible paid ChatGPT plan.".into());
        }
        Ok(())
    }

    fn start_thread(
        &mut self,
        resume: Option<&str>,
        workspace: &Path,
        instructions: &str,
        editor_tools: bool,
        selection: &workspace::ModelSelection,
    ) -> Result<workspace::StartedThread, String> {
        let mut params = json!({"cwd":workspace,"approvalPolicy":"never","sandbox":"read-only","modelProvider":"openai","developerInstructions":instructions});
        let catalog = workspace::model_catalog(self, false)?;
        let (model, effort) = workspace::select_model(&catalog, selection)?;
        params["model"] = json!(model);
        if let Some(effort) = &effort {
            params["config"] = json!({"model_reasoning_effort":effort});
        }
        let method = if let Some(thread_id) = resume {
            params["threadId"] = json!(thread_id);
            params["excludeTurns"] = json!(true);
            "thread/resume"
        } else {
            params["ephemeral"] = json!(!editor_tools);
            if editor_tools {
                params["dynamicTools"] = editor_tool_specs();
            }
            "thread/start"
        };
        let result = self.rpc(method, params)?;
        let id = result.pointer("/thread/id").and_then(Value::as_str).ok_or("Codex did not return a thread ID.")?;
        workspace::validate_id(id)?;
        Ok(workspace::StartedThread { id: id.into(), model, effort })
    }

    fn start_turn(&mut self, thread_id: &str, prompt: &str, references: &[PathBuf], thread: &workspace::StartedThread) -> Result<String, String> {
        if references.len() > 5 {
            return Err("The image tool supports at most five attached references per turn.".into());
        }
        let input = turn_input(prompt, references);
        let mut params = json!({"threadId":thread_id,"input":input,"model":thread.model});
        if let Some(effort) = &thread.effort {
            params["effort"] = json!(effort);
        }
        let result = self.rpc("turn/start", params)?;
        result.pointer("/turn/id").and_then(Value::as_str).map(str::to_owned).ok_or_else(|| "Codex did not return a turn ID.".into())
    }

    fn run_turn(
        &mut self,
        thread_id: &str,
        turn_id: &str,
        cancel: &AtomicBool,
        events: Option<&mpsc::Sender<CodexEvent>>,
        commands: Option<&mpsc::Sender<AgentCommand>>,
        steer: Option<&mpsc::Receiver<String>>,
    ) -> Result<Vec<Vec<u8>>, String> {
        let deadline = Instant::now() + TURN_TIMEOUT;
        let mut images = Vec::new();
        let mut assistant_text = String::new();
        let mut editor_calls: VecDeque<PendingEditorCall> = VecDeque::new();
        let mut transcript = Transcript::default();
        let mut steering: Vec<(u64, String)> = Vec::new();
        // A failed image call: Codex may explain or retry it, so the turn goes on.
        let mut image_error: Option<String> = None;
        loop {
            if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
                let cancelled = cancel.load(Ordering::Relaxed);
                cancel.store(true, Ordering::Relaxed);
                self.interrupt_turn(thread_id, turn_id);
                return Err(if cancelled { "Cancelled." } else { "Codex reached the 30-minute request timeout." }.into());
            }
            // Several child agents can have editor calls outstanding. Keep reading protocol
            // events while the UI waits for background filters instead of blocking on one job.
            for _ in 0..editor_calls.len() {
                let Some(call) = editor_calls.pop_front() else { break };
                match call.result.try_recv() {
                    Ok(result) => self.reply_editor_tool(&call.id, result)?,
                    Err(mpsc::TryRecvError::Disconnected) => self.reply_editor_tool(&call.id, Err("The image editor closed its command connection.".into()))?,
                    Err(mpsc::TryRecvError::Empty) if call.started.elapsed() >= TOOL_TIMEOUT => {
                        cancel.store(true, Ordering::Relaxed);
                        self.reply_editor_tool(&call.id, Err("The image editor did not complete its command within 30 minutes.".into()))?;
                    }
                    Err(mpsc::TryRecvError::Empty) => editor_calls.push_back(call),
                }
            }
            if cancel.load(Ordering::Relaxed) {
                continue;
            }
            // A message sent while the task runs joins this turn, as typing in Codex does.
            self.send_steering(steer, thread_id, turn_id, &mut steering)?;
            let message = if let Some(message) = self.pending.pop_front() {
                self.pending_bytes = self.pending_bytes.saturating_sub(message.to_string().len());
                message
            } else {
                match self.messages.recv_timeout(Duration::from_millis(100)) {
                    Ok(message) => message?,
                    Err(mpsc::RecvTimeoutError::Timeout) => continue,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return Err("The Codex process exited before completing the request.".into()),
                }
            };
            if message.get("method").is_none() {
                steering_reply(&message, &mut steering, events);
                continue;
            }
            let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
            let params = message.get("params").unwrap_or(&Value::Null);
            if let Some(events) = events
                && let Some((kind, data)) = workspace::telemetry(method, params, Some(thread_id))
            {
                let _ = events.send(CodexEvent::Telemetry { kind, data });
            }
            // Item notifications also carry normal message/image events below.
            match method {
                "item/tool/call" if message.get("id").is_some() => {
                    if let Some(events) = events {
                        let tool = params.get("tool").and_then(Value::as_str).unwrap_or("editor tool");
                        let _ = events.send(CodexEvent::Status(format!("Using {tool}…")));
                    }
                    let id = message.get("id").cloned().unwrap_or(Value::Null);
                    if editor_calls.len() >= 64 {
                        self.reply_editor_tool(&id, Err("There are already 64 pending editor commands; wait for them to finish.".into()))?;
                    } else {
                        match begin_editor_tool(params, commands, cancel) {
                            Ok(result) => editor_calls.push_back(PendingEditorCall { id, result, started: Instant::now() }),
                            Err(error) => self.reply_editor_tool(&id, Err(error))?,
                        }
                    }
                }
                "item/agentMessage/delta" => {
                    if let Some(delta) = params.get("delta").and_then(Value::as_str) {
                        // Each agent message is its own paragraph, as Codex shows them.
                        let text = transcript.delta(params.get("itemId").and_then(Value::as_str), delta);
                        if assistant_text.len().saturating_add(text.len()) <= 8192 {
                            assistant_text.push_str(&text);
                        }
                        if let Some(events) = events {
                            let _ = events.send(CodexEvent::Text(text));
                        }
                    }
                }
                "item/reasoning/summaryTextDelta" | "turn/plan/updated" | "warning" | "item/started" => {
                    if let Some(events) = events
                        && let Some(status) = progress_status(method, params, &mut transcript)
                    {
                        let _ = events.send(CodexEvent::Status(status));
                    }
                }
                "item/completed" => {
                    if let Some(item) = params.get("item") {
                        if item.get("type").and_then(Value::as_str) == Some("agentMessage")
                            && let Some(text) = item.get("text").and_then(Value::as_str)
                            && let Some(text) = transcript.completed(item.get("id").and_then(Value::as_str), text)
                        {
                            if assistant_text.len().saturating_add(text.len()) <= 8192 {
                                assistant_text.push_str(&text);
                            }
                            if let Some(events) = events {
                                let _ = events.send(CodexEvent::Text(text));
                            }
                        }
                        if item.get("type").and_then(Value::as_str) == Some("imageGeneration") {
                            if images.len() >= 5 {
                                return Err("Codex returned more than five images in one turn.".into());
                            }
                            match read_generated_image(item, &self.image_root, &self.workspace) {
                                Ok(bytes) => images.push(bytes),
                                Err(error) => {
                                    // In a chat the note stays in the transcript, as its own paragraph.
                                    if let Some(events) = events {
                                        let _ = events.send(CodexEvent::Text(transcript.delta(item.get("id").and_then(Value::as_str), &error)));
                                    }
                                    image_error = Some(error);
                                }
                            }
                        }
                    }
                }
                "turn/completed" => {
                    let turn = params.get("turn").unwrap_or(&Value::Null);
                    if turn.get("id").and_then(Value::as_str) != Some(turn_id) {
                        continue;
                    }
                    return match turn.get("status").and_then(Value::as_str) {
                        Some("completed") if images.is_empty() && events.is_none() => {
                            let reason = image_error.as_deref().unwrap_or("Codex completed without generating an image.");
                            let said = assistant_text.split_whitespace().collect::<Vec<_>>().join(" ");
                            Err(format!("{reason} {said}").trim_end().to_owned())
                        }
                        Some("completed") => Ok(images),
                        Some("interrupted") => Err("Cancelled.".into()),
                        _ => Err(turn.pointer("/error/message").and_then(Value::as_str).unwrap_or("Codex could not complete the request.").into()),
                    };
                }
                "error" => {
                    if params.get("willRetry").and_then(Value::as_bool) != Some(true) {
                        return Err(params.pointer("/error/message").and_then(Value::as_str).unwrap_or("Codex reported an error.").into());
                    }
                    if let Some(events) = events {
                        let _ = events.send(CodexEvent::Status("Reconnecting to Codex…".into()));
                    }
                }
                _ => self.reject_other_request(&message)?,
            }
        }
    }

    /// Send the messages typed while the turn runs as Codex's `turn/steer` (at most 16 unanswered).
    fn send_steering(&mut self, steer: Option<&mpsc::Receiver<String>>, thread_id: &str, turn_id: &str, steering: &mut Vec<(u64, String)>) -> Result<(), String> {
        while let Some(text) = steer.and_then(|steer| steer.try_recv().ok()) {
            if text.trim().is_empty() || text.len() > MAX_PROMPT_BYTES || steering.len() >= 16 {
                continue;
            }
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            self.send(&json!({"id":id,"method":"turn/steer","params":{"threadId":thread_id,"expectedTurnId":turn_id,"input":[{"type":"text","text":text}]}}))?;
            steering.push((id, text));
        }
        Ok(())
    }

    fn reject_other_request(&mut self, message: &Value) -> Result<(), String> {
        if message.get("method").is_some()
            && let Some(id) = message.get("id")
        {
            self.send(&json!({"id":id,"error":{"code":-32601,"message":"This editor bridge supports only its live editor tools; interactive approval requests are disabled."}}))?;
        }
        Ok(())
    }

    fn reply_editor_tool(&mut self, id: &Value, result: Result<Value, String>) -> Result<(), String> {
        let success = result.is_ok();
        let text = match result {
            Ok(value) => value.to_string(),
            Err(error) => error,
        };
        self.send(&json!({"id":id,"result":{"contentItems":[{"type":"inputText","text":text}],"success":success}}))
    }

    fn interrupt_turn(&mut self, thread_id: &str, turn_id: &str) {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        if self.send(&json!({"id":id,"method":"turn/interrupt","params":{"threadId":thread_id,"turnId":turn_id}})).is_err() {
            return;
        }
        // Keep the runtime alive long enough to process cancellation. Closing pipes immediately
        // after writing the request can leave it unread and an upstream generation running.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return;
            }
            match self.messages.recv_timeout(remaining.min(Duration::from_millis(100))) {
                Ok(Ok(message))
                    if message.get("method").and_then(Value::as_str) == Some("turn/completed")
                        && message.pointer("/params/turn/id").and_then(Value::as_str) == Some(turn_id) =>
                {
                    return;
                }
                Ok(Ok(_)) | Err(mpsc::RecvTimeoutError::Timeout) => continue,
                Ok(Err(_)) | Err(mpsc::RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

impl LoginConnection for AppServer {
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        AppServer::rpc(self, method, params)
    }

    fn receive_login(&mut self, deadline: Instant) -> Result<Value, String> {
        if let Some(message) = self.pending.pop_front() {
            self.pending_bytes = self.pending_bytes.saturating_sub(message.to_string().len());
            Ok(message)
        } else {
            self.receive(deadline, true)
        }
    }

    fn reject_request(&mut self, message: &Value) -> Result<(), String> {
        self.reject_other_request(message)
    }

    fn cancel_login(&mut self, login_id: &str) {
        let id = self.next_id;
        self.next_id = self.next_id.saturating_add(1);
        if self.send(&json!({"id":id,"method":"account/login/cancel","params":{"loginId":login_id}})).is_err() {
            return;
        }
        // Wait briefly for the cancel acknowledgement before Drop closes Codex's pipes.
        // Cancellation must ignore the already-set user cancellation flag during this wait.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match self.receive(deadline, false) {
                Ok(message) if message.get("id").and_then(Value::as_u64) == Some(id) && message.get("method").is_none() => return,
                Ok(_) => continue,
                Err(_) => return,
            }
        }
    }
}

fn turn_input(prompt: &str, references: &[PathBuf]) -> Vec<Value> {
    let text = match references.len() {
        0 => prompt.into(),
        1 => format!("{prompt}{ATTACHED_REFERENCE_INSTRUCTIONS}"),
        count => format!(
            "{prompt}\nAll {count} images attached to this turn are references. Use num_last_images_to_include: {count} with image_gen.imagegen and omit referenced_image_paths. Use all attached conversation images directly, in their given order; do not look for or reread their local files."
        ),
    };
    let mut input = vec![json!({"type":"text","text":text})];
    for path in references {
        // Codex loads this structured attachment into the conversation. The model-facing
        // text deliberately contains no filename, avoiding a second sandbox filesystem read.
        input.push(json!({"type":"localImage","path":path,"detail":"original"}));
    }
    input
}

impl Drop for AppServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn rpc_error(error: &Value) -> String {
    error.get("message").and_then(Value::as_str).unwrap_or("The installed Codex version rejected this protocol request. Update Codex.").into()
}

fn read_message(reader: &mut impl BufRead) -> Result<Value, String> {
    let mut line = Vec::new();
    let count = reader.take((MAX_RPC_BYTES + 1) as u64).read_until(b'\n', &mut line).map_err(|error| format!("Could not read the Codex response: {error}"))?;
    if count == 0 {
        return Err("The Codex process closed its output pipe. Check your Codex installation.".into());
    }
    if line.len() > MAX_RPC_BYTES {
        return Err("The Codex response exceeded the 48 MiB protocol limit.".into());
    }
    serde_json::from_slice(&line).map_err(|error| format!("Codex returned an invalid protocol message: {error}"))
}

fn read_generated_image(item: &Value, image_root: &Path, workspace: &Path) -> Result<Vec<u8>, String> {
    if item.get("status").and_then(Value::as_str) != Some("completed") {
        return Err(image_failure(item, SystemTime::now()));
    }
    let Some(path) = item.get("savedPath").and_then(Value::as_str) else {
        // Without a saved file the image may still come inline.
        let inline = item.get("result").and_then(Value::as_str).map(str::trim).filter(|result| !result.is_empty());
        let Some(result) = inline else {
            return Err("Codex did not save its generated image. Update Codex and retry.".into());
        };
        if result.len() > MAX_IMAGE_BYTES.div_ceil(3) * 4 {
            return Err("The generated image exceeds the 32 MiB file limit.".into());
        }
        let bytes = base64::engine::general_purpose::STANDARD.decode(result).map_err(|_| "Codex returned an unreadable generated image.".to_owned())?;
        validate_generated(&bytes)?;
        return Ok(bytes);
    };
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err("Codex returned a non-absolute generated-image path.".into());
    }
    let path = fs::canonicalize(path).map_err(|error| format!("Could not find the generated image: {error}"))?;
    let allowed = [image_root, workspace].iter().filter_map(|root| fs::canonicalize(root).ok()).any(|root| path.starts_with(root));
    if !allowed {
        return Err("Codex returned an image outside its generated-image directory.".into());
    }
    let file = File::open(&path).map_err(|error| format!("Could not open the generated image: {error}"))?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_IMAGE_BYTES as u64 {
        return Err("The generated image exceeds the 32 MiB file limit.".into());
    }
    let mut bytes = Vec::new();
    file.take((MAX_IMAGE_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|error| format!("Could not read the generated image: {error}"))?;
    validate_generated(&bytes)?;
    Ok(bytes)
}

/// A reply to a steering message: only a refusal needs saying, and its text goes back to the
/// composer.
fn steering_reply(message: &Value, steering: &mut Vec<(u64, String)>, events: Option<&mpsc::Sender<CodexEvent>>) {
    let Some(id) = message.get("id").and_then(Value::as_u64) else { return };
    let Some(index) = steering.iter().position(|(pending, _)| *pending == id) else { return };
    let (_, text) = steering.swap_remove(index);
    if message.get("error").is_some()
        && let Some(events) = events
    {
        let _ = events.send(CodexEvent::Unsent(text));
    }
}

/// Why an image generation failed, in words: a ChatGPT plan's image limit says when it resets.
fn image_failure(item: &Value, now: SystemTime) -> String {
    if item.pointer("/failure/type").and_then(Value::as_str) != Some("usageLimitExceeded") {
        return "Codex image generation failed.".into();
    }
    let now = now.duration_since(UNIX_EPOCH).map_or(0, |time| time.as_secs()) as i64;
    match item.pointer("/failure/resetsAt").and_then(Value::as_i64).map(|at| at - now) {
        Some(seconds) if seconds > 0 => {
            let minutes = (seconds + 59) / 60;
            let wait = match (minutes / 60, minutes % 60) {
                (0, m) => format!("{m} min"),
                (h, 0) => format!("{h} h"),
                (h, m) => format!("{h} h {m} min"),
            };
            format!("You reached your ChatGPT plan's image generation limit. It resets in {wait}.")
        }
        _ => "You reached your ChatGPT plan's image generation limit. Try again later.".into(),
    }
}

/// A generated image: a PNG, or a GIF for an animation, under 32 MiB.
fn validate_generated(bytes: &[u8]) -> Result<(), String> {
    let png = bytes.get(..8) == Some(b"\x89PNG\r\n\x1a\n");
    let gif = matches!(bytes.get(..6), Some(b"GIF87a" | b"GIF89a"));
    if bytes.len() > MAX_IMAGE_BYTES || !(png || gif) {
        return Err("The generated image must be a PNG or GIF smaller than 32 MiB.".into());
    }
    Ok(())
}

/// The status line for a progress notification: the heading of what Codex is thinking about, its
/// plan, a warning, or the kind of work it started.
fn progress_status(method: &str, params: &Value, transcript: &mut Transcript) -> Option<String> {
    match method {
        "item/reasoning/summaryTextDelta" => {
            let item = params.get("itemId").and_then(Value::as_str)?;
            let delta = params.get("delta").and_then(Value::as_str)?;
            transcript.thinking(item, params.get("summaryIndex").and_then(Value::as_i64).unwrap_or(0), delta)
        }
        "turn/plan/updated" => plan_status(params),
        "warning" => params.get("message").and_then(Value::as_str).map(|warning| warning.chars().take(240).collect()),
        "item/started" => match params.pointer("/item/type").and_then(Value::as_str)? {
            "imageGeneration" => Some("Generating image…".into()),
            "reasoning" => Some("Thinking…".into()),
            "webSearch" => Some("Searching the web…".into()),
            _ => None,
        },
        _ => None,
    }
}

/// What a plan update says in the status line: steps done, then the step in progress.
fn plan_status(params: &Value) -> Option<String> {
    let plan = params.get("plan")?.as_array()?;
    if plan.is_empty() {
        return None;
    }
    let status = |step: &Value| step.get("status").and_then(Value::as_str).unwrap_or_default().to_owned();
    let done = plan.iter().filter(|step| status(step) == "completed").count();
    let mut text = format!("Plan {done}/{}", plan.len());
    let current = plan.iter().find(|step| status(step) == "inProgress").or_else(|| plan.iter().find(|step| status(step) == "pending"));
    if let Some(step) = current.and_then(|step| step.get("step")).and_then(Value::as_str) {
        text.push_str(": ");
        text.extend(step.chars().take(100));
    }
    Some(text)
}

/// A turn's agent messages as the chat shows them: each one its own paragraph, and the heading of
/// what Codex is reasoning about for the status line.
#[derive(Default)]
struct Transcript {
    /// Agent messages that streamed deltas, so their completed text isn't shown twice.
    streamed: Vec<String>,
    /// The agent message the last text belonged to.
    last_item: Option<String>,
    /// Some text was shown in this turn.
    any: bool,
    /// The reasoning summary part being read: its item, part and text so far.
    reasoning: Option<(String, i64, String)>,
    /// The last heading shown.
    title: Option<String>,
}

impl Transcript {
    /// The text to show for a delta of agent message `item`: a new message starts a paragraph.
    fn delta(&mut self, item: Option<&str>, delta: &str) -> String {
        let mut text = String::new();
        if let Some(item) = item {
            if self.any && self.last_item.as_deref() != Some(item) {
                text.push_str("\n\n");
            }
            if !self.streamed.iter().any(|seen| seen == item) && self.streamed.len() < 512 {
                self.streamed.push(item.into());
            }
            self.last_item = Some(item.into());
        }
        text.push_str(delta);
        self.any |= !delta.is_empty();
        text
    }

    /// The text to show when agent message `item` completes: all of it, unless it streamed.
    fn completed(&mut self, item: Option<&str>, full: &str) -> Option<String> {
        if full.is_empty() || item.is_some_and(|id| self.streamed.iter().any(|seen| seen == id)) {
            return None;
        }
        Some(self.delta(item, full))
    }

    /// "Thinking: <heading>" when a reasoning summary part's bold heading is complete and new.
    fn thinking(&mut self, item: &str, part: i64, delta: &str) -> Option<String> {
        match &mut self.reasoning {
            Some((id, index, text)) if id == item && *index == part => {
                if text.len() < 4096 {
                    text.push_str(delta);
                }
            }
            _ => self.reasoning = Some((item.into(), part, delta.into())),
        }
        let text = &self.reasoning.as_ref()?.2;
        let (title, _) = text.trim_start().strip_prefix("**")?.split_once("**")?;
        let title = title.trim();
        if title.is_empty() || title.len() > 120 || self.title.as_deref() == Some(title) {
            return None;
        }
        self.title = Some(title.to_owned());
        Some(format!("Thinking: {title}"))
    }
}

fn validate_png(bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_IMAGE_BYTES || bytes.get(..8) != Some(b"\x89PNG\r\n\x1a\n") {
        return Err("The image must be a PNG smaller than 32 MiB.".into());
    }
    Ok(())
}

fn editor_tool_specs() -> Value {
    json!([
        {"name":"yorusphoto_commands","description":"List available yorusphoto editor commands, their parameters, and the current document state.","inputSchema":{"type":"object","properties":{},"additionalProperties":false}},
        {"name":"yorusphoto_execute","description":"Run an undoable yorusphoto command in the live image editor. Inspect yorusphoto_commands first.","inputSchema":{"type":"object","properties":{"command":{"type":"string"},"params":{"type":"object","additionalProperties":true}},"required":["command","params"],"additionalProperties":false}}
    ])
}

fn begin_editor_tool(
    params: &Value,
    commands: Option<&mpsc::Sender<AgentCommand>>,
    cancel: &AtomicBool,
) -> Result<mpsc::Receiver<Result<Value, String>>, String> {
    if cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    let commands = commands.ok_or("Editor commands are not enabled for this image-only request.")?;
    let tool = params.get("tool").and_then(Value::as_str).unwrap_or_default();
    let arguments = params.get("arguments").unwrap_or(&Value::Null);
    let (command, params) = match tool {
        "yorusphoto_commands" => ("__list_commands".to_owned(), json!({})),
        "yorusphoto_execute" => {
            let command = arguments.get("command").and_then(Value::as_str).ok_or("Missing command ID.")?;
            let params = arguments.get("params").filter(|value| value.is_object()).ok_or("Command params must be an object.")?;
            if command.is_empty() || command.len() > 256 || params.to_string().len() > MAX_PROMPT_BYTES {
                return Err("Editor command arguments are too large.".into());
            }
            (command.to_owned(), params.clone())
        }
        _ => return Err("Unknown editor tool.".into()),
    };
    let (reply, result) = mpsc::channel();
    commands.send(AgentCommand { command, params, reply }).map_err(|_| "The image editor closed.".to_owned())?;
    Ok(result)
}

#[cfg(test)]
fn execute_editor_tool(params: &Value, commands: Option<&mpsc::Sender<AgentCommand>>, cancel: &AtomicBool) -> Result<Value, String> {
    let result = begin_editor_tool(params, commands, cancel)?;
    let deadline = Instant::now() + TOOL_TIMEOUT;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err("Cancelled.".into());
        }
        if Instant::now() >= deadline {
            return Err("The image editor did not complete its command within 30 minutes.".into());
        }
        match result.recv_timeout(Duration::from_millis(100)) {
            Ok(result) => return result,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("The image editor closed its command connection.".into()),
        }
    }
}

fn resolve_codex(configured: &str) -> Result<PathBuf, String> {
    if !configured.trim().is_empty() {
        let path = PathBuf::from(configured.trim());
        if path.is_file()
            && (!cfg!(windows) || path.extension().and_then(|extension| extension.to_str()).is_some_and(|extension| extension.eq_ignore_ascii_case("exe")))
        {
            return Ok(path);
        }
        return Err("Choose the native Codex executable (codex.exe on Windows), rather than an npm .cmd or .ps1 launcher.".into());
    }
    if let Some(managed) = crate::codex_update::managed_runtime() {
        return Ok(managed);
    }
    let executable = if cfg!(windows) { "codex.exe" } else { "codex" };
    let mut candidates = Vec::new();
    if let Ok(current) = std::env::current_exe()
        && let Some(parent) = current.parent()
    {
        candidates.push(parent.join("codex").join("bin").join(executable));
        candidates.push(parent.join(executable));
    }
    if let Some(paths) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&paths).map(|path| path.join(executable)));
    }
    #[cfg(windows)]
    if let Some(roaming) = std::env::var_os("APPDATA") {
        for arch in ["x64", "arm64"] {
            let triple = if arch == "x64" { "x86_64-pc-windows-msvc" } else { "aarch64-pc-windows-msvc" };
            candidates.push(
                PathBuf::from(&roaming).join(format!("npm/node_modules/@openai/codex/node_modules/@openai/codex-win32-{arch}/vendor/{triple}/bin/codex.exe")),
            );
        }
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| "Codex was not found. Install the Codex CLI or select its native executable in the Codex panel.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct LoginMock {
        replies: VecDeque<Result<Value, String>>,
        notifications: VecDeque<Result<Value, String>>,
        calls: Vec<(String, Value)>,
        cancelled: Vec<String>,
        cancel_on_receive: Option<Arc<AtomicBool>>,
    }

    impl LoginConnection for LoginMock {
        fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.calls.push((method.into(), params));
            self.replies.pop_front().expect("mock RPC response")
        }

        fn receive_login(&mut self, _deadline: Instant) -> Result<Value, String> {
            if let Some(cancel) = &self.cancel_on_receive {
                cancel.store(true, Ordering::Relaxed);
            }
            self.notifications.pop_front().expect("mock notification")
        }

        fn reject_request(&mut self, _message: &Value) -> Result<(), String> {
            Ok(())
        }

        fn cancel_login(&mut self, login_id: &str) {
            self.cancelled.push(login_id.into());
        }
    }

    fn login_mock(notifications: impl IntoIterator<Item = Result<Value, String>>) -> LoginMock {
        LoginMock {
            replies: [
                Ok(json!({"account":null,"requiresOpenaiAuth":true})),
                Ok(json!({"type":"chatgpt","loginId":"fixture-login","authUrl":"https://chatgpt.com/auth/authorize?code=fixture-secret"})),
            ]
            .into(),
            notifications: notifications.into_iter().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn sign_in_accepts_only_exact_https_openai_authorization_hosts() {
        for valid in ["https://auth.openai.com/authorize?client_id=fixture", "https://chatgpt.com/auth/authorize", "https://chatgpt.com:443/auth/authorize"] {
            assert!(approved_login_url(valid), "{valid}");
        }
        for invalid in [
            "http://auth.openai.com/authorize",
            "https://auth.openai.com.example.org/authorize",
            "https://example.org/auth.openai.com/authorize",
            "https://openai.com/authorize",
            "https://auth.openai.com./authorize",
            "https://auth.openai.com:444/authorize",
            "https://auth.openai.com@evil.example/authorize",
            "https://user@chatgpt.com/authorize",
            "https://@chatgpt.com/authorize",
            "https://:password@chatgpt.com/authorize",
            "https://chatgpt.com\n/authorize",
            "not a URL",
        ] {
            assert!(!approved_login_url(invalid), "{invalid}");
        }
        assert!(!approved_login_url(&format!("https://chatgpt.com/?{}", "x".repeat(16 * 1024))));
    }

    #[test]
    fn sign_in_reuses_a_connected_chatgpt_account_without_opening_a_browser() {
        let mut server = LoginMock { replies: [Ok(json!({"account":{"type":"chatgpt","planType":"free"}}))].into(), ..Default::default() };
        let (sender, receiver) = mpsc::channel();
        let opened = std::cell::Cell::new(false);
        login_flow(&mut server, &AtomicBool::new(false), &sender, |_| {
            opened.set(true);
            Ok(())
        })
        .expect("existing account is connected");
        assert!(!opened.get());
        assert!(receiver.try_recv().is_err());
        assert_eq!(server.calls, [("account/read".into(), json!({"refreshToken":false}))]);
        assert!(server.cancelled.is_empty());
    }

    #[test]
    fn sign_in_manual_link_survives_browser_failure_and_waits_for_matching_completion() {
        let mut server = login_mock([
            Ok(json!({"method":"account/login/completed","params":{"loginId":"another-login","success":false}})),
            Ok(json!({"method":"account/login/completed","params":{"loginId":"fixture-login","success":true}})),
        ]);
        server.replies.push_back(Ok(json!({"account":{"type":"chatgpt","planType":"plus"}})));
        let (sender, receiver) = mpsc::channel();
        login_flow(&mut server, &AtomicBool::new(false), &sender, |url| {
            assert_eq!(url, "https://chatgpt.com/auth/authorize?code=fixture-secret");
            assert!(matches!(receiver.try_recv(), Ok(CodexLoginEvent::OpeningBrowser { .. })));
            Err(())
        })
        .expect("manual browser sign-in remains available");
        let event = receiver.try_recv().expect("browser failure event");
        assert!(matches!(event, CodexLoginEvent::BrowserError(message) if !message.contains("fixture-secret") && message.contains("sign-in page")));
        assert!(receiver.try_recv().is_err());
        assert_eq!(server.calls.last(), Some(&("account/read".into(), json!({"refreshToken":false}))));
        assert!(server.cancelled.is_empty());
    }

    #[test]
    fn sign_in_cancellation_wins_over_a_simultaneous_success_and_cancels_the_request() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut server = login_mock([Ok(json!({"method":"account/login/completed","params":{"loginId":"fixture-login","success":true}}))]);
        server.cancel_on_receive = Some(cancel.clone());
        let (sender, _receiver) = mpsc::channel();
        let error = login_flow(&mut server, &cancel, &sender, |_| Ok(())).expect_err("user cancelled");
        assert_eq!(error, "ChatGPT sign-in cancelled.");
        assert_eq!(server.cancelled, ["fixture-login"]);
        assert_eq!(server.calls.len(), 2);
    }

    #[test]
    fn sign_in_rejects_bad_addresses_before_launch_and_redacts_provider_errors() {
        let mut bad_address = login_mock([]);
        bad_address.replies.get_mut(1).expect("login response").as_mut().expect("success response")["authUrl"] =
            json!("https://example.org/?code=fixture-secret");
        let (sender, receiver) = mpsc::channel();
        let opened = std::cell::Cell::new(false);
        let error = login_flow(&mut bad_address, &AtomicBool::new(false), &sender, |_| {
            opened.set(true);
            Ok(())
        })
        .expect_err("address rejected");
        assert!(!opened.get());
        assert!(receiver.try_recv().is_err());
        assert!(!error.contains("fixture-secret"));
        assert_eq!(bad_address.cancelled, ["fixture-login"]);

        let mut failed = login_mock([Ok(json!({"method":"account/login/completed","params":{
            "loginId":"fixture-login","success":false,"error":"https://auth.openai.com/?code=fixture-secret"
        }}))]);
        let error = login_flow(&mut failed, &AtomicBool::new(false), &sender, |_| Ok(())).expect_err("provider failure");
        assert!(!error.contains("fixture-secret"));
        assert!(!error.contains("https://"));
        assert_eq!(error, "ChatGPT sign-in was cancelled or failed. Start sign-in again.");
    }

    #[test]
    fn sign_in_connection_loss_cancels_pending_authorization_without_leaking_diagnostics() {
        let mut server = login_mock([Err("transport contains https://auth.openai.com/?code=fixture-secret".into())]);
        let (sender, _receiver) = mpsc::channel();
        let error = login_flow(&mut server, &AtomicBool::new(false), &sender, |_| Ok(())).expect_err("connection closed");
        assert_eq!(error, "The ChatGPT sign-in connection closed. Start sign-in again.");
        assert_eq!(server.cancelled, ["fixture-login"]);
    }

    #[test]
    fn parses_only_json_protocol_messages() {
        assert!(read_message(&mut std::io::Cursor::new(b"{\"id\":1,\"result\":{}}\n")).is_ok());
        assert!(read_message(&mut std::io::Cursor::new(b"not json\n")).is_err());
        assert!(read_message(&mut std::io::Cursor::new(b"")).is_err());
    }

    #[test]
    fn rejects_missing_prompts_and_non_png_images() {
        assert!(validate_prompt(" ").is_err());
        assert!(validate_prompt(&"x".repeat(MAX_PROMPT_BYTES + 1)).is_err());
        assert!(validate_png(b"not an image").is_err());
    }

    #[test]
    fn attached_reference_uses_the_conversation_image_without_a_model_facing_path() {
        let path = Path::new(r"C:\Users\Example User\AppData\Local\Temp\selected-reference.png");
        let input = turn_input("Add a flying character while preserving the photograph.", &[path.to_owned()]);
        assert_eq!(input.len(), 2);
        let text = input.first().and_then(|item| item.get("text")).and_then(Value::as_str).expect("reference instruction");
        assert!(text.contains("num_last_images_to_include: 1"));
        assert!(text.contains("omit referenced_image_paths"));
        assert!(!text.contains(&path.to_string_lossy().into_owned()));
        assert_eq!(input.get(1), Some(&json!({"type":"localImage","path":path,"detail":"original"})));
        let without_reference = turn_input("Create a fresh image.", &[]);
        assert_eq!(without_reference, vec![json!({"type":"text","text":"Create a fresh image."})]);
    }

    #[test]
    fn every_uploaded_reference_is_attached_once_in_order_with_the_exact_tool_count() {
        for count in 1..=5 {
            let paths: Vec<_> = (0..count).map(|index| PathBuf::from(format!("C:/private/reference-{index}.png"))).collect();
            let input = turn_input("Use all the attached references.", &paths);
            assert_eq!(input.len(), count + 1);
            let text = input.first().unwrap()["text"].as_str().unwrap();
            assert!(text.contains(&format!("num_last_images_to_include: {count}")));
            assert!(text.contains("omit referenced_image_paths"));
            assert!(!text.contains("C:/private"));
            for (item, path) in input.iter().skip(1).zip(&paths) {
                assert_eq!(item, &json!({"type":"localImage","path":path,"detail":"original"}));
            }
        }
    }

    #[test]
    fn selected_crop_and_uploads_use_distinct_immutable_files_and_are_cleaned_up() {
        let workspace = TemporaryWorkspace::new().unwrap();
        let root = workspace.0.clone();
        // Minimal fixture accepted by the existing header validator; UI validates decoding.
        let selection = b"\x89PNG\r\n\x1a\nselection";
        let first: Arc<[u8]> = b"\x89PNG\r\n\x1a\nupload-one".as_slice().into();
        let second: Arc<[u8]> = b"\x89PNG\r\n\x1a\nupload-two".as_slice().into();
        let paths = workspace.image_references(Some(selection), &[first.clone(), second.clone()]).unwrap();
        assert_eq!(paths.len(), 3);
        assert_eq!(fs::read(paths.first().unwrap()).unwrap(), selection);
        assert_eq!(fs::read(paths.get(1).unwrap()).unwrap().as_slice(), first.as_ref());
        assert_eq!(fs::read(paths.get(2).unwrap()).unwrap().as_slice(), second.as_ref());
        assert!(paths.iter().all(|path| path.starts_with(&root)));
        assert_eq!(paths.iter().collect::<std::collections::HashSet<_>>().len(), 3);
        drop(workspace);
        assert!(!root.exists());
    }

    #[test]
    fn malformed_or_excess_uploaded_references_are_rejected_before_any_files_are_written() {
        let workspace = TemporaryWorkspace::new().unwrap();
        let png: Arc<[u8]> = b"\x89PNG\r\n\x1a\nupload".as_slice().into();
        assert!(workspace.image_references(None, &[]).unwrap().is_empty());
        assert!(workspace.image_references(None, &vec![png.clone(); 5]).is_err());
        assert!(workspace.image_references(Some(&png), &[Arc::from(b"invalid".as_slice())]).is_err());
        assert_eq!(fs::read_dir(&workspace.0).unwrap().count(), 0);
    }

    #[test]
    fn rejects_arbitrary_file_paths_from_image_events() {
        let workspace = TemporaryWorkspace::new().expect("temporary workspace");
        let outside = TemporaryWorkspace::new().expect("outside workspace");
        let path = outside.0.join("image.png");
        fs::write(&path, b"\x89PNG\r\n\x1a\n").expect("write fixture");
        let item = json!({"status":"completed","savedPath":path});
        assert!(read_generated_image(&item, &workspace.0, &workspace.0).is_err());
        assert!(read_generated_image(&json!({"status":"completed","savedPath":"relative.png"}), &workspace.0, &workspace.0).is_err());
    }

    #[test]
    fn generated_gifs_and_inline_results_are_read_and_other_files_refused() {
        let workspace = TemporaryWorkspace::new().expect("temporary workspace");
        let gif = workspace.0.join("generated_images").join("animation.gif");
        fs::create_dir_all(gif.parent().unwrap()).unwrap();
        fs::write(&gif, b"GIF89a\x01\x00\x01\x00").unwrap();
        let bytes = read_generated_image(&json!({"status":"completed","savedPath":gif}), &workspace.0, &workspace.0).expect("a saved GIF");
        assert!(bytes.starts_with(b"GIF89a"));
        let text = workspace.0.join("generated_images").join("notes.txt");
        fs::write(&text, b"hello").unwrap();
        assert!(read_generated_image(&json!({"status":"completed","savedPath":text}), &workspace.0, &workspace.0).is_err());
        let inline = base64::engine::general_purpose::STANDARD.encode(b"\x89PNG\r\n\x1a\nrest");
        let bytes = read_generated_image(&json!({"status":"completed","result":inline}), &workspace.0, &workspace.0).expect("an inline PNG");
        assert!(bytes.starts_with(b"\x89PNG"));
        let not_image = base64::engine::general_purpose::STANDARD.encode(b"plain text");
        assert!(read_generated_image(&json!({"status":"completed","result":not_image}), &workspace.0, &workspace.0).is_err());
        assert!(read_generated_image(&json!({"status":"completed","fileId":"file-1"}), &workspace.0, &workspace.0).is_err());
    }

    #[test]
    fn an_image_limit_failure_says_when_it_resets() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let item = |resets: Value| json!({"status":"failed","failure":{"type":"usageLimitExceeded","limitId":"image_gen","resetsAt":resets}});
        assert!(image_failure(&item(json!(1_000_000 + 25 * 60)), now).ends_with("It resets in 25 min."));
        assert!(image_failure(&item(json!(1_000_000 + 2 * 3600 + 5 * 60)), now).ends_with("It resets in 2 h 5 min."));
        assert!(image_failure(&item(json!(1_000_000 + 3 * 3600)), now).ends_with("It resets in 3 h."));
        assert!(image_failure(&item(Value::Null), now).ends_with("Try again later."));
        assert!(image_failure(&json!({"status":"failed"}), now).starts_with("Codex image generation failed"));
    }

    #[test]
    fn agent_messages_are_separate_paragraphs_and_streamed_text_is_not_repeated() {
        let mut transcript = Transcript::default();
        assert_eq!(transcript.delta(Some("m1"), "First"), "First");
        assert_eq!(transcript.delta(Some("m1"), " note."), " note.");
        assert_eq!(transcript.completed(Some("m1"), "First note."), None, "a streamed message is not sent twice");
        assert_eq!(transcript.delta(Some("m2"), "Second"), "\n\nSecond");
        assert_eq!(transcript.completed(Some("m3"), "Third, whole."), Some("\n\nThird, whole.".into()));
        assert_eq!(transcript.completed(Some("m4"), ""), None);
        let mut fresh = Transcript::default();
        assert_eq!(fresh.completed(Some("only"), "Only one."), Some("Only one.".into()), "the first message has no break before it");
    }

    #[test]
    fn reasoning_summaries_show_their_heading_once() {
        let mut transcript = Transcript::default();
        assert_eq!(transcript.thinking("r1", 0, "**Planning the"), None);
        assert_eq!(transcript.thinking("r1", 0, " layers**\n\nI will"), Some("Thinking: Planning the layers".into()));
        assert_eq!(transcript.thinking("r1", 0, " split the sprite."), None);
        assert_eq!(transcript.thinking("r1", 1, "**Checking colours**"), Some("Thinking: Checking colours".into()));
        assert_eq!(transcript.thinking("r2", 0, "No heading here"), None);
    }

    #[test]
    fn a_refused_steering_message_goes_back_to_the_composer() {
        let (events, received) = mpsc::channel();
        let mut steering = vec![(7, "Make it blue".to_owned()), (8, "And bigger".to_owned())];
        steering_reply(&json!({"id":7,"result":{"turnId":"t"}}), &mut steering, Some(&events));
        assert!(received.try_recv().is_err(), "an accepted message stays in the turn");
        steering_reply(&json!({"id":8,"error":{"message":"no active turn"}}), &mut steering, Some(&events));
        assert!(matches!(received.try_recv(), Ok(CodexEvent::Unsent(text)) if text == "And bigger"));
        assert!(steering.is_empty());
        steering_reply(&json!({"id":99,"error":{}}), &mut steering, Some(&events));
        assert!(received.try_recv().is_err(), "a reply to something else is not a steering reply");
    }

    #[test]
    fn progress_notifications_name_the_work_in_the_status_line() {
        let mut transcript = Transcript::default();
        let started = |kind: &str| json!({"item":{"type":kind,"id":"i"}});
        assert_eq!(progress_status("item/started", &started("webSearch"), &mut transcript).as_deref(), Some("Searching the web…"));
        assert_eq!(progress_status("item/started", &started("agentMessage"), &mut transcript), None);
        assert_eq!(progress_status("warning", &json!({"message":"Model is busy"}), &mut transcript).as_deref(), Some("Model is busy"));
        let delta = json!({"itemId":"r","delta":"**Choosing colours**","summaryIndex":0});
        assert_eq!(progress_status("item/reasoning/summaryTextDelta", &delta, &mut transcript).as_deref(), Some("Thinking: Choosing colours"));
        assert_eq!(progress_status("turn/started", &json!({}), &mut transcript), None);
    }

    #[test]
    fn plan_updates_count_done_steps_and_name_the_current_one() {
        let params = json!({"plan":[{"step":"Inspect the canvas","status":"completed"},{"step":"Cut out the sprite","status":"inProgress"},{"step":"Save","status":"pending"}]});
        assert_eq!(plan_status(&params).as_deref(), Some("Plan 1/3: Cut out the sprite"));
        assert_eq!(plan_status(&json!({"plan":[]})), None);
        assert_eq!(plan_status(&json!({"plan":[{"step":"Done","status":"completed"}]})).as_deref(), Some("Plan 1/1"));
    }

    #[test]
    fn editor_tool_requests_are_validated_and_cancelled() {
        let (sender, receiver) = mpsc::channel();
        let cancel = AtomicBool::new(true);
        assert!(execute_editor_tool(&json!({"tool":"yorusphoto_execute","arguments":{"command":"layer.new","params":[]}}), Some(&sender), &cancel).is_err());
        assert!(receiver.try_recv().is_err());
        assert!(execute_editor_tool(&json!({"tool":"unknown"}), Some(&sender), &cancel).is_err());
    }

    #[test]
    fn editor_tools_round_trip_through_the_ui_channel() {
        let (sender, receiver) = mpsc::channel();
        std::thread::scope(|scope| {
            scope.spawn(move || {
                let command: AgentCommand = receiver.recv_timeout(Duration::from_secs(1)).expect("editor command");
                assert_eq!(command.command, "layer.new");
                assert_eq!(command.params, json!({"name":"Generated image"}));
                let parallel: AgentCommand = receiver.recv_timeout(Duration::from_secs(1)).expect("parallel editor command");
                assert_eq!(parallel.params, json!({"name":"Parallel layer"}));
                parallel.reply.send(Ok(json!({"sequence":2}))).expect("parallel editor reply");
                command.reply.send(Ok(json!({"sequence":1}))).expect("editor reply");
            });
            let response = begin_editor_tool(
                &json!({"tool":"yorusphoto_execute","arguments":{"command":"layer.new","params":{"name":"Generated image"}}}),
                Some(&sender),
                &AtomicBool::new(false),
            )
            .expect("pending tool result");
            let parallel = begin_editor_tool(
                &json!({"tool":"yorusphoto_execute","arguments":{"command":"layer.new","params":{"name":"Parallel layer"}}}),
                Some(&sender),
                &AtomicBool::new(false),
            )
            .expect("parallel pending tool result");
            assert_eq!(parallel.recv_timeout(Duration::from_secs(1)).expect("parallel result").expect("parallel success"), json!({"sequence":2}));
            assert_eq!(response.recv_timeout(Duration::from_secs(1)).expect("first result").expect("first success"), json!({"sequence":1}));
        });
    }
}
