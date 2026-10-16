//! Account telemetry and app-owned saved-chat operations over Codex's public protocol.

use super::*;

const MAX_MODELS: usize = 256;
const MAX_MODEL_PAGES: usize = 8;
const MODEL_REFRESH_SETTLE: Duration = Duration::from_secs(3);
const MAX_THREADS: usize = 100;
const MAX_HISTORY_BYTES: usize = 8 << 20;
const MAX_MESSAGE_BYTES: usize = 128 << 10;

pub(super) struct ModelSelection {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub requires_image: bool,
}

pub(super) struct StartedThread {
    pub id: String,
    pub model: String,
    pub effort: Option<String>,
}

pub(super) fn validate_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
        return Err("The Codex chat identifier is invalid.".into());
    }
    Ok(())
}

fn bounded(value: Option<&Value>, limit: usize) -> Option<String> {
    let text = value?.as_str()?;
    if text.len() > limit || text.chars().any(|character| character == '\0') {
        return None;
    }
    Some(text.to_owned())
}

fn shortened(text: &str, limit: usize) -> String {
    if text.len() <= limit {
        return text.to_owned();
    }
    let mut end = limit.saturating_sub('…'.len_utf8());
    while !text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    format!("{}…", text.get(..end).unwrap_or_default())
}

pub(super) fn select_model(catalog: &Value, selection: &ModelSelection) -> Result<(String, Option<String>), String> {
    let models = catalog.get("data").and_then(Value::as_array).ok_or("Codex did not return its runtime model catalog.")?;
    let accepts_image = |model: &&Value| {
        !selection.requires_image
            || model.get("inputModalities").and_then(Value::as_array).is_some_and(|values| values.iter().any(|value| value.as_str() == Some("image")))
    };
    let selected = if let Some(requested) = &selection.model {
        if requested.is_empty() || requested.len() > 256 || requested.chars().any(char::is_control) {
            return Err("The selected Codex model identifier is invalid.".into());
        }
        let model = models
            .iter()
            .take(MAX_MODELS)
            .find(|model| model.get("model").and_then(Value::as_str) == Some(requested) || model.get("id").and_then(Value::as_str) == Some(requested))
            .ok_or("The selected model is not in the current Codex runtime catalog. Refresh models and choose again.")?;
        if !accepts_image(&model) {
            return Err("The selected Codex model cannot accept image references. Choose an image-capable model.".into());
        }
        model
    } else {
        models
            .iter()
            .take(MAX_MODELS)
            .filter(accepts_image)
            .find(|model| model.get("isDefault").and_then(Value::as_bool) == Some(true))
            .or_else(|| models.iter().take(MAX_MODELS).find(accepts_image))
            .ok_or("The current Codex runtime catalog has no model supporting the requested input.")?
    };
    let model =
        bounded(selected.get("model").or_else(|| selected.get("id")), 256).filter(|id| !id.is_empty()).ok_or("Codex returned an invalid model identifier.")?;
    let efforts = selected.get("supportedReasoningEfforts").and_then(Value::as_array);
    let effort = if let Some(effort) = &selection.effort {
        if effort.is_empty()
            || effort.len() > 64
            || effort.chars().any(char::is_control)
            || !efforts.is_some_and(|choices| choices.iter().any(|choice| choice.get("reasoningEffort").and_then(Value::as_str) == Some(effort)))
        {
            return Err("The selected reasoning effort is unavailable for this Codex model. Choose a supported effort.".into());
        }
        Some(effort.clone())
    } else {
        bounded(selected.get("defaultReasoningEffort"), 64)
    };
    Ok((model, effort))
}

pub(super) fn queued_notification(method: &str) -> bool {
    method.starts_with("item/")
        || matches!(
            method,
            "turn/started"
                | "turn/completed"
                | "account/login/completed"
                | "error"
                | "thread/tokenUsage/updated"
                | "account/rateLimits/updated"
                | "thread/compacted"
                | "thread/name/updated"
                | "model/rerouted"
        )
}

fn usage_breakdown(value: &Value) -> Value {
    let mut output = serde_json::Map::new();
    for key in ["totalTokens", "inputTokens", "cachedInputTokens", "cacheWriteInputTokens", "outputTokens", "reasoningOutputTokens"] {
        if let Some(number) = value.get(key).and_then(Value::as_u64) {
            output.insert(key.into(), json!(number));
        }
    }
    Value::Object(output)
}

fn preserve_nulls(source: &Value, target: &mut Value, keys: &[&str]) {
    for key in keys {
        if source.get(*key).is_some_and(Value::is_null) {
            target[*key] = Value::Null;
        }
    }
}

fn rate_window(value: &Value) -> Value {
    let mut result = json!({});
    if let Some(percent) = value.get("usedPercent").and_then(Value::as_f64).filter(|number| number.is_finite() && *number >= 0.0) {
        result["usedPercent"] = json!(percent.min(100.0));
    }
    for key in ["windowDurationMins", "resetsAt"] {
        if let Some(number) = value.get(key).and_then(Value::as_u64) {
            result[key] = json!(number);
        }
    }
    preserve_nulls(value, &mut result, &["usedPercent", "windowDurationMins", "resetsAt"]);
    result
}

fn rate_snapshot(value: &Value) -> Value {
    let mut result = json!({});
    for key in ["limitId", "limitName", "normalModelSlug", "planType", "rateLimitReachedType"] {
        if let Some(text) = bounded(value.get(key), 256) {
            result[key] = json!(text);
        }
    }
    for key in ["primary", "secondary"] {
        if let Some(window) = value.get(key).filter(|window| window.is_object()) {
            result[key] = rate_window(window);
        }
    }
    if let Some(credits) = value.get("credits").filter(|credits| credits.is_object()) {
        let mut safe = json!({});
        for key in ["hasCredits", "unlimited"] {
            if let Some(flag) = credits.get(key).and_then(Value::as_bool) {
                safe[key] = json!(flag);
            }
        }
        if let Some(balance) = bounded(credits.get("balance"), 128) {
            safe["balance"] = json!(balance);
        }
        preserve_nulls(credits, &mut safe, &["hasCredits", "unlimited", "balance"]);
        result["credits"] = safe;
    }
    if let Some(limit) = value.get("individualLimit").filter(|value| value.is_object()) {
        let mut safe = json!({});
        for key in ["limit", "used"] {
            if let Some(text) = bounded(limit.get(key), 128) {
                safe[key] = json!(text);
            }
        }
        for key in ["remainingPercent", "resetsAt"] {
            if let Some(number) = limit.get(key).and_then(Value::as_u64) {
                safe[key] = json!(number);
            }
        }
        preserve_nulls(limit, &mut safe, &["limit", "used", "remainingPercent", "resetsAt"]);
        result["individualLimit"] = safe;
    }
    if let Some(reached) = value.get("spendControlReached").and_then(Value::as_bool) {
        result["spendControlReached"] = json!(reached);
    } else if value.get("spendControlReached").is_some_and(Value::is_null) {
        // This field is explicitly not a sparse recovery marker in Codex's protocol.
        result["spendControlReached"] = Value::Null;
    }
    preserve_nulls(
        value,
        &mut result,
        &["limitId", "limitName", "normalModelSlug", "planType", "rateLimitReachedType", "primary", "secondary", "credits", "individualLimit"],
    );
    result
}

fn rate_response(value: &Value) -> Value {
    let mut result = json!({});
    if let Some(rate) = value.get("rateLimits").filter(|rate| rate.is_object()) {
        result["rateLimits"] = rate_snapshot(rate);
    }
    if let Some(rates) = value.get("rateLimitsByLimitId").and_then(Value::as_object) {
        let mut safe = serde_json::Map::new();
        for (id, rate) in rates.iter().take(64) {
            if id.len() <= 256 && rate.is_object() {
                safe.insert(id.clone(), rate_snapshot(rate));
            }
        }
        if !safe.is_empty() {
            result["rateLimitsByLimitId"] = Value::Object(safe);
        }
    }
    if let Some(allowed) = value.get("ordinaryUsageAllowed").and_then(Value::as_bool) {
        result["ordinaryUsageAllowed"] = json!(allowed);
    } else if value.get("ordinaryUsageAllowed").is_some_and(Value::is_null) {
        result["ordinaryUsageAllowed"] = Value::Null;
    }
    if let Some(count) = value.pointer("/rateLimitResetCredits/availableCount").and_then(Value::as_u64) {
        result["rateLimitResetCredits"] = json!({"availableCount":count});
    } else if value.get("rateLimitResetCredits").is_some_and(Value::is_null) {
        result["rateLimitResetCredits"] = Value::Null;
    } else if value.pointer("/rateLimitResetCredits/availableCount").is_some_and(Value::is_null) {
        result["rateLimitResetCredits"] = json!({"availableCount":null});
    }
    result
}

pub(super) fn telemetry(method: &str, params: &Value, thread_id: Option<&str>) -> Option<(String, Value)> {
    if method == "account/rateLimits/updated" {
        return Some(("rateLimits".into(), rate_response(params)));
    }
    let id = bounded(params.get("threadId"), 256)?;
    if thread_id.is_some_and(|expected| expected != id) {
        return None;
    }
    match method {
        "thread/tokenUsage/updated" => {
            let usage = params.get("tokenUsage")?;
            let mut safe = json!({"threadId":id,"tokenUsage":{}});
            if let Some(turn_id) = bounded(params.get("turnId"), 256) {
                safe["turnId"] = json!(turn_id);
            }
            for key in ["total", "last"] {
                if let Some(value) = usage.get(key).filter(|value| value.is_object()) {
                    safe["tokenUsage"][key] = usage_breakdown(value);
                }
            }
            if let Some(window) = usage.get("modelContextWindow").and_then(Value::as_u64).filter(|window| *window > 0) {
                safe["tokenUsage"]["modelContextWindow"] = json!(window);
            }
            Some(("tokenUsage".into(), safe))
        }
        "item/started" | "item/completed" if params.pointer("/item/type").and_then(Value::as_str) == Some("contextCompaction") => Some((
            "compaction".into(),
            json!({"state":if method == "item/started" {"running"} else {"complete"},"threadId":id,"turnId":params.get("turnId").and_then(Value::as_str)}),
        )),
        "thread/compacted" => Some(("compaction".into(), json!({"state":"complete","threadId":id}))),
        "model/rerouted" => {
            let model = bounded(params.get("toModel").or_else(|| params.get("model")), 256)?;
            Some(("model".into(), json!({"model":model})))
        }
        _ => None,
    }
}

trait Connection {
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String>;
    fn next(&mut self, deadline: Instant) -> Result<Value, String>;
    fn reject(&mut self, message: &Value) -> Result<(), String>;
    fn interrupt(&mut self, thread_id: &str, turn_id: &str);
    fn settle_catalog(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn cancelled(&self) -> bool {
        false
    }
}

impl Connection for AppServer {
    fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
        AppServer::rpc(self, method, params)
    }
    fn next(&mut self, deadline: Instant) -> Result<Value, String> {
        if let Some(message) = self.pending.pop_front() {
            self.pending_bytes = self.pending_bytes.saturating_sub(message.to_string().len());
            Ok(message)
        } else {
            self.receive(deadline, true)
        }
    }
    fn reject(&mut self, message: &Value) -> Result<(), String> {
        self.reject_other_request(message)
    }
    fn interrupt(&mut self, thread_id: &str, turn_id: &str) {
        self.interrupt_turn(thread_id, turn_id);
    }
    fn settle_catalog(&mut self) -> Result<(), String> {
        // Recent public app-server releases start their own Online catalog refresh on
        // startup. Keep this process alive briefly, then re-query its public model/list;
        // an immediate one-shot exit can otherwise win the race with that refresh.
        // There is no force-refresh/source-completion field in model/list's schema.
        let deadline = self.spawned_at + MODEL_REFRESH_SETTLE;
        while Instant::now() < deadline {
            if self.cancel.as_ref().is_some_and(|cancel| cancel.load(Ordering::Relaxed)) {
                return Err("Cancelled.".into());
            }
            std::thread::sleep(deadline.saturating_duration_since(Instant::now()).min(Duration::from_millis(50)));
        }
        Ok(())
    }
    fn cancelled(&self) -> bool {
        self.cancel.as_ref().is_some_and(|cancel| cancel.load(Ordering::Relaxed))
    }
}

pub(super) fn model_catalog(server: &mut AppServer, recheck: bool) -> Result<Value, String> {
    discover_models(server, recheck)
}

fn collect_model_pages(server: &mut impl Connection) -> Result<Value, String> {
    let mut models = Vec::new();
    let mut seen_ids = std::collections::HashSet::new();
    let mut seen_cursors = std::collections::HashSet::new();
    let mut cursor: Option<String> = None;
    for page in 1..=MAX_MODEL_PAGES {
        let response = server.rpc("model/list", json!({"limit":100,"includeHidden":true,"cursor":cursor}))?;
        let data = response.get("data").and_then(Value::as_array).ok_or("Codex returned an invalid model catalog page.")?;
        if data.len() > MAX_MODELS || models.len().saturating_add(data.len()) > MAX_MODELS {
            return Err("The Codex model catalog exceeds the supported 256-model limit. Existing choices were kept.".into());
        }
        for model in data {
            let id = bounded(model.get("id"), 256)
                .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
                .ok_or("Codex returned an invalid model catalog identifier.")?;
            let slug = bounded(model.get("model"), 256)
                .filter(|id| !id.is_empty() && !id.chars().any(char::is_control))
                .ok_or("Codex returned an invalid model catalog name.")?;
            if seen_ids.insert(id) {
                let mut model = model.clone();
                model["model"] = json!(slug);
                models.push(model);
            }
        }
        match response.get("nextCursor") {
            None | Some(Value::Null) => {
                let mut result = model_response(&json!({"data":models}));
                result["catalogInfo"] =
                    json!({"source":"runtime_reported","freshness":"not_reported","pageCount":page,"complete":true,"includesHidden":true,"followupQueries":0});
                return Ok(result);
            }
            Some(Value::String(next)) if !next.is_empty() && next.len() <= 4096 && !next.chars().any(char::is_control) => {
                if !seen_cursors.insert(next.clone()) {
                    return Err("Codex repeated a model catalog page cursor. Existing choices were kept.".into());
                }
                cursor = Some(next.clone());
            }
            _ => return Err("Codex returned an invalid model catalog page cursor.".into()),
        }
    }
    Err("The Codex model catalog exceeded eight pages. Existing choices were kept.".into())
}

fn discover_models(server: &mut impl Connection, recheck: bool) -> Result<Value, String> {
    let initial = collect_model_pages(server)?;
    if !recheck {
        return Ok(initial);
    }
    server.settle_catalog()?;
    match collect_model_pages(server) {
        Ok(mut refreshed) => {
            refreshed["catalogInfo"]["followupQueries"] = json!(1);
            refreshed["catalogInfo"]["followupSucceeded"] = json!(true);
            Ok(refreshed)
        }
        Err(_) if server.cancelled() => Err("Cancelled.".into()),
        Err(_) => {
            let mut initial = initial;
            initial["catalogInfo"]["followupQueries"] = json!(1);
            initial["catalogInfo"]["followupSucceeded"] = json!(false);
            Ok(initial)
        }
    }
}

fn owned_thread(thread: &Value, directory: &Path) -> bool {
    // Codex persists the originating initialize.clientInfo.name. It survives moving a
    // portable app and avoids treating an old absolute workspace path as lost history.
    if thread.get("originator").and_then(Value::as_str) == Some("yorusphoto") {
        return true;
    }
    let Some(cwd) = thread.get("cwd").and_then(Value::as_str) else { return false };
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return false;
    }
    let normalized = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let directory = fs::canonicalize(directory).unwrap_or_else(|_| directory.to_path_buf());
    if normalized == directory {
        return true;
    }
    let Some(parent) = directory.parent() else { return false };
    let legacy = parent.join("codex-references");
    let legacy = fs::canonicalize(&legacy).unwrap_or(legacy);
    if normalized.starts_with(legacy) {
        return true;
    }
    // Codex 0.150 does not return originator metadata. Recognize the two directory
    // layouts this bridge created before a portable folder was moved or upgraded.
    let Some(name) = normalized.file_name().and_then(|name| name.to_str()) else { return false };
    let Some(parent_name) = normalized.parent().and_then(Path::file_name).and_then(|name| name.to_str()) else { return false };
    (name.eq_ignore_ascii_case("codex-workspace") && ["data", "yorusphotoData", "yorusphoto"].iter().any(|parent| parent_name.eq_ignore_ascii_case(parent)))
        || (name.starts_with("yorusphoto-codex-") && parent_name.eq_ignore_ascii_case("codex-references"))
}

fn read_owned(server: &mut impl Connection, id: &str, directory: &Path, include_turns: bool) -> Result<Value, String> {
    validate_id(id)?;
    // Validate metadata before requesting any transcript from the account's other clients.
    let result = server.rpc("thread/read", json!({"threadId":id,"includeTurns":false}))?;
    let thread = result.get("thread").ok_or("Codex did not return the saved chat.")?;
    if thread.get("id").and_then(Value::as_str) != Some(id) || !owned_thread(thread, directory) {
        return Err("This saved chat belongs to another Codex workspace. Open a yorusphoto chat instead.".into());
    }
    if !include_turns {
        return Ok(thread.clone());
    }
    let history = server.rpc("thread/read", json!({"threadId":id,"includeTurns":true}))?;
    let thread = history.get("thread").ok_or("Codex did not return the saved chat.")?;
    if thread.get("id").and_then(Value::as_str) != Some(id) || !owned_thread(thread, directory) {
        return Err("The saved chat changed workspace while it was loading. Refresh the chat list.".into());
    }
    Ok(thread.clone())
}

pub(super) fn verify_thread(server: &mut AppServer, id: &str, directory: &Path) -> Result<(), String> {
    read_owned(server, id, directory, false).map(|_| ())
}

fn safe_thread(thread: &Value, history: bool) -> Result<Value, String> {
    let id = bounded(thread.get("id"), 256).ok_or("Codex returned an invalid saved-chat identifier.")?;
    validate_id(&id)?;
    let mut result = json!({"id":id});
    for key in ["name", "model", "reasoningEffort"] {
        if let Some(value) = bounded(thread.get(key), 1024) {
            result[key] = json!(value);
        }
    }
    if let Some(preview) = thread.get("preview").and_then(Value::as_str) {
        result["preview"] = json!(shortened(preview, 4096));
    }
    for key in ["createdAt", "updatedAt"] {
        if let Some(value) = thread.get(key).and_then(Value::as_u64) {
            result[key] = json!(value);
        }
    }
    if !history {
        return Ok(result);
    }
    let mut turns = VecDeque::new();
    let mut total_bytes: usize = 0;
    let mut truncated = false;
    if let Some(source) = thread.get("turns").and_then(Value::as_array) {
        for turn in source.iter().rev().take(100) {
            let mut items = VecDeque::new();
            if let Some(source_items) = turn.get("items").and_then(Value::as_array) {
                for item in source_items.iter().rev().take(256) {
                    let item = match item.get("type").and_then(Value::as_str) {
                        Some("userMessage") => {
                            let mut content = Vec::new();
                            if let Some(input) = item.get("content").and_then(Value::as_array) {
                                for value in input.iter().take(128) {
                                    if value.get("type").and_then(Value::as_str) == Some("text")
                                        && let Some(text) = value.get("text").and_then(Value::as_str)
                                    {
                                        truncated |= text.len() > MAX_MESSAGE_BYTES;
                                        content.push(json!({"type":"text","text":shortened(text, MAX_MESSAGE_BYTES)}));
                                    }
                                }
                            }
                            json!({"type":"userMessage","content":content})
                        }
                        Some("agentMessage") => {
                            let text = item.get("text").and_then(Value::as_str).unwrap_or_default();
                            truncated |= text.len() > MAX_MESSAGE_BYTES;
                            json!({"type":"agentMessage","text":shortened(text, MAX_MESSAGE_BYTES)})
                        }
                        Some("contextCompaction") => json!({"type":"contextCompaction"}),
                        _ => continue,
                    };
                    total_bytes = total_bytes.saturating_add(item.to_string().len());
                    if total_bytes > MAX_HISTORY_BYTES {
                        truncated = true;
                        break;
                    }
                    items.push_front(item);
                }
                truncated |= source_items.len() > 256;
            }
            turns.push_front(json!({"id":bounded(turn.get("id"),256),"status":bounded(turn.get("status"),32),"items":items}));
            if total_bytes > MAX_HISTORY_BYTES {
                break;
            }
        }
        truncated |= source.len() > 100;
    }
    result["turns"] = json!(turns);
    result["historyTruncated"] = json!(truncated);
    Ok(result)
}

fn model_response(value: &Value) -> Value {
    let mut models = Vec::new();
    if let Some(data) = value.get("data").and_then(Value::as_array) {
        for model in data.iter().take(MAX_MODELS) {
            let Some(id) = bounded(model.get("id"), 256) else { continue };
            let Some(slug) = bounded(model.get("model"), 256) else { continue };
            let mut safe = json!({"id":id,"model":slug});
            for (key, limit) in [("displayName", 256), ("description", 2048), ("defaultReasoningEffort", 64)] {
                if let Some(value) = bounded(model.get(key), limit) {
                    safe[key] = json!(value);
                }
            }
            for key in ["isDefault", "hidden"] {
                if let Some(value) = model.get(key).and_then(Value::as_bool) {
                    safe[key] = json!(value);
                }
            }
            if let Some(modalities) = model.get("inputModalities").and_then(Value::as_array) {
                let modalities: Vec<_> = modalities.iter().take(8).filter_map(|value| bounded(Some(value), 32)).collect();
                safe["inputModalities"] = json!(modalities);
            }
            let mut efforts = Vec::new();
            if let Some(options) = model.get("supportedReasoningEfforts").and_then(Value::as_array) {
                for option in options.iter().take(16) {
                    if let Some(effort) = bounded(option.get("reasoningEffort"), 64) {
                        efforts.push(json!({"reasoningEffort":effort,"description":bounded(option.get("description"),1024)}));
                    }
                }
            }
            safe["supportedReasoningEfforts"] = json!(efforts);
            models.push(safe);
        }
    }
    json!({"data":models,"nextCursor":bounded(value.get("nextCursor"),4096)})
}

pub(super) fn perform(
    server: &mut AppServer,
    request: &CodexWorkspaceRequest,
    directory: &Path,
    events: &mpsc::Sender<CodexWorkspaceEvent>,
) -> Result<Value, String> {
    perform_inner(server, request, directory, events)
}

fn perform_inner(
    server: &mut impl Connection,
    request: &CodexWorkspaceRequest,
    directory: &Path,
    events: &mpsc::Sender<CodexWorkspaceEvent>,
) -> Result<Value, String> {
    if request.cancel.load(Ordering::Relaxed) {
        return Err("Cancelled.".into());
    }
    match &request.action {
        CodexWorkspaceAction::Refresh => {
            let mut result = json!({"errors":[]});
            let mut errors = Vec::new();
            match server.rpc("account/read", json!({"refreshToken":false})) {
                Ok(account) => {
                    let account = account.get("account").unwrap_or(&Value::Null);
                    result["account"] = json!({"type":bounded(account.get("type"),32),"planType":bounded(account.get("planType"),64)});
                }
                Err(_) => errors.push("Could not refresh the Codex account status."),
            }
            match discover_models(server, true) {
                Ok(models) => result["models"] = models,
                Err(_) => errors.push("Could not refresh the Codex model catalog."),
            }
            match server.rpc("account/rateLimits/read", json!({})) {
                Ok(rates) => result["rateLimits"] = rate_response(&rates),
                Err(_) => errors.push("Codex usage is temporarily unavailable. Refresh again after signing in."),
            }
            result["errors"] = json!(errors);
            Ok(result)
        }
        CodexWorkspaceAction::ListThreads { cursor, archived, search } => {
            if cursor.as_ref().is_some_and(|text| text.len() > 4096) || search.as_ref().is_some_and(|text| text.len() > 256) {
                return Err("The saved-chat search or page cursor is too long.".into());
            }
            let result = server.rpc(
                "thread/list",
                json!({"cursor":cursor,"limit":100,"sortKey":"updated_at","archived":archived,"searchTerm":search,"modelProviders":["openai"]}),
            )?;
            let data = result.get("data").and_then(Value::as_array).ok_or("Codex did not return its saved chats.")?;
            let threads = data
                .iter()
                .take(MAX_THREADS)
                .filter(|thread| owned_thread(thread, directory))
                .map(|thread| safe_thread(thread, false))
                .collect::<Result<Vec<_>, _>>()?;
            Ok(json!({"data":threads,"nextCursor":bounded(result.get("nextCursor"),4096),"archived":archived}))
        }
        CodexWorkspaceAction::ReadThread { thread_id } => {
            let thread = read_owned(server, thread_id, directory, true)?;
            Ok(json!({"thread":safe_thread(&thread,true)?}))
        }
        CodexWorkspaceAction::RenameThread { thread_id, name } => {
            if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
                return Err("Enter a chat name of at most 256 bytes without control characters.".into());
            }
            read_owned(server, thread_id, directory, false)?;
            server.rpc("thread/name/set", json!({"threadId":thread_id,"name":name.trim()}))?;
            Ok(json!({"threadId":thread_id,"name":name.trim()}))
        }
        CodexWorkspaceAction::ArchiveThread { thread_id, archived } => {
            read_owned(server, thread_id, directory, false)?;
            server.rpc(if *archived { "thread/archive" } else { "thread/unarchive" }, json!({"threadId":thread_id}))?;
            Ok(json!({"threadId":thread_id,"archived":archived}))
        }
        CodexWorkspaceAction::ForkThread { thread_id } => {
            read_owned(server, thread_id, directory, false)?;
            let mut params = json!({"threadId":thread_id,"cwd":directory,"approvalPolicy":"never","sandbox":"read-only","modelProvider":"openai","ephemeral":false,"excludeTurns":false,"developerInstructions":CHAT_INSTRUCTIONS});
            let catalog = discover_models(server, false)?;
            let (model, effort) =
                select_model(&catalog, &ModelSelection { model: request.model.clone(), effort: request.reasoning_effort.clone(), requires_image: false })?;
            params["model"] = json!(model);
            if let Some(effort) = &effort {
                params["config"] = json!({"model_reasoning_effort":effort});
            }
            let result = server.rpc("thread/fork", params)?;
            let thread = result.get("thread").ok_or("Codex did not return the forked chat.")?;
            if !owned_thread(thread, directory) {
                return Err("Codex returned a fork outside the yorusphoto workspace.".into());
            }
            Ok(json!({"thread":safe_thread(thread,true)?,"model":model,"reasoningEffort":effort}))
        }
        CodexWorkspaceAction::CompactThread { thread_id } => {
            read_owned(server, thread_id, directory, false)?;
            let catalog = discover_models(server, false)?;
            let (model, effort) =
                select_model(&catalog, &ModelSelection { model: request.model.clone(), effort: request.reasoning_effort.clone(), requires_image: false })?;
            let mut params = json!({"threadId":thread_id,"cwd":directory,"approvalPolicy":"never","sandbox":"read-only","modelProvider":"openai","excludeTurns":true,"model":model,"developerInstructions":CHAT_INSTRUCTIONS});
            if let Some(effort) = effort {
                params["config"] = json!({"model_reasoning_effort":effort});
            }
            server.rpc("thread/resume", params)?;
            server.rpc("thread/compact/start", json!({"threadId":thread_id}))?;
            let _ = events.send(CodexWorkspaceEvent::Telemetry { kind: "compaction".into(), data: json!({"state":"running","threadId":thread_id}) });
            match await_compaction(server, thread_id, &request.cancel, events) {
                Ok(()) => Ok(json!({"threadId":thread_id,"compacted":true})),
                Err(error) => {
                    let _ = events.send(CodexWorkspaceEvent::Telemetry { kind: "compaction".into(), data: json!({"state":"failed","threadId":thread_id}) });
                    Err(error)
                }
            }
        }
    }
}

fn await_compaction(server: &mut impl Connection, id: &str, cancel: &AtomicBool, events: &mpsc::Sender<CodexWorkspaceEvent>) -> Result<(), String> {
    let deadline = Instant::now() + TURN_TIMEOUT;
    let mut turn_id: Option<String> = None;
    let mut complete = false;
    loop {
        if cancel.load(Ordering::Relaxed) || Instant::now() >= deadline {
            if let Some(turn_id) = &turn_id {
                server.interrupt(id, turn_id);
            }
            return Err(if cancel.load(Ordering::Relaxed) { "Compaction cancelled." } else { "Codex compaction timed out." }.into());
        }
        let message = match server.next(deadline) {
            Ok(message) => message,
            Err(error) => {
                if let Some(turn_id) = &turn_id {
                    server.interrupt(id, turn_id);
                }
                return Err(error);
            }
        };
        let method = message.get("method").and_then(Value::as_str).unwrap_or_default();
        let params = message.get("params").unwrap_or(&Value::Null);
        if params.get("threadId").and_then(Value::as_str) == Some(id) {
            if let Some(value) = bounded(params.get("turnId").or_else(|| params.pointer("/turn/id")), 256) {
                turn_id = Some(value);
            }
            if let Some((kind, data)) = telemetry(method, params, Some(id)) {
                if kind == "compaction" && data.get("state").and_then(Value::as_str) == Some("complete") {
                    complete = true;
                }
                let _ = events.send(CodexWorkspaceEvent::Telemetry { kind, data });
            }
            if method == "turn/completed" {
                return match params.pointer("/turn/status").and_then(Value::as_str) {
                    Some("completed") if complete => Ok(()),
                    Some("interrupted") => Err("Compaction cancelled.".into()),
                    _ => Err("Codex could not complete compaction. The saved chat is still available.".into()),
                };
            }
        }
        if method == "account/rateLimits/updated"
            && let Some((kind, data)) = telemetry(method, params, None)
        {
            let _ = events.send(CodexWorkspaceEvent::Telemetry { kind, data });
        }
        if method == "error" && params.get("willRetry").and_then(Value::as_bool) != Some(true) {
            return Err("Codex could not complete compaction. Check the account usage or try again.".into());
        }
        server.reject(&message)?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct Mock {
        replies: VecDeque<Result<Value, String>>,
        notifications: VecDeque<Result<Value, String>>,
        calls: Vec<(String, Value)>,
        interruptions: Vec<(String, String)>,
        cancel_after_receive: Option<Arc<AtomicBool>>,
        catalog_settles: usize,
        cancel_catalog: bool,
    }

    impl Connection for Mock {
        fn rpc(&mut self, method: &str, params: Value) -> Result<Value, String> {
            self.calls.push((method.into(), params));
            self.replies.pop_front().expect("mock response")
        }
        fn next(&mut self, _deadline: Instant) -> Result<Value, String> {
            if self.notifications.len() == 1
                && let Some(cancel) = &self.cancel_after_receive
            {
                cancel.store(true, Ordering::Relaxed);
            }
            self.notifications.pop_front().expect("mock event")
        }
        fn reject(&mut self, _message: &Value) -> Result<(), String> {
            Ok(())
        }
        fn interrupt(&mut self, thread: &str, turn: &str) {
            self.interruptions.push((thread.into(), turn.into()));
        }
        fn settle_catalog(&mut self) -> Result<(), String> {
            self.catalog_settles += 1;
            if self.cancel_catalog { Err("Cancelled.".into()) } else { Ok(()) }
        }
        fn cancelled(&self) -> bool {
            self.cancel_catalog
        }
    }

    fn catalog() -> Value {
        json!({"data":[
            {"id":"text","model":"text-model","isDefault":true,"inputModalities":["text"],"defaultReasoningEffort":"low","supportedReasoningEfforts":[{"reasoningEffort":"low"}]},
            {"id":"vision","model":"vision-model","inputModalities":["text","image"],"defaultReasoningEffort":"medium","supportedReasoningEfforts":[{"reasoningEffort":"medium"},{"reasoningEffort":"high"}]}
        ]})
    }

    fn request(action: CodexWorkspaceAction) -> CodexWorkspaceRequest {
        CodexWorkspaceRequest { codex_path: String::new(), model: None, reasoning_effort: None, cancel: Arc::new(AtomicBool::new(false)), action }
    }

    fn fixture_thread(directory: &Path) -> Value {
        json!({"id":"fixture-thread","cwd":directory,"name":"Saved chat","preview":"A synthetic test chat","path":"private-file","accountId":"private-account","turns":[]})
    }

    #[test]
    fn model_selection_respects_catalog_modalities_and_supported_effort() {
        let selection = ModelSelection { model: None, effort: None, requires_image: true };
        assert_eq!(select_model(&catalog(), &selection).expect("vision default"), ("vision-model".into(), Some("medium".into())));
        let selection = ModelSelection { model: Some("vision".into()), effort: Some("high".into()), requires_image: true };
        assert_eq!(select_model(&catalog(), &selection).expect("requested model"), ("vision-model".into(), Some("high".into())));
        assert!(select_model(&catalog(), &ModelSelection { model: Some("text".into()), effort: None, requires_image: true }).is_err());
        assert!(select_model(&catalog(), &ModelSelection { model: Some("vision".into()), effort: Some("ultra".into()), requires_image: false }).is_err());
        assert!(select_model(&catalog(), &ModelSelection { model: Some("retired-model".into()), effort: None, requires_image: false }).is_err());
        let mut future_catalog = catalog();
        future_catalog["data"][1]["supportedReasoningEfforts"] = json!([{"reasoningEffort":"future-effort"}]);
        assert!(
            select_model(&future_catalog, &ModelSelection { model: Some("vision".into()), effort: Some("future-effort".into()), requires_image: false })
                .is_ok()
        );
        assert!(model_response(&json!({"data":[{"id":"unknown-input","model":"unknown-input"}]})).pointer("/data/0/inputModalities").is_none());
    }

    #[test]
    fn model_discovery_collects_all_pages_and_includes_hidden_models() {
        let mut first = catalog();
        first["nextCursor"] = json!("page-two");
        let mut server = Mock {
            replies: VecDeque::from([
                Ok(first),
                Ok(json!({"data":[
                    {"id":"vision","model":"vision-model","hidden":false},
                    {"id":"expert","model":"expert-model","hidden":true,"private":"secret"}
                ],"nextCursor":null})),
            ]),
            ..Default::default()
        };
        let result = discover_models(&mut server, false).expect("complete catalog");
        assert_eq!(result["data"].as_array().expect("models").len(), 3);
        assert_eq!(result.pointer("/data/2/hidden"), Some(&json!(true)));
        assert_eq!(result.pointer("/catalogInfo/pageCount"), Some(&json!(2)));
        assert_eq!(result.pointer("/catalogInfo/freshness"), Some(&json!("not_reported")));
        assert_eq!(result["nextCursor"], Value::Null);
        assert!(!result.to_string().contains("secret"));
        assert_eq!(server.catalog_settles, 0);
        assert_eq!(server.calls.len(), 2);
        assert!(server.calls.iter().all(|(method, params)| method == "model/list" && params["includeHidden"] == true && params["limit"] == 100));
        assert_eq!(server.calls[0].1["cursor"], Value::Null);
        assert_eq!(server.calls[1].1["cursor"], "page-two");
    }

    #[test]
    fn model_discovery_requeries_same_process_without_claiming_network_freshness() {
        let mut server = Mock {
            replies: VecDeque::from([Ok(catalog()), Ok(json!({"data":[{"id":"latest","model":"latest-model","isDefault":true}]}))]),
            ..Default::default()
        };
        let result = discover_models(&mut server, true).expect("rechecked catalog");
        assert_eq!(result.pointer("/data/0/model"), Some(&json!("latest-model")));
        assert_eq!(result.pointer("/catalogInfo/followupSucceeded"), Some(&json!(true)));
        assert_eq!(result.pointer("/catalogInfo/followupQueries"), Some(&json!(1)));
        assert_eq!(result.pointer("/catalogInfo/freshness"), Some(&json!("not_reported")));
        assert_eq!(server.catalog_settles, 1);
    }

    #[test]
    fn failed_catalog_followup_keeps_initial_complete_catalog_but_cancellation_wins() {
        let mut server = Mock { replies: VecDeque::from([Ok(catalog()), Err("private provider details".into())]), ..Default::default() };
        let result = discover_models(&mut server, true).expect("first complete catalog retained");
        assert_eq!(result.pointer("/data/0/model"), Some(&json!("text-model")));
        assert_eq!(result.pointer("/catalogInfo/followupSucceeded"), Some(&json!(false)));
        assert!(!result.to_string().contains("private"));
        let mut cancelled = Mock { replies: VecDeque::from([Ok(catalog())]), cancel_catalog: true, ..Default::default() };
        assert_eq!(discover_models(&mut cancelled, true).expect_err("cancelled"), "Cancelled.");
        assert_eq!(cancelled.calls.len(), 1);
    }

    #[test]
    fn catalog_pagination_rejects_cycles_invalid_cursors_and_unbounded_results() {
        let repeated = json!({"data":[],"nextCursor":"again"});
        let mut cyclic = Mock { replies: VecDeque::from([Ok(repeated.clone()), Ok(repeated)]), ..Default::default() };
        assert!(discover_models(&mut cyclic, false).expect_err("cycle").contains("repeated"));
        for cursor in [json!(""), json!("x".repeat(4097)), json!(42)] {
            let mut invalid = Mock { replies: VecDeque::from([Ok(json!({"data":[],"nextCursor":cursor}))]), ..Default::default() };
            assert!(discover_models(&mut invalid, false).is_err());
        }
        let mut excessive_pages =
            Mock { replies: (0..MAX_MODEL_PAGES).map(|page| Ok(json!({"data":[],"nextCursor":format!("page-{page}")}))).collect(), ..Default::default() };
        assert!(discover_models(&mut excessive_pages, false).expect_err("page bound").contains("eight pages"));
        assert_eq!(excessive_pages.calls.len(), MAX_MODEL_PAGES);
        let oversized: Vec<_> = (0..=MAX_MODELS).map(|index| json!({"id":format!("model-{index}"),"model":format!("model-{index}")})).collect();
        let mut excessive_models = Mock { replies: VecDeque::from([Ok(json!({"data":oversized}))]), ..Default::default() };
        assert!(discover_models(&mut excessive_models, false).expect_err("model bound").contains("256-model"));
    }

    #[test]
    fn telemetry_is_thread_scoped_preserves_unknowns_and_strips_extra_fields() {
        let params = json!({"threadId":"fixture-thread","turnId":"fixture-turn","tokenUsage":{
            "last":{"inputTokens":120,"totalTokens":160,"outputTokens":40,"cachedInputTokens":80,"private":"secret"},
            "total":{"inputTokens":500,"totalTokens":800},"modelContextWindow":200000,"accountId":"private"}});
        assert!(telemetry("thread/tokenUsage/updated", &params, Some("another-thread")).is_none());
        let (kind, safe) = telemetry("thread/tokenUsage/updated", &params, Some("fixture-thread")).expect("usage");
        assert_eq!(kind, "tokenUsage");
        assert_eq!(safe.pointer("/tokenUsage/modelContextWindow"), Some(&json!(200000)));
        assert_eq!(safe.pointer("/tokenUsage/last/cachedInputTokens"), Some(&json!(80)));
        assert!(!safe.to_string().contains("private"));
        let empty = telemetry(
            "thread/tokenUsage/updated",
            &json!({"threadId":"fixture-thread","tokenUsage":{"last":{"inputTokens":-1},"modelContextWindow":null}}),
            None,
        )
        .expect("unknown usage")
        .1;
        assert!(empty.pointer("/tokenUsage/last/inputTokens").is_none());
        assert!(empty.pointer("/tokenUsage/modelContextWindow").is_none());
        for method in ["thread/tokenUsage/updated", "account/rateLimits/updated", "turn/started"] {
            assert!(queued_notification(method));
        }
    }

    #[test]
    fn rate_limit_snapshots_preserve_nulls_and_omit_missing_fields() {
        let full = rate_response(&json!({"accountId":"secret-account","ordinaryUsageAllowed":false,
            "rateLimits":{"limitId":"codex","primary":{"usedPercent":98,"windowDurationMins":300,"resetsAt":2000000000},"secondary":null,"credits":{"hasCredits":true,"unlimited":false,"balance":"12.5"}},
            "rateLimitsByLimitId":{"image":{"primary":{"usedPercent":60},"planType":"pro"}},"rateLimitResetCredits":{"availableCount":2,"details":["private"]}}));
        assert_eq!(full.pointer("/rateLimits/primary/resetsAt"), Some(&json!(2000000000)));
        assert_eq!(full.pointer("/rateLimits/credits/balance"), Some(&json!("12.5")));
        assert_eq!(full.get("ordinaryUsageAllowed"), Some(&json!(false)));
        assert_eq!(full.pointer("/rateLimits/secondary"), Some(&Value::Null));
        assert!(!full.to_string().contains("private"));
        assert!(full.get("accountId").is_none());
        assert_eq!(rate_response(&json!({"ordinaryUsageAllowed":null})), json!({"ordinaryUsageAllowed":null}));
        assert_eq!(rate_response(&json!({"rateLimitResetCredits":null})), json!({"rateLimitResetCredits":null}));
        assert_eq!(rate_response(&json!({"rateLimitResetCredits":{"availableCount":null}})), json!({"rateLimitResetCredits":{"availableCount":null}}));
        let sparse = telemetry(
            "account/rateLimits/updated",
            &json!({"rateLimits":{"limitId":"codex","credits":null,"primary":{"usedPercent":-1,"resetsAt":null}}}),
            None,
        )
        .expect("sparse rates")
        .1;
        assert_eq!(sparse, json!({"rateLimits":{"limitId":"codex","credits":null,"primary":{"resetsAt":null}}}));
        let cleared =
            json!({"primary":null,"secondary":null,"credits":null,"individualLimit":null,"rateLimitReachedType":null,"limitName":null,"planType":null});
        assert_eq!(rate_snapshot(&cleared), cleared);
        let nested = json!({"primary":{"usedPercent":null,"windowDurationMins":null,"resetsAt":null},"credits":{"hasCredits":null,"unlimited":null,"balance":null},"individualLimit":{"limit":null,"used":null,"remainingPercent":null,"resetsAt":null}});
        assert_eq!(rate_snapshot(&nested), nested);
        assert_eq!(rate_snapshot(&json!({})), json!({}));
    }

    #[test]
    fn refresh_optional_failures_do_not_hide_models_and_account_is_redacted() {
        let mut server = Mock {
            replies: VecDeque::from([
                Ok(json!({"account":{"type":"chatgpt","planType":"plus","email":"private","accessToken":"secret"}})),
                Ok(catalog()),
                Ok(catalog()),
                Err("private provider diagnostics".into()),
            ]),
            ..Default::default()
        };
        let (sender, _) = mpsc::channel();
        let result = perform_inner(&mut server, &request(CodexWorkspaceAction::Refresh), Path::new("/app/codex-workspace"), &sender).expect("partial refresh");
        assert_eq!(result.pointer("/account/type"), Some(&json!("chatgpt")));
        assert_eq!(result.pointer("/models/data/1/model"), Some(&json!("vision-model")));
        assert_eq!(result.get("errors").and_then(Value::as_array).expect("errors").len(), 1);
        assert!(!result.to_string().contains("private"));
        assert!(!result.to_string().contains("secret"));
    }

    #[test]
    fn saved_chats_reject_another_workspace_and_strip_paths_and_image_references() {
        let directory = TemporaryWorkspace::new().expect("test directory");
        let foreign = TemporaryWorkspace::new().expect("foreign directory");
        let mut thread = fixture_thread(&directory.0);
        thread["turns"] = json!([{"id":"turn-one","status":"completed","items":[
            {"type":"userMessage","content":[{"type":"text","text":"Draw a tree"},{"type":"localImage","path":"private-image.png"}]},
            {"type":"commandExecution","command":"private command"},
            {"type":"agentMessage","text":"Done"},{"type":"contextCompaction"}
        ]}]);
        let mut server = Mock {
            replies: VecDeque::from([Ok(json!({"thread":thread})), Ok(json!({"thread":thread})), Ok(json!({"thread":fixture_thread(&foreign.0)}))]),
            ..Default::default()
        };
        let (sender, _) = mpsc::channel();
        let result = perform_inner(&mut server, &request(CodexWorkspaceAction::ReadThread { thread_id: "fixture-thread".into() }), &directory.0, &sender)
            .expect("read own chat");
        assert_eq!(result.pointer("/thread/turns/0/items/0/content/0/text"), Some(&json!("Draw a tree")));
        assert_eq!(result.pointer("/thread/turns/0/items").and_then(Value::as_array).expect("messages").len(), 3);
        assert!(!result.to_string().contains("private"));
        assert!(
            perform_inner(
                &mut server,
                &request(CodexWorkspaceAction::ArchiveThread { thread_id: "fixture-thread".into(), archived: true }),
                &directory.0,
                &sender
            )
            .is_err()
        );
        assert_eq!(server.calls.len(), 3, "foreign chat must not be mutated");
        assert_eq!(server.calls.first().expect("metadata check").1.get("includeTurns"), Some(&json!(false)));
        assert_eq!(server.calls.get(1).expect("history read").1.get("includeTurns"), Some(&json!(true)));
        assert_eq!(server.calls.last().expect("foreign metadata check").1.get("includeTurns"), Some(&json!(false)));
    }

    #[test]
    fn app_originator_preserves_chat_access_after_a_portable_folder_move() {
        let directory = TemporaryWorkspace::new().expect("current portable directory");
        let previous = TemporaryWorkspace::new().expect("previous portable directory");
        let mut thread = fixture_thread(&previous.0);
        thread["originator"] = json!("yorusphoto");
        assert!(owned_thread(&thread, &directory.0));
        thread["originator"] = json!("another-client");
        assert!(!owned_thread(&thread, &directory.0));
        thread["cwd"] = json!(directory.0);
        assert!(owned_thread(&thread, &directory.0), "legacy records use the authorized app cwd");
        for relative in ["data/codex-workspace", "yorusphotoData/codex-workspace", "codex-references/yorusphoto-codex-fixture"] {
            let relocated = previous.0.join(relative);
            fs::create_dir_all(&relocated).expect("previous portable workspace");
            let old_thread = fixture_thread(&relocated);
            assert!(owned_thread(&old_thread, &directory.0), "bundled 0.150 metadata can recover known app layouts");
        }
        assert!(!owned_thread(&fixture_thread(&previous.0.join("unrelated/codex-workspace")), &directory.0));
        assert!(!owned_thread(&fixture_thread(&previous.0.join("codex-references/unrelated-folder")), &directory.0));
    }

    #[test]
    fn chat_list_preserves_cursor_and_filters_unrelated_threads() {
        let directory = TemporaryWorkspace::new().expect("test directory");
        let foreign = TemporaryWorkspace::new().expect("foreign directory");
        let mut server = Mock {
            replies: VecDeque::from([Ok(json!({"data":[fixture_thread(&foreign.0),fixture_thread(&directory.0)],"nextCursor":"page-two"}))]),
            ..Default::default()
        };
        let (sender, _) = mpsc::channel();
        let result = perform_inner(
            &mut server,
            &request(CodexWorkspaceAction::ListThreads { cursor: Some("page-one".into()), archived: true, search: Some("tree".into()) }),
            &directory.0,
            &sender,
        )
        .expect("list");
        assert_eq!(result.pointer("/data").and_then(Value::as_array).expect("chats").len(), 1);
        assert_eq!(result.get("nextCursor"), Some(&json!("page-two")));
        assert_eq!(server.calls.first().expect("list call").1.get("archived"), Some(&json!(true)));
        assert!(!result.to_string().contains("private"));
    }

    #[test]
    fn manual_compaction_waits_for_completion_and_forwards_usage() {
        let directory = TemporaryWorkspace::new().expect("test directory");
        let mut server = Mock {
            replies: VecDeque::from([Ok(json!({"thread":fixture_thread(&directory.0)})), Ok(catalog()), Ok(json!({})), Ok(json!({}))]),
            notifications: VecDeque::from([
                Ok(
                    json!({"method":"item/started","params":{"threadId":"fixture-thread","turnId":"compact-turn","item":{"type":"contextCompaction","id":"compact"}}}),
                ),
                Ok(
                    json!({"method":"thread/tokenUsage/updated","params":{"threadId":"fixture-thread","turnId":"compact-turn","tokenUsage":{"last":{"totalTokens":100},"modelContextWindow":200000}}}),
                ),
                Ok(
                    json!({"method":"item/completed","params":{"threadId":"fixture-thread","turnId":"compact-turn","item":{"type":"contextCompaction","id":"compact"}}}),
                ),
                Ok(json!({"method":"turn/completed","params":{"threadId":"fixture-thread","turn":{"id":"compact-turn","status":"completed"}}})),
            ]),
            ..Default::default()
        };
        let (sender, receiver) = mpsc::channel();
        let result = perform_inner(&mut server, &request(CodexWorkspaceAction::CompactThread { thread_id: "fixture-thread".into() }), &directory.0, &sender)
            .expect("compact");
        assert_eq!(result.get("compacted"), Some(&json!(true)));
        assert!(server.notifications.is_empty(), "must wait after compact RPC acknowledgement");
        assert_eq!(server.calls.last().expect("compact call").0, "thread/compact/start");
        let resume = server.calls.iter().find(|(method, _)| method == "thread/resume").expect("load saved chat");
        assert_eq!(resume.1.get("model"), Some(&json!("text-model")));
        assert_eq!(resume.1.pointer("/config/model_reasoning_effort"), Some(&json!("low")));
        let events: Vec<_> = receiver.try_iter().collect();
        assert!(events.iter().any(|event| matches!(event,CodexWorkspaceEvent::Telemetry {kind,..} if kind=="tokenUsage")));
        assert!(events.iter().any(|event|matches!(event,CodexWorkspaceEvent::Telemetry {kind,data} if kind=="compaction" && data.get("state").and_then(Value::as_str)==Some("complete"))));
    }

    #[test]
    fn compaction_cancellation_interrupts_the_known_turn() {
        let cancel = Arc::new(AtomicBool::new(false));
        let mut server = Mock {
            notifications: VecDeque::from([Ok(
                json!({"method":"item/started","params":{"threadId":"fixture-thread","turnId":"compact-turn","item":{"type":"contextCompaction"}}}),
            )]),
            cancel_after_receive: Some(cancel.clone()),
            ..Default::default()
        };
        let (sender, _) = mpsc::channel();
        assert!(await_compaction(&mut server, "fixture-thread", &cancel, &sender).is_err());
        assert_eq!(server.interruptions, [("fixture-thread".into(), "compact-turn".into())]);
    }

    #[test]
    fn displayed_history_is_bounded_and_marks_truncation_on_utf8_boundaries() {
        let mut thread = fixture_thread(Path::new("/app"));
        thread["turns"] = json!([{"items":[{"type":"agentMessage","text":"é".repeat(MAX_MESSAGE_BYTES)}]}]);
        let safe = safe_thread(&thread, true).expect("bounded history");
        let text = safe.pointer("/turns/0/items/0/text").and_then(Value::as_str).expect("message");
        assert!(text.len() <= MAX_MESSAGE_BYTES);
        assert!(safe.get("historyTruncated").and_then(Value::as_bool).expect("truncation"));
    }
}
