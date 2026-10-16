//! Stable official Codex updates, isolated from conversations, images and Codex credentials.
//!
//! Each verified release has its own complete directory. Only a small atomic pointer changes;
//! an already-running agent keeps its executable and a failed update keeps the preceding one.

use base64::Engine;
use photocraft_ui_egui::{CodexUpdateEvent, CodexUpdateRequest};
use serde_json::{Value, json};
use sha2::{Digest, Sha512};
use std::collections::HashSet;
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock, mpsc};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const INTERVAL: u64 = 24 * 60 * 60;
const MAX_META: u64 = 2 << 20;
const MAX_STATE: u64 = 512 << 10;
const MAX_DOWNLOAD: u64 = 256 << 20;
const MAX_UNPACKED: u64 = 1 << 30;
const MAX_FILE: u64 = 768 << 20;
const MAX_ENTRIES: usize = 2048;
const TARGET: &str = "x86_64-pc-windows-msvc";
const REGISTRY: &str = "https://registry.npmjs.org/@openai/codex";
static RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Debug, Default)]
struct State {
    active: Option<String>,
    previous: Option<String>,
    latest: Option<String>,
    checked: Option<u64>,
}

impl State {
    fn read(root: &Path) -> Self {
        let Ok(value) = read_json(&root.join("state.json"), MAX_STATE) else { return Self::default() };
        let version = |key: &str| value.get(key).and_then(Value::as_str).filter(|s| stable_version(s).is_some()).map(str::to_owned);
        Self { active: version("active"), previous: version("previous"), latest: version("latest"), checked: value.get("checked").and_then(Value::as_u64) }
    }

    fn save(&self, root: &Path) -> Result<(), String> {
        let path = root.join("state.json");
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && (!metadata.is_file() || linked(&path, &metadata))
        {
            return Err("The Codex update state is not a regular file; it was not changed.".into());
        }
        let bytes = serde_json::to_vec(&json!({"version":1,"active":self.active,"previous":self.previous,"latest":self.latest,"checked":self.checked}))
            .map_err(|_| "Could not encode the Codex update state.".to_owned())?;
        photocraft_format::atomic_write(&path, &bytes).map_err(|e| format!("Could not save the Codex update state: {e}"))
    }

    fn status(&self, phase: &str, bundled: Option<&str>, active: Option<&str>, message: &str, updated: bool) -> Value {
        json!({
            "phase":phase,"activeVersion":active.or(bundled),"bundledVersion":bundled,"latestVersion":self.latest,
            "lastCheckUnix":self.checked,"nextCheckUnix":self.checked.map(|n| n.saturating_add(INTERVAL)),"message":message,"updated":updated
        })
    }
}

/// Start a check off the UI thread. Automatic checks are daily; an explicit check bypasses it.
pub fn update(request: CodexUpdateRequest, reply: mpsc::Sender<CodexUpdateEvent>) {
    let failed = reply.clone();
    if let Err(error) = std::thread::Builder::new().name("yorusphoto-codex-update".into()).spawn(move || {
        if RUNNING.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            let _ = reply.send(CodexUpdateEvent::Error("A Codex update check is already running.".into()));
            return;
        }
        struct Reset;
        impl Drop for Reset {
            fn drop(&mut self) {
                RUNNING.store(false, Ordering::Release);
            }
        }
        let _reset = Reset;
        if let Err(error) = perform(&request, &reply) {
            let _ = reply.send(CodexUpdateEvent::Error(error));
        }
    }) {
        let _ = failed.send(CodexUpdateEvent::Error(format!("Could not start the Codex updater: {error}")));
    }
}

fn perform(request: &CodexUpdateRequest, reply: &mpsc::Sender<CodexUpdateEvent>) -> Result<(), String> {
    let bundled = bundled_version();
    if !request.configured_path.trim().is_empty() {
        let state = State::default();
        let mut status =
            state.status("custom", bundled.as_deref(), None, "A custom Codex executable is selected. Its installation is managed separately.", false);
        if let Some(object) = status.as_object_mut() {
            object.insert("activeVersion".into(), Value::Null);
        }
        emit(reply, status);
        return Ok(());
    }
    if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        return Err("Automatic Codex runtime updates currently support Windows x64. The bundled or custom runtime remains available.".into());
    }
    let root = crate::app_dirs::config_dir().ok_or("No writable configuration directory is available for Codex updates.")?.join("codex-updates");
    ensure_directory(&root)?;
    let _lease = Lease::acquire(&root)?;
    let mut state = State::read(&root);
    let active = resolve_managed(&root, &state)
        .map(|(version, _)| version)
        .filter(|version| bundled.as_deref().is_none_or(|bundle| stable_version(version) >= stable_version(bundle)));
    if !should_check(state.checked, now(), request.force) {
        emit(
            reply,
            state.status("throttled", bundled.as_deref(), active.as_deref(), "Codex was checked within the last 24 hours. Check now to check again.", false),
        );
        return Ok(());
    }
    check_cancel(&request.cancel)?;
    state.checked = Some(now());
    state.save(&root)?;
    emit(reply, state.status("checking", bundled.as_deref(), active.as_deref(), "Checking the official stable Codex release.", false));
    let result = install(&root, &mut state, bundled.as_deref(), active.as_deref(), &request.cancel, reply);
    if let Err(error) = &result {
        // Do not persist response bodies, credentials, account data or download URLs.
        emit(reply, state.status("error", bundled.as_deref(), active.as_deref(), &error.chars().take(300).collect::<String>(), false));
    }
    result
}

fn emit(reply: &mpsc::Sender<CodexUpdateEvent>, value: Value) {
    let _ = reply.send(CodexUpdateEvent::Status(value));
}

fn install(
    root: &Path,
    state: &mut State,
    bundled: Option<&str>,
    active: Option<&str>,
    cancel: &AtomicBool,
    reply: &mpsc::Sender<CodexUpdateEvent>,
) -> Result<(), String> {
    let client = reqwest::blocking::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(15))
        .timeout(Duration::from_secs(300))
        .user_agent("yorusphoto-codex-updater")
        .build()
        .map_err(|_| "Could not prepare the secure Codex update connection.")?;
    let latest = fetch_json(&client, &format!("{REGISTRY}/latest"), cancel)?;
    let version = latest
        .get("version")
        .and_then(Value::as_str)
        .filter(|v| stable_version(v).is_some())
        .ok_or("The registry did not report a stable Codex version.")?
        .to_owned();
    if latest.get("name").and_then(Value::as_str) != Some("@openai/codex") {
        return Err("Unexpected Codex registry package.".into());
    }
    state.latest = Some(version.clone());
    state.save(root)?;
    let current = newer_version(active, bundled);
    if current.is_some_and(|current| stable_version(current) >= stable_version(&version)) {
        emit(reply, state.status("current", bundled, active, "The latest stable Codex runtime is already available.", false));
        return Ok(());
    }
    let native_version = format!("{version}-win32-x64");
    let expected_alias = format!("npm:@openai/codex@{native_version}");
    if latest.pointer("/optionalDependencies/@openai~1codex-win32-x64").and_then(Value::as_str) != Some(expected_alias.as_str()) {
        return Err("The official Codex Windows package mapping changed. The current runtime was retained.".into());
    }
    let metadata = fetch_json(&client, &format!("{REGISTRY}/{native_version}"), cancel)?;
    let package = Package::parse(&metadata, &version)?;
    emit(reply, state.status("downloading", bundled, active, "Downloading and verifying the official Windows x64 Codex package.", false));
    let mut stage = Stage::new(root)?;
    let archive_path = stage.path.join("download.tgz");
    download(&client, &package.url, &archive_path, &package.integrity, cancel)?;
    let runtime = stage.path.join("runtime");
    ensure_directory(&runtime)?;
    let manifest = extract(&archive_path, &runtime, &version, cancel)?;
    emit(reply, state.status("validating", bundled, active, "Checking the executable version and app-server protocol before activation.", false));
    let binary = runtime.join(manifest.binary()?);
    probe(&binary, &version, &stage.path.join("probe-home"), cancel)?;
    check_cancel(cancel)?;
    photocraft_format::atomic_write(
        &runtime.join("verified.json"),
        &serde_json::to_vec(&manifest.0).map_err(|_| "Could not encode the verified runtime manifest.")?,
    )
    .map_err(|e| format!("Could not save runtime verification: {e}"))?;
    let versions = root.join("versions");
    ensure_directory(&versions)?;
    let destination = versions.join(&version);
    if destination.exists() {
        // Never overwrite an executable directory, including a version used by another process.
        if verified_runtime(&destination, &version).is_none() {
            return Err("A different or incomplete directory already exists for this Codex version. The current runtime was retained.".into());
        }
    } else {
        std::fs::rename(&runtime, &destination).map_err(|e| format!("Could not install the versioned Codex runtime: {e}"))?;
    }
    let mut next = state.clone();
    next.previous = active.map(str::to_owned);
    next.active = Some(version.clone());
    // A failed pointer write cannot deactivate the old release.
    next.save(root)?;
    *state = next;
    stage.cleanup();
    emit(reply, state.status("ready", bundled, Some(&version), "Codex updated. New tasks use this release; running tasks continue uninterrupted.", true));
    Ok(())
}

struct Package {
    url: String,
    integrity: Vec<u8>,
}
impl Package {
    fn parse(value: &Value, version: &str) -> Result<Self, String> {
        let native = format!("{version}-win32-x64");
        if value.get("name").and_then(Value::as_str) != Some("@openai/codex") || value.get("version").and_then(Value::as_str) != Some(native.as_str()) {
            return Err("The registry returned an unexpected Codex platform package.".into());
        }
        let url = format!("{REGISTRY}/-/codex-{native}.tgz");
        if value.pointer("/dist/tarball").and_then(Value::as_str) != Some(url.as_str()) {
            return Err("The Codex package download is not on the expected official registry path.".into());
        }
        let integrity = value
            .pointer("/dist/integrity")
            .and_then(Value::as_str)
            .and_then(|s| s.strip_prefix("sha512-"))
            .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
            .filter(|v| v.len() == 64)
            .ok_or("The Codex package has no valid SHA-512 integrity value.")?;
        if value.pointer("/dist/unpackedSize").and_then(Value::as_u64).is_some_and(|n| n > MAX_UNPACKED) {
            return Err("The Codex package exceeds the extraction limit.".into());
        }
        Ok(Self { url, integrity })
    }
}

fn fetch_json(client: &reqwest::blocking::Client, url: &str, cancel: &AtomicBool) -> Result<Value, String> {
    check_cancel(cancel)?;
    let response = client.get(url).send().map_err(|_| "Could not reach the official Codex update registry. The current runtime is available offline.")?;
    if !response.status().is_success() {
        return Err(format!("The official Codex registry returned HTTP {}. The current runtime was retained.", response.status().as_u16()));
    }
    if response.content_length().is_some_and(|n| n > MAX_META) {
        return Err("Codex update metadata exceeds the size limit.".into());
    }
    let mut bytes = Vec::new();
    response.take(MAX_META + 1).read_to_end(&mut bytes).map_err(|_| "Could not read Codex update metadata.")?;
    check_cancel(cancel)?;
    if bytes.len() as u64 > MAX_META {
        return Err("Codex update metadata exceeds the size limit.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "The official registry returned invalid Codex update metadata.".into())
}

fn download(client: &reqwest::blocking::Client, url: &str, path: &Path, expected: &[u8], cancel: &AtomicBool) -> Result<(), String> {
    let mut response = client.get(url).send().map_err(|_| "Could not download the Codex runtime. The current runtime was retained.")?;
    if !response.status().is_success() {
        return Err("The official Codex package download was unavailable.".into());
    }
    if response.content_length().is_some_and(|n| n > MAX_DOWNLOAD) {
        return Err("The Codex package exceeds the download limit.".into());
    }
    let mut file = OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| format!("Could not create the Codex download: {e}"))?;
    let mut hash = Sha512::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 64 << 10];
    loop {
        check_cancel(cancel)?;
        let n = response.read(&mut buffer).map_err(|_| "The Codex download was interrupted. The current runtime was retained.")?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if total > MAX_DOWNLOAD {
            return Err("The Codex package exceeds the download limit.".into());
        }
        let bytes = buffer.get(..n).ok_or("Invalid Codex download buffer length.")?;
        hash.update(bytes);
        file.write_all(bytes).map_err(|e| format!("Could not save the Codex download: {e}"))?;
    }
    verify_integrity(&hash.finalize(), expected)?;
    file.sync_all().map_err(|e| format!("Could not flush the Codex download: {e}"))
}

fn verify_integrity(actual: &[u8], expected: &[u8]) -> Result<(), String> {
    if actual.len() != 64 || expected.len() != 64 || actual != expected {
        return Err("The Codex package failed SHA-512 integrity verification. The current runtime was retained.".into());
    }
    Ok(())
}

#[derive(Debug)]
struct Manifest(Value);
impl Manifest {
    fn binary(&self) -> Result<&str, String> {
        self.0.get("binary").and_then(Value::as_str).ok_or_else(|| "The Codex runtime manifest has no executable.".into())
    }
}

fn extract(archive: &Path, destination: &Path, version: &str, cancel: &AtomicBool) -> Result<Manifest, String> {
    let file = File::open(archive).map_err(|e| format!("Could not open the verified Codex package: {e}"))?;
    let decoder = flate2::read::GzDecoder::new(file);
    extract_tar(decoder, destination, version, cancel)
}

fn extract_tar(reader: impl Read, destination: &Path, version: &str, cancel: &AtomicBool) -> Result<Manifest, String> {
    let mut archive = tar::Archive::new(reader.take(MAX_UNPACKED.saturating_add(8 << 20)));
    let entries = archive.entries().map_err(|_| "The verified Codex package is not a valid archive.")?;
    let mut files = Vec::new();
    let mut seen = HashSet::new();
    let mut total = 0u64;
    let mut binary = None;
    for (count, entry) in entries.enumerate() {
        check_cancel(cancel)?;
        if count >= MAX_ENTRIES {
            return Err("The Codex package contains too many entries.".into());
        }
        let mut entry = entry.map_err(|_| "Could not read a Codex package entry.")?;
        let relative = entry.path().map_err(|_| "A Codex archive path is invalid.")?;
        let text = relative.to_str().ok_or("A Codex archive path is not UTF-8.")?.to_owned();
        validate_relative(&text)?;
        if !text.starts_with("package/") {
            return Err("The Codex package has an unexpected archive root.".into());
        }
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            return Err("Links and special files are not permitted in a Codex update.".into());
        }
        let target = destination.join(&text);
        if kind.is_dir() {
            ensure_directory(&target)?;
            continue;
        }
        if !seen.insert(text.to_ascii_lowercase()) {
            return Err("The Codex package contains duplicate file paths.".into());
        }
        let size = entry.size();
        total = total.checked_add(size).ok_or("The Codex extraction size overflowed.")?;
        if size > MAX_FILE || total > MAX_UNPACKED {
            return Err("The Codex package exceeds the extraction limit.".into());
        }
        if let Some(parent) = target.parent() {
            ensure_directory(parent)?;
        }
        let mut output = OpenOptions::new().write(true).create_new(true).open(&target).map_err(|e| format!("Could not create a Codex runtime file: {e}"))?;
        let mut hash = Sha512::new();
        let copied = copy_hash(&mut entry, &mut output, &mut hash, size, cancel)?;
        if copied != size {
            return Err("A Codex runtime file was truncated.".into());
        }
        output.sync_all().map_err(|e| format!("Could not flush a Codex runtime file: {e}"))?;
        let checksum = base64::engine::general_purpose::STANDARD.encode(hash.finalize());
        files.push(json!({"path":text,"bytes":size,"sha512":checksum}));
        if text == format!("package/vendor/{TARGET}/bin/codex.exe") || text == format!("package/vendor/{TARGET}/codex/codex.exe") {
            if binary.is_some() {
                return Err("The Codex package contains multiple native entrypoints.".into());
            }
            binary = Some(text);
        }
    }
    let binary = binary.ok_or("The Codex Windows x64 package did not contain its native executable.")?;
    Ok(Manifest(json!({"format":1,"version":version,"target":TARGET,"binary":binary,"files":files})))
}

fn copy_hash(reader: &mut impl Read, writer: &mut impl Write, hash: &mut Sha512, max: u64, cancel: &AtomicBool) -> Result<u64, String> {
    let mut total = 0u64;
    let mut buffer = [0u8; 64 << 10];
    loop {
        check_cancel(cancel)?;
        let n = reader.read(&mut buffer).map_err(|_| "Could not read a Codex runtime file.")?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n as u64);
        if total > max {
            return Err("A Codex runtime file exceeded its declared size.".into());
        }
        let bytes = buffer.get(..n).ok_or("Invalid Codex extraction buffer length.")?;
        hash.update(bytes);
        writer.write_all(bytes).map_err(|e| format!("Could not write a Codex runtime file: {e}"))?;
    }
    Ok(total)
}

fn validate_relative(path: &str) -> Result<(), String> {
    if path.len() > 512 || path.is_empty() || path.contains(['\\', ':', '\0']) || path.chars().any(char::is_control) {
        return Err("A Codex archive path is unsafe.".into());
    }
    for component in Path::new(path).components() {
        let Component::Normal(part) = component else { return Err("A Codex archive path leaves its version directory.".into()) };
        let part = part.to_str().ok_or("A Codex archive path is invalid.")?;
        if part.ends_with(['.', ' '])
            || part.split('.').next().is_some_and(|s| {
                let name = s.to_ascii_uppercase();
                matches!(name.as_str(), "CON" | "PRN" | "AUX" | "NUL")
                    || (name.len() == 4 && (name.starts_with("COM") || name.starts_with("LPT")) && name.as_bytes().get(3).is_some_and(u8::is_ascii_digit))
            })
        {
            return Err("A Codex archive path uses a reserved Windows filename.".into());
        }
    }
    if path.split('/').any(|part| part.is_empty() || part == "." || part == "..") {
        return Err("A Codex archive path is unsafe.".into());
    }
    Ok(())
}

fn stable_version(version: &str) -> Option<(u32, u32, u32)> {
    if version.len() > 32 {
        return None;
    }
    let mut parts = version.split('.');
    let mut number = || {
        parts
            .next()
            .filter(|s| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit()) && (s.len() == 1 || !s.starts_with('0')))
            .and_then(|s| s.parse::<u32>().ok())
    };
    let result = (number()?, number()?, number()?);
    if parts.next().is_some() {
        return None;
    }
    Some(result)
}

fn newer_version<'a>(a: Option<&'a str>, b: Option<&'a str>) -> Option<&'a str> {
    match (a.filter(|s| stable_version(s).is_some()), b.filter(|s| stable_version(s).is_some())) {
        (Some(a), Some(b)) => Some(if stable_version(a) >= stable_version(b) { a } else { b }),
        (a, b) => a.or(b),
    }
}

fn should_check(checked: Option<u64>, current: u64, force: bool) -> bool {
    force || checked.is_none_or(|last| current < last || current.saturating_sub(last) >= INTERVAL)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_secs()
}
fn check_cancel(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Relaxed) { Err("Codex update cancelled. The current runtime was retained.".into()) } else { Ok(()) }
}

fn read_json(path: &Path, max: u64) -> Result<Value, String> {
    regular_metadata(path)?;
    let file = File::open(path).map_err(|_| "Could not read Codex update state.")?;
    let mut bytes = Vec::new();
    file.take(max.saturating_add(1)).read_to_end(&mut bytes).map_err(|_| "Could not read Codex update state.")?;
    if bytes.len() as u64 > max {
        return Err("Codex update state exceeds the size limit.".into());
    }
    serde_json::from_slice(&bytes).map_err(|_| "Codex update state is invalid.".into())
}

fn linked(path: &Path, metadata: &std::fs::Metadata) -> bool {
    // OneDrive cloud placeholders carry FILE_ATTRIBUTE_REPARSE_POINT too. They are ordinary
    // local files/directories for this purpose, unlike readable symlink/junction targets.
    metadata.file_type().is_symlink() || std::fs::read_link(path).is_ok()
}

fn regular_metadata(path: &Path) -> Result<std::fs::Metadata, String> {
    let metadata = std::fs::symlink_metadata(path).map_err(|_| "A Codex runtime file is unavailable.")?;
    if !metadata.is_file() || linked(path, &metadata) {
        return Err("A Codex runtime file is not a regular file.".into());
    }
    Ok(metadata)
}

fn ensure_directory(path: &Path) -> Result<(), String> {
    // Walk each existing component: create_dir_all would otherwise follow a junction or symlink.
    let mut cursor = PathBuf::new();
    for part in path.components() {
        cursor.push(part);
        match std::fs::symlink_metadata(&cursor) {
            Ok(metadata) if !metadata.is_dir() || linked(&cursor, &metadata) => {
                return Err("The Codex update directory contains a link or non-directory.".into());
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir(&cursor).map_err(|e| format!("Could not create the Codex update directory: {e}"))?
            }
            Err(error) => return Err(format!("Could not inspect the Codex update directory: {error}")),
        }
    }
    Ok(())
}

/// Resolve an installed, complete, verified release without network requests or subprocesses.
pub fn managed_runtime() -> Option<PathBuf> {
    if !cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        return None;
    }
    let root = crate::app_dirs::config_dir()?.join("codex-updates");
    let (version, path) = resolve_managed(&root, &State::read(&root))?;
    if bundled_version().is_some_and(|bundle| stable_version(&bundle) > stable_version(&version)) {
        return None;
    }
    Some(path)
}

fn resolve_managed(root: &Path, state: &State) -> Option<(String, PathBuf)> {
    // A copied portable directory may be redirected through a junction after activation.
    for path in root.ancestors() {
        let metadata = std::fs::symlink_metadata(path).ok()?;
        if !metadata.is_dir() || linked(path, &metadata) {
            return None;
        }
    }
    let versions = root.join("versions");
    let metadata = std::fs::symlink_metadata(&versions).ok()?;
    if !metadata.is_dir() || linked(&versions, &metadata) {
        return None;
    }
    for version in [state.active.as_deref(), state.previous.as_deref()].into_iter().flatten() {
        if stable_version(version).is_some()
            && let Some(path) = verified_runtime(&root.join("versions").join(version), version)
        {
            return Some((version.to_owned(), path));
        }
    }
    None
}

// Successful full hashes are cached only while every file's size and modification time match.
// This makes normal app-server setup cheap while a modified or truncated runtime falls back.
type Fingerprint = Vec<(String, u64, SystemTime)>;
type VerificationCache = Vec<(PathBuf, Fingerprint)>;

fn verified_runtime(directory: &Path, version: &str) -> Option<PathBuf> {
    let metadata = std::fs::symlink_metadata(directory).ok()?;
    if !metadata.is_dir() || linked(directory, &metadata) {
        return None;
    }
    let marker = directory.join("verified.json");
    let manifest = read_json(&marker, MAX_STATE).ok()?;
    if manifest.get("format").and_then(Value::as_u64) != Some(1)
        || manifest.get("version").and_then(Value::as_str) != Some(version)
        || manifest.get("target").and_then(Value::as_str) != Some(TARGET)
    {
        return None;
    }
    let binary = manifest.get("binary")?.as_str()?;
    if validate_relative(binary).is_err() || !binary.starts_with("package/") || !binary.ends_with("/codex.exe") {
        return None;
    }
    let files = manifest.get("files")?.as_array()?;
    if files.is_empty() || files.len() > MAX_ENTRIES {
        return None;
    }
    let marker_meta = regular_metadata(&marker).ok()?;
    let mut fingerprint = vec![("verified.json".into(), marker_meta.len(), marker_meta.modified().ok()?)];
    let mut seen = HashSet::new();
    let mut total = 0u64;
    for file in files {
        let path = file.get("path")?.as_str()?;
        validate_relative(path).ok()?;
        if !path.starts_with("package/") || !seen.insert(path.to_ascii_lowercase()) {
            return None;
        }
        let bytes = file.get("bytes")?.as_u64()?;
        total = total.checked_add(bytes)?;
        if bytes > MAX_FILE || total > MAX_UNPACKED {
            return None;
        }
        let full = directory.join(path);
        let mut parent = full.parent();
        while let Some(path) = parent {
            if path == directory {
                break;
            }
            let meta = std::fs::symlink_metadata(path).ok()?;
            if !meta.is_dir() || linked(path, &meta) {
                return None;
            }
            parent = path.parent();
        }
        let meta = regular_metadata(&full).ok()?;
        if meta.len() != bytes {
            return None;
        }
        fingerprint.push((path.to_owned(), bytes, meta.modified().ok()?));
    }
    if !seen.contains(&binary.to_ascii_lowercase()) {
        return None;
    }
    static CACHE: OnceLock<Mutex<VerificationCache>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(Vec::new()));
    if cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).iter().any(|(path, stamp)| path == directory && stamp == &fingerprint) {
        return Some(directory.join(binary));
    }
    for file in files {
        let path = file.get("path")?.as_str()?;
        let expected = file.get("sha512")?.as_str()?;
        let mut hash = Sha512::new();
        let mut input = File::open(directory.join(path)).ok()?;
        let mut buffer = [0u8; 64 << 10];
        loop {
            let n = input.read(&mut buffer).ok()?;
            if n == 0 {
                break;
            }
            hash.update(buffer.get(..n)?);
        }
        if base64::engine::general_purpose::STANDARD.encode(hash.finalize()) != expected {
            return None;
        }
    }
    let mut cache = cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    cache.retain(|(path, _)| path != directory);
    if cache.len() >= 4 {
        cache.remove(0);
    }
    cache.push((directory.to_path_buf(), fingerprint));
    Some(directory.join(binary))
}

fn bundled_version() -> Option<String> {
    let directory = std::env::current_exe().ok()?.parent()?.join("codex");
    let value = read_json(&directory.join("codex-package.json"), MAX_STATE).ok()?;
    value.get("version")?.as_str().filter(|v| stable_version(v).is_some()).map(str::to_owned)
}

struct Lease {
    _file: File,
}
impl Lease {
    fn acquire(root: &Path) -> Result<Self, String> {
        let path = root.join("update.lock");
        if let Ok(metadata) = std::fs::symlink_metadata(&path)
            && (!metadata.is_file() || linked(&path, &metadata))
        {
            return Err("The Codex update lock is not a regular file.".into());
        }
        let file = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(&path).map_err(|_| "Could not open the Codex update lock.")?;
        file.try_lock().map_err(|_| "Another yorusphoto window is checking Codex updates. The current runtime remains available.")?;
        // Keep the file name stable: unlinking a locked file can create a second lock inode.
        // The OS releases this exclusive lease when File drops, including after a crash.
        Ok(Self { _file: file })
    }
}

struct Stage {
    path: PathBuf,
    root: PathBuf,
}
impl Stage {
    fn new(root: &Path) -> Result<Self, String> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = root.join(format!("staging-{}-{}-{}", std::process::id(), now(), NEXT.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir(&path).map_err(|e| format!("Could not prepare the Codex update: {e}"))?;
        Ok(Self { path, root: root.to_path_buf() })
    }
    fn cleanup(&mut self) {
        if let (Ok(path), Ok(root)) = (std::fs::canonicalize(&self.path), std::fs::canonicalize(&self.root))
            && path.parent() == Some(root.as_path())
            && self.path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("staging-"))
        {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}
impl Drop for Stage {
    fn drop(&mut self) {
        self.cleanup();
    }
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn prepare_command(binary: &Path, home: &Path) -> Command {
    let mut command = Command::new(binary);
    command
        .current_dir(home)
        .env("CODEX_HOME", home)
        .env_remove("OPENAI_API_KEY")
        .env_remove("CODEX_API_KEY")
        .env_remove("CODEX_ACCESS_TOKEN")
        .env_remove("OPENAI_ACCESS_TOKEN")
        .env_remove("ACCESS_TOKEN")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        command.creation_flags(0x08000000);
    }
    command
}

fn probe(binary: &Path, expected: &str, home: &Path, cancel: &AtomicBool) -> Result<(), String> {
    ensure_directory(home)?;
    let mut command = prepare_command(binary, home);
    command.arg("--version");
    let mut child = ChildGuard(command.spawn().map_err(|_| "The downloaded Codex executable could not start. The current runtime was retained.")?);
    let stdout = child.0.stdout.take().ok_or("The downloaded Codex executable has no output pipe.")?;
    let (send, receive) = mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("yorusphoto-update-version".into())
        .spawn(move || {
            let mut bytes = Vec::new();
            let result = stdout.take(4097).read_to_end(&mut bytes).ok().filter(|_| bytes.len() <= 4096).map(|_| bytes);
            let _ = send.send(result);
        })
        .map_err(|_| "Could not start the Codex validation reader.")?;
    let deadline = Instant::now() + Duration::from_secs(15);
    let bytes = receive_until(&receive, deadline, cancel)?.ok_or("The downloaded Codex version output exceeded its limit.")?;
    let output = String::from_utf8(bytes).map_err(|_| "The downloaded Codex version output was invalid.")?;
    if output.trim() != format!("codex-cli {expected}") {
        return Err("The downloaded executable did not match the official Codex version.".into());
    }
    loop {
        check_cancel(cancel)?;
        if let Some(status) = child.0.try_wait().map_err(|_| "Could not inspect the downloaded Codex process.")? {
            if !status.success() {
                return Err("The downloaded Codex executable failed validation.".into());
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err("The downloaded Codex version check timed out.".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    drop(child);
    schema_probe(binary, home, cancel)?;
    protocol_probe(binary, home, cancel)
}

fn schema_probe(binary: &Path, home: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let directory = home.join("compatibility-schema");
    ensure_directory(&directory)?;
    let mut command = prepare_command(binary, home);
    command.args(["app-server", "generate-json-schema", "--out"]).arg(&directory).stdin(Stdio::null()).stdout(Stdio::null());
    let mut child = ChildGuard(command.spawn().map_err(|_| "The downloaded Codex could not export its protocol schema.")?);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        check_cancel(cancel)?;
        if let Some(status) = child.0.try_wait().map_err(|_| "Could not inspect the Codex schema probe.")? {
            if !status.success() {
                return Err("The downloaded Codex could not export a compatible protocol schema.".into());
            }
            break;
        }
        if Instant::now() >= deadline {
            return Err("The downloaded Codex schema probe timed out.".into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    let schema = read_json(&directory.join("ClientRequest.json"), 8 << 20)?;
    validate_schema(&schema)
}

fn validate_schema(schema: &Value) -> Result<(), String> {
    let methods = schema.get("oneOf").and_then(Value::as_array).ok_or("The downloaded Codex request schema is incompatible.")?;
    let has_method = |method: &str| {
        methods.iter().any(|entry| {
            entry.pointer("/properties/method/enum").and_then(Value::as_array).is_some_and(|values| values.iter().any(|value| value.as_str() == Some(method)))
        })
    };
    for method in [
        "initialize",
        "account/read",
        "account/rateLimits/read",
        "model/list",
        "thread/start",
        "thread/resume",
        "thread/read",
        "thread/list",
        "thread/fork",
        "thread/name/set",
        "thread/archive",
        "thread/unarchive",
        "thread/compact/start",
        "turn/start",
        "turn/interrupt",
    ] {
        if !has_method(method) {
            return Err(format!("The downloaded Codex does not support required method {method}. The current runtime was retained."));
        }
    }
    let input = schema.pointer("/definitions/UserInput/oneOf").and_then(Value::as_array).ok_or("The downloaded Codex input schema is incompatible.")?;
    if !input.iter().any(|entry| {
        entry.pointer("/properties/type/enum").and_then(Value::as_array).is_some_and(|values| values.iter().any(|value| value.as_str() == Some("localImage")))
    }) {
        return Err("The downloaded Codex cannot accept selected-area image references. The current runtime was retained.".into());
    }
    Ok(())
}

fn receive_until<T>(receive: &mpsc::Receiver<T>, deadline: Instant, cancel: &AtomicBool) -> Result<T, String> {
    loop {
        check_cancel(cancel)?;
        if Instant::now() >= deadline {
            return Err("The downloaded Codex compatibility check timed out. The current runtime was retained.".into());
        }
        match receive.recv_timeout(Duration::from_millis(50)) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err("The downloaded Codex compatibility check closed unexpectedly.".into()),
        }
    }
}

fn protocol_probe(binary: &Path, home: &Path, cancel: &AtomicBool) -> Result<(), String> {
    let mut command = prepare_command(binary, home);
    command.args(["app-server", "--listen", "stdio://", "--enable", "image_generation"]);
    command.args(["-c", "features.shell_tool=false", "-c", "features.hooks=false", "-c", "cli_auth_credentials_store=\"file\""]);
    let mut child = ChildGuard(command.spawn().map_err(|_| "The downloaded Codex app-server could not start.")?);
    let mut stdin = child.0.stdin.take().ok_or("The downloaded Codex app-server has no input pipe.")?;
    let stdout = child.0.stdout.take().ok_or("The downloaded Codex app-server has no output pipe.")?;
    let (send, receive) = mpsc::sync_channel(4);
    std::thread::Builder::new()
        .name("yorusphoto-update-protocol".into())
        .spawn(move || {
            let mut reader = BufReader::new(stdout);
            for _ in 0..64 {
                let mut bytes = Vec::new();
                let result = (&mut reader).take((256 << 10) + 1).read_until(b'\n', &mut bytes);
                if !matches!(result, Ok(n) if n > 0 && n <= 256 << 10) {
                    break;
                }
                let parsed = serde_json::from_slice::<Value>(&bytes).ok();
                if send.send(parsed).is_err() {
                    break;
                }
            }
        })
        .map_err(|_| "Could not start the Codex protocol validation reader.")?;
    write_rpc(
        &mut stdin,
        &json!({"id":1,"method":"initialize","params":{"clientInfo":{"name":"yorusphoto","version":env!("CARGO_PKG_VERSION")},"capabilities":{"experimentalApi":true}}}),
    )?;
    let deadline = Instant::now() + Duration::from_secs(30);
    let initialized = probe_reply(&receive, 1, deadline, cancel)?;
    if initialized.get("codexHome").and_then(Value::as_str).is_none() {
        return Err("The downloaded Codex app-server cannot report its image output directory.".into());
    }
    write_rpc(&mut stdin, &json!({"method":"initialized","params":{}}))?;
    write_rpc(&mut stdin, &json!({"id":2,"method":"model/list","params":{"limit":1,"includeHidden":true}}))?;
    let models = probe_reply(&receive, 2, deadline, cancel)?;
    if !models.get("data").is_some_and(Value::is_array) {
        return Err("The downloaded Codex app-server returned an incompatible model catalog.".into());
    }
    Ok(())
}

fn write_rpc(writer: &mut impl Write, value: &Value) -> Result<(), String> {
    serde_json::to_writer(&mut *writer, value).map_err(|_| "Could not send the Codex compatibility check.")?;
    writer.write_all(b"\n").and_then(|()| writer.flush()).map_err(|_| "The downloaded Codex compatibility connection closed.".into())
}

fn probe_reply(receive: &mpsc::Receiver<Option<Value>>, id: u64, deadline: Instant, cancel: &AtomicBool) -> Result<Value, String> {
    for _ in 0..64 {
        let value = receive_until(receive, deadline, cancel)?.ok_or("The downloaded Codex app-server emitted invalid JSON.")?;
        if value.get("id").and_then(Value::as_u64) == Some(id) {
            if value.get("error").is_some() {
                return Err("The downloaded Codex app-server rejected the compatibility check.".into());
            }
            return value.get("result").cloned().ok_or_else(|| "The downloaded Codex app-server returned an incomplete reply.".into());
        }
    }
    Err("The downloaded Codex app-server exceeded the compatibility notification limit.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Temp(PathBuf);
    impl Temp {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("yorusphoto-update-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn stable_versions_and_daily_throttle_are_bounded() {
        assert_eq!(stable_version("0.163.0"), Some((0, 163, 0)));
        for value in ["", "0.163.0-alpha", "0.163", "01.2.3", "1.2.3.4", "../../1", "1.2.99999999999999999999"] {
            assert_eq!(stable_version(value), None, "{value}");
        }
        assert!(!should_check(Some(100), 101, false));
        assert!(should_check(Some(100), 100 + INTERVAL, false));
        assert!(should_check(Some(100), 101, true));
        assert!(should_check(Some(100), 99, false));
        assert_eq!(newer_version(Some("0.163.0"), Some("0.150.1")), Some("0.163.0"));
    }

    fn package() -> Value {
        json!({"name":"@openai/codex","version":"0.163.0-win32-x64","dist":{
            "tarball":"https://registry.npmjs.org/@openai/codex/-/codex-0.163.0-win32-x64.tgz",
            "integrity":format!("sha512-{}",base64::engine::general_purpose::STANDARD.encode([42u8;64])),"unpackedSize":100
        }})
    }

    #[test]
    fn metadata_rejects_foreign_urls_wrong_versions_and_missing_integrity() {
        assert!(Package::parse(&package(), "0.163.0").is_ok());
        for (pointer, value) in [
            ("/dist/tarball", json!("https://example.com/codex.tgz")),
            ("/version", json!("0.162.0-win32-x64")),
            ("/dist/integrity", json!("sha1-bad")),
            ("/dist/unpackedSize", json!(MAX_UNPACKED + 1)),
        ] {
            let mut metadata = package();
            *metadata.pointer_mut(pointer).unwrap() = value;
            assert!(Package::parse(&metadata, "0.163.0").is_err());
        }
    }

    #[test]
    fn archive_paths_cannot_escape_or_alias_windows_paths() {
        for value in [
            "../outside",
            "/outside",
            "package/../outside",
            "package/./a",
            "package//a",
            "package/a\\b",
            "C:/a",
            "package/a:evil",
            "package/NUL.txt",
            "package/COM1",
            "package/a.",
            "package/a ",
            "package/a\0",
        ] {
            assert!(validate_relative(value).is_err(), "{value}");
        }
        assert!(validate_relative("package/vendor/x86_64-pc-windows-msvc/bin/codex.exe").is_ok());
    }

    fn archive(path: &str, bytes: &[u8], link: bool) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        let mut header = tar::Header::new_gnu();
        header.set_mode(0o644);
        header.set_size(bytes.len() as u64);
        if link {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_link_name("outside").unwrap();
        }
        header.set_cksum();
        builder.append_data(&mut header, path, bytes).unwrap();
        builder.into_inner().unwrap()
    }

    #[test]
    fn extraction_rejects_links_missing_entrypoints_and_cancellation() {
        let root = Temp::new("extract");
        let cancel = AtomicBool::new(false);
        let link = archive("package/link", b"", true);
        assert!(extract_tar(link.as_slice(), &root.0, "0.163.0", &cancel).is_err());
        let plain = archive("package/readme", b"hello", false);
        assert!(extract_tar(plain.as_slice(), &root.0, "0.163.0", &cancel).is_err());
        cancel.store(true, Ordering::Relaxed);
        let error = extract_tar(plain.as_slice(), &root.0, "0.163.0", &cancel).unwrap_err();
        assert!(error.contains("cancelled"));
    }

    fn install_fixture(root: &Path, version: &str, bytes: &[u8]) -> PathBuf {
        let destination = root.join("versions").join(version);
        std::fs::create_dir_all(&destination).unwrap();
        let path = format!("package/vendor/{TARGET}/bin/codex.exe");
        let data = archive(&path, bytes, false);
        let manifest = extract_tar(data.as_slice(), &destination, version, &AtomicBool::new(false)).unwrap();
        std::fs::write(destination.join("verified.json"), serde_json::to_vec(&manifest.0).unwrap()).unwrap();
        destination.join(path)
    }

    #[test]
    fn corrupt_active_runtime_falls_back_to_verified_previous() {
        let root = Temp::new("fallback");
        let previous = install_fixture(&root.0, "0.162.0", b"previous");
        let active = install_fixture(&root.0, "0.163.0", b"active");
        let state = State { active: Some("0.163.0".into()), previous: Some("0.162.0".into()), ..State::default() };
        assert_eq!(resolve_managed(&root.0, &state).unwrap().1, active);
        std::fs::write(&active, b"broken-and-longer").unwrap();
        assert_eq!(resolve_managed(&root.0, &state).unwrap().1, previous);
        std::fs::write(&previous, b"also-broken").unwrap();
        assert!(resolve_managed(&root.0, &state).is_none());
    }

    #[test]
    fn unverified_or_unsafe_pointer_never_executes_and_state_roundtrips() {
        let root = Temp::new("state");
        let state = State { active: Some("0.163.0".into()), previous: Some("0.162.0".into()), latest: Some("0.163.0".into()), checked: Some(42) };
        state.save(&root.0).unwrap();
        assert_eq!(State::read(&root.0).active, state.active);
        assert!(resolve_managed(&root.0, &state).is_none());
        std::fs::write(root.0.join("state.json"), br#"{"active":"../../outside","previous":"0.163.0-alpha"}"#).unwrap();
        let read = State::read(&root.0);
        assert!(read.active.is_none() && read.previous.is_none());
    }

    #[test]
    fn failed_atomic_activation_retains_existing_pointer() {
        let root = Temp::new("pointer");
        let original = State { active: Some("0.162.0".into()), ..State::default() };
        original.save(&root.0).unwrap();
        let before = std::fs::read(root.0.join("state.json")).unwrap();
        // The update never touches this pointer until both integrity and probe have passed.
        let mut pending = original.clone();
        pending.active = Some("0.163.0".into());
        std::fs::write(root.0.join("blocked-parent"), b"not a directory").unwrap();
        assert!(pending.save(&root.0.join("blocked-parent")).is_err());
        assert_eq!(std::fs::read(root.0.join("state.json")).unwrap(), before);
    }

    #[test]
    fn leases_do_not_overlap_and_staging_cleanup_is_scoped() {
        let root = Temp::new("lease");
        let lease = Lease::acquire(&root.0).unwrap();
        assert!(Lease::acquire(&root.0).is_err());
        drop(lease);
        assert!(Lease::acquire(&root.0).is_ok());
        std::fs::write(root.0.join("saved-chat.json"), b"keep").unwrap();
        let stage = Stage::new(&root.0).unwrap();
        let stage_path = stage.path.clone();
        drop(stage);
        assert!(!stage_path.exists());
        assert_eq!(std::fs::read(root.0.join("saved-chat.json")).unwrap(), b"keep");
    }

    #[test]
    fn protocol_reply_errors_and_timeout_are_bounded() {
        let cancel = AtomicBool::new(false);
        let (send, receive) = mpsc::sync_channel(2);
        send.send(Some(json!({"id":2,"error":{"message":"unexpected"}}))).unwrap();
        assert!(probe_reply(&receive, 2, Instant::now() + Duration::from_secs(1), &cancel).is_err());
        assert!(receive_until(&receive, Instant::now(), &cancel).is_err());
        cancel.store(true, Ordering::Relaxed);
        assert!(receive_until(&receive, Instant::now() + Duration::from_secs(1), &cancel).is_err());
    }

    #[test]
    fn integrity_failure_rejects_changed_or_truncated_payloads() {
        let expected = Sha512::digest(b"official package");
        assert!(verify_integrity(&Sha512::digest(b"official package"), &expected).is_ok());
        assert!(verify_integrity(&Sha512::digest(b"changed package"), &expected).is_err());
        assert!(verify_integrity(&expected[..63], &expected).is_err());
    }

    #[test]
    fn ordinary_directories_work_and_available_directory_links_are_rejected() {
        let root = Temp::new("ordinary-directory");
        let nested = root.0.join("nested").join("codex-updates");
        ensure_directory(&nested).unwrap();
        assert!(!linked(&nested, &std::fs::symlink_metadata(&nested).unwrap()));
        std::fs::write(nested.join("state.json"), b"{}").unwrap();
        assert!(regular_metadata(&nested.join("state.json")).is_ok());
        let link = root.0.join("directory-link");
        #[cfg(windows)]
        let created = std::os::windows::fs::symlink_dir(&nested, &link);
        #[cfg(not(windows))]
        let created = std::os::unix::fs::symlink(&nested, &link);
        if created.is_ok() {
            assert!(linked(&link, &std::fs::symlink_metadata(&link).unwrap()));
            assert!(ensure_directory(&link.join("rejected")).is_err());
            assert!(!nested.join("rejected").exists());
        }
    }

    #[test]
    fn compatibility_schema_requires_core_methods_and_image_references() {
        let methods = [
            "initialize",
            "account/read",
            "account/rateLimits/read",
            "model/list",
            "thread/start",
            "thread/resume",
            "thread/read",
            "thread/list",
            "thread/fork",
            "thread/name/set",
            "thread/archive",
            "thread/unarchive",
            "thread/compact/start",
            "turn/start",
            "turn/interrupt",
        ];
        let mut schema = json!({"oneOf": methods.iter().map(|method| json!({"properties":{"method":{"enum":[method]}}})).collect::<Vec<_>>(),
            "definitions":{"UserInput":{"oneOf":[{"properties":{"type":{"enum":["localImage"]}}}]}}});
        assert!(validate_schema(&schema).is_ok());
        schema.pointer_mut("/oneOf").unwrap().as_array_mut().unwrap().pop();
        assert!(validate_schema(&schema).is_err());
    }
}
