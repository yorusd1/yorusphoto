//! Bounded, server-reported Codex workspace telemetry and compact native controls.
//!
//! Account limits are independent of conversation token usage. Missing fields stay
//! unknown; a passed reset timestamp never fabricates a replenished allowance.

use egui::{RichText, Ui};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const MAX_MODELS: usize = 256;
const MAX_LIMITS: usize = 64;
const MAX_COUNT: u64 = 1_000_000_000_000;
const STALE_AFTER_SECONDS: u64 = 120;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenCounts {
    pub total_tokens: Option<u64>,
    pub input_tokens: Option<u64>,
    pub cached_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_output_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TokenUsage {
    pub last: TokenCounts,
    pub total: TokenCounts,
    pub model_context_window: Option<u64>,
}

impl TokenUsage {
    /// Last-turn usage describes the current context; cumulative totals do not.
    /// The share of the context window in use, reckoned as Codex does: the 12,000 tokens that are
    /// always there (instructions and tools) count as neither used nor free.
    pub fn context_fraction(&self) -> Option<f32> {
        const BASELINE: u64 = 12_000;
        let used = self.last.total_tokens?;
        let capacity = self.model_context_window.filter(|value| *value > 0)?;
        if capacity <= BASELINE {
            return Some(1.0);
        }
        let effective = capacity - BASELINE;
        Some((used.saturating_sub(BASELINE).min(effective) as f64 / effective as f64) as f32)
    }

    pub fn valid(&self) -> bool {
        let valid_counts = |counts: &TokenCounts| {
            [
                counts.total_tokens,
                counts.input_tokens,
                counts.cached_input_tokens,
                counts.output_tokens,
                counts.reasoning_output_tokens,
                counts.cache_write_input_tokens,
            ]
            .into_iter()
            .flatten()
            .all(|value| value <= MAX_COUNT)
        };
        valid_counts(&self.last) && valid_counts(&self.total) && self.model_context_window.is_none_or(|value| value > 0 && value <= MAX_COUNT)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", content = "message", rename_all = "snake_case", deny_unknown_fields)]
pub enum Compaction {
    #[default]
    Idle,
    Running,
    Complete,
    Failed(String),
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReasoningEffort {
    pub id: String,
    pub description: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Model {
    pub id: String,
    /// Model name accepted by `turn/start`; catalog entry IDs may differ.
    pub model: String,
    pub display_name: String,
    pub description: String,
    pub supported_reasoning_efforts: Vec<ReasoningEffort>,
    pub default_reasoning_effort: Option<String>,
    pub supports_image_input: Option<bool>,
    pub is_default: bool,
    /// The runtime advertises it outside its normal picker; this is not an entitlement.
    #[serde(default)]
    pub hidden: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogInfo {
    pub source: Option<String>,
    pub freshness: Option<String>,
    pub page_count: Option<u64>,
    pub complete: Option<bool>,
    pub includes_hidden: Option<bool>,
    pub followup_queries: Option<u64>,
    pub followup_succeeded: Option<bool>,
}

impl Model {
    pub fn supports_reasoning(&self, effort: &str) -> bool {
        self.supported_reasoning_efforts.iter().any(|option| option.id == effort)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub account_type: Option<String>,
    pub plan_type: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub used_percent: Option<f64>,
    pub window_duration_mins: Option<u64>,
    pub resets_at: Option<u64>,
}

impl Window {
    pub fn remaining_percent(&self) -> Option<f64> {
        self.used_percent.map(|value| (100.0 - value).clamp(0.0, 100.0))
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Credits {
    pub has_credits: Option<bool>,
    pub unlimited: Option<bool>,
    pub balance: Option<String>,
}

impl Credits {
    pub fn exhausted(&self) -> bool {
        self.has_credits == Some(false) && self.unlimited == Some(false)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct SpendLimit {
    pub remaining_percent: Option<f64>,
    pub limit: Option<String>,
    pub used: Option<String>,
    pub resets_at: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RateLimit {
    pub id: String,
    pub name: Option<String>,
    pub primary: Option<Window>,
    pub secondary: Option<Window>,
    pub credits: Option<Credits>,
    pub plan_type: Option<String>,
    pub spend_control_reached: Option<bool>,
    pub reached_type: Option<String>,
    pub individual_limit: Option<SpendLimit>,
    pub observed_at_unix: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    Refresh,
    Compact,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub models: Vec<Model>,
    #[serde(default)]
    pub model_catalog: Option<ModelCatalogInfo>,
    pub account: Option<Account>,
    pub rate_limits: Vec<RateLimit>,
    pub token_usage: Option<TokenUsage>,
    pub compaction: Compaction,
    pub models_observed_at_unix: Option<u64>,
    pub account_observed_at_unix: Option<u64>,
    pub usage_observed_at_unix: Option<u64>,
    pub tokens_observed_at_unix: Option<u64>,
    pub compaction_observed_at_unix: Option<u64>,
    pub reset_credits_available: Option<u64>,
    pub ordinary_usage_allowed: Option<bool>,
    pub errors: Vec<String>,
}

impl Workspace {
    pub fn selected_model(&self, selected: Option<&str>) -> Option<&Model> {
        match selected {
            Some(id) => self.models.iter().find(|model| model.model == id || model.id == id),
            None => self.models.iter().find(|model| model.is_default),
        }
    }

    pub fn validate_selection(&self, model: Option<&str>, reasoning: Option<&str>) -> Result<(), String> {
        self.validate_selection_with_image(model, reasoning, false)
    }

    pub fn validate_selection_with_image(&self, model: Option<&str>, reasoning: Option<&str>, image: bool) -> Result<(), String> {
        // The runtime can resolve its own default before a catalog is available.
        let Some(selected) = self.selected_model(model) else {
            return if model.is_some() || reasoning.is_some() { Err("Refresh models and choose an available model before sending.".into()) } else { Ok(()) };
        };
        if reasoning.is_some_and(|effort| !selected.supports_reasoning(effort)) {
            return Err("That reasoning level is not supported by the selected model.".into());
        }
        if image && selected.supports_image_input == Some(false) {
            return Err("The selected model does not accept image references. Choose an image-capable model.".into());
        }
        Ok(())
    }

    pub fn apply_models(&mut self, value: &Value, now_unix: u64) -> Result<(), String> {
        let entries = value.get("data").and_then(Value::as_array).ok_or("Codex returned an invalid model catalog.")?;
        if entries.len() > MAX_MODELS {
            return Err("Codex returned too many models.".into());
        }
        if value.get("nextCursor").is_some_and(|cursor| !cursor.is_null()) {
            return Err("The model catalog is incomplete. Refresh to load all model pages.".into());
        }
        let catalog_info = if let Some(info) = value.get("catalogInfo").filter(|value| !value.is_null()) {
            if !info.is_object() {
                return Err("Codex returned invalid catalog information.".into());
            }
            let info = ModelCatalogInfo {
                source: optional_text(info, "source", 64)?,
                freshness: optional_text(info, "freshness", 64)?,
                page_count: bounded_number(info, "pageCount")?,
                complete: optional_bool(info, "complete")?,
                includes_hidden: optional_bool(info, "includesHidden")?,
                followup_queries: bounded_number(info, "followupQueries")?,
                followup_succeeded: optional_bool(info, "followupSucceeded")?,
            };
            if info.complete == Some(false)
                || info.page_count.is_some_and(|count| count == 0 || count > 8)
                || info.followup_queries.is_some_and(|count| count > 1)
            {
                return Err("Codex returned incomplete or invalid catalog information.".into());
            }
            Some(info)
        } else {
            None
        };
        let mut models: Vec<Model> = Vec::new();
        for entry in entries {
            let hidden = optional_bool(entry, "hidden")?.unwrap_or(false);
            let id = required_text(entry, "id", 256)?;
            let model = if entry.get("model").is_some_and(|value| !value.is_null()) { required_text(entry, "model", 256)? } else { id.clone() };
            let display_name = optional_text(entry, "displayName", 256)?.filter(|name| !name.is_empty()).unwrap_or_else(|| model.clone());
            let description = optional_text(entry, "description", 2048)?.unwrap_or_default();
            let mut efforts = Vec::new();
            if let Some(options) = entry.get("supportedReasoningEfforts").filter(|value| !value.is_null()) {
                let options = options.as_array().filter(|values| values.len() <= 32).ok_or("Codex returned invalid model reasoning options.")?;
                for option in options {
                    efforts.push(ReasoningEffort {
                        id: required_text(option, "reasoningEffort", 64)?,
                        description: optional_text(option, "description", 2048)?.unwrap_or_default(),
                    });
                }
            }
            let default_effort = optional_text(entry, "defaultReasoningEffort", 64)?;
            if default_effort.as_ref().is_some_and(|id| !efforts.iter().any(|effort| &effort.id == id)) {
                return Err("Codex returned an unsupported default reasoning option.".into());
            }
            let supports_image_input = match entry.get("inputModalities") {
                None | Some(Value::Null) => None,
                Some(Value::Array(items)) if items.len() <= 16 && items.iter().all(Value::is_string) => {
                    Some(items.iter().any(|item| item.as_str() == Some("image")))
                }
                _ => return Err("Codex returned invalid model input capabilities.".into()),
            };
            let model = Model {
                id,
                model,
                display_name,
                description,
                supported_reasoning_efforts: efforts,
                default_reasoning_effort: default_effort,
                supports_image_input,
                is_default: entry.get("isDefault").and_then(Value::as_bool).unwrap_or(false),
                hidden,
            };
            if let Some(existing) = models.iter_mut().find(|entry| entry.model == model.model) {
                if existing.hidden && !model.hidden {
                    *existing = model;
                }
            } else {
                models.push(model);
            }
        }
        self.models = models;
        self.model_catalog = catalog_info;
        self.models_observed_at_unix = Some(now_unix);
        Ok(())
    }

    pub fn apply_account(&mut self, value: &Value, now_unix: u64) -> Result<(), String> {
        let account = value.get("account").unwrap_or(value);
        if account.is_null() {
            self.account = None;
        } else {
            if !account.is_object() {
                return Err("Codex returned invalid account information.".into());
            }
            self.account = Some(Account { account_type: optional_text(account, "type", 64)?, plan_type: optional_text(account, "planType", 128)? });
        }
        self.account_observed_at_unix = Some(now_unix);
        Ok(())
    }

    /// Merge sparse notifications by limit ID, preferring the server's multi-bucket view.
    pub fn apply_rate_limits(&mut self, value: &Value, now_unix: u64) -> Result<(), String> {
        let mut next = self.rate_limits.clone();
        let mut updated = false;
        if let Some(map) = value.get("rateLimitsByLimitId").filter(|value| !value.is_null()) {
            let map = map.as_object().filter(|map| map.len() <= MAX_LIMITS).ok_or("Codex returned invalid usage buckets.")?;
            for (id, limit) in map {
                merge_limit(&mut next, id, limit, now_unix)?;
                updated = true;
            }
        }
        if !updated && let Some(limit) = value.get("rateLimits").filter(|value| !value.is_null()) {
            let id = optional_text(limit, "limitId", 256)?.unwrap_or_else(|| "codex".into());
            merge_limit(&mut next, &id, limit, now_unix)?;
            updated = true;
        } else if !updated
            && (value.get("primary").is_some() || value.get("secondary").is_some() || value.get("credits").is_some() || value.get("limitId").is_some())
        {
            let id = optional_text(value, "limitId", 256)?.unwrap_or_else(|| "codex".into());
            merge_limit(&mut next, &id, value, now_unix)?;
            updated = true;
        }
        let ordinary_usage_allowed = if value.get("ordinaryUsageAllowed").is_some() { Some(optional_bool(value, "ordinaryUsageAllowed")?) } else { None };
        let reset_credits = match value.get("rateLimitResetCredits") {
            None => None,
            Some(Value::Null) => Some(None),
            Some(credits) if credits.is_object() => {
                if credits.get("availableCount").is_some() {
                    Some(bounded_number(credits, "availableCount")?)
                } else {
                    None
                }
            }
            Some(_) => return Err("Codex returned invalid reset credit information.".into()),
        };
        if !updated && ordinary_usage_allowed.is_none() && reset_credits.is_none() {
            return Err("Codex did not provide account usage data.".into());
        }
        self.rate_limits = next;
        if let Some(allowed) = ordinary_usage_allowed {
            self.ordinary_usage_allowed = allowed;
        }
        if let Some(available) = reset_credits {
            self.reset_credits_available = available;
        }
        self.usage_observed_at_unix = Some(now_unix);
        Ok(())
    }

    pub fn apply_token_usage(&mut self, value: &Value, now_unix: u64) -> Result<(), String> {
        let value = value.get("tokenUsage").unwrap_or(value);
        if !value.is_object() {
            return Err("Codex returned invalid token usage.".into());
        }
        let usage = TokenUsage {
            last: parse_counts(value.get("last"))?,
            total: parse_counts(value.get("total"))?,
            model_context_window: bounded_number(value, "modelContextWindow")?,
        };
        if !usage.valid() || (usage.last == TokenCounts::default() && usage.total == TokenCounts::default() && usage.model_context_window.is_none()) {
            return Err("Codex returned unavailable or invalid token usage.".into());
        }
        self.token_usage = Some(usage);
        self.tokens_observed_at_unix = Some(now_unix);
        Ok(())
    }

    pub fn apply_compaction(&mut self, state: Compaction, now_unix: u64) {
        if state == Compaction::Complete && self.compaction != Compaction::Complete {
            // The old context count is not the size of the newly compacted input.
            self.tokens_observed_at_unix = None;
        }
        self.compaction = match state {
            Compaction::Failed(message) if message.len() > 2048 => Compaction::Failed("Compaction failed; check the task status.".into()),
            other => other,
        };
        self.compaction_observed_at_unix = Some(now_unix);
    }

    pub fn snapshot(&self) -> Value {
        json!({"models": self.models,"modelCatalog":self.model_catalog,"modelsObservedAt":self.models_observed_at_unix, "account": self.account, "rateLimits": self.rate_limits,
            "tokenUsage": self.token_usage, "compaction": self.compaction,
            "usageObservedAt": self.usage_observed_at_unix, "tokensObservedAt": self.tokens_observed_at_unix,
            "resetCreditsAvailable": self.reset_credits_available, "ordinaryUsageAllowed":self.ordinary_usage_allowed,"errors": self.errors})
    }

    pub fn warnings(&self) -> Vec<String> {
        let mut result = Vec::new();
        if self.ordinary_usage_allowed == Some(false) {
            result.push("Codex reports that ordinary account usage is currently restricted.".into());
        }
        for limit in &self.rate_limits {
            let name = limit.name.as_deref().unwrap_or(&limit.id);
            for (label, window) in [("primary", &limit.primary), ("secondary", &limit.secondary)] {
                if let Some(used) = window.as_ref().and_then(|window| window.used_percent) {
                    let severity = if used >= 100.0 {
                        Some("limit reached")
                    } else if used >= 95.0 {
                        Some("almost used up")
                    } else if used >= 80.0 {
                        Some("running low")
                    } else {
                        None
                    };
                    if let Some(severity) = severity {
                        result.push(format!("{name} {label}: {severity} ({:.0}% remaining).", (100.0 - used).clamp(0.0, 100.0)));
                    }
                }
            }
            let has_reported_allowance =
                [&limit.primary, &limit.secondary].into_iter().flatten().any(|window| window.remaining_percent().is_some_and(|remaining| remaining > 0.0));
            let explicitly_restricted = self.ordinary_usage_allowed == Some(false) || limit.spend_control_reached == Some(true) || limit.reached_type.is_some();
            if limit.credits.as_ref().is_some_and(Credits::exhausted) && (!has_reported_allowance || explicitly_restricted) {
                result.push(format!("{name}: Codex reports no available credits."));
            }
            if limit.spend_control_reached == Some(true) || limit.reached_type.is_some() {
                result.push(format!("{name}: an account or workspace usage limit has been reached."));
            }
            if let Some(remaining) = limit.individual_limit.as_ref().and_then(|value| value.remaining_percent)
                && remaining <= 20.0
            {
                result.push(format!("{name}: individual allowance {:.0}% remaining.", remaining.clamp(0.0, 100.0)));
            }
        }
        result
    }
}

fn required_text(value: &Value, field: &str, cap: usize) -> Result<String, String> {
    optional_text(value, field, cap)?
        .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        .ok_or_else(|| format!("Codex returned missing or invalid {field}."))
}

fn optional_text(value: &Value, field: &str, cap: usize) -> Result<Option<String>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) if text.len() <= cap && !text.chars().any(|character| character.is_control() && character != '\n' && character != '\t') => {
            Ok(Some(text.clone()))
        }
        _ => Err(format!("Codex returned invalid {field}.")),
    }
}

fn bounded_number(value: &Value, field: &str) -> Result<Option<u64>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_u64().filter(|number| *number <= MAX_COUNT).map(Some).ok_or_else(|| format!("Codex returned invalid {field}.")),
    }
}

fn optional_bool(value: &Value, field: &str) -> Result<Option<bool>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        _ => Err(format!("Codex returned invalid {field}.")),
    }
}

fn percent(value: &Value, field: &str) -> Result<Option<f64>, String> {
    match value.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_f64()
            .filter(|number| number.is_finite() && (0.0..=10_000.0).contains(number))
            .map(Some)
            .ok_or_else(|| format!("Codex returned invalid {field}.")),
    }
}

fn parse_counts(value: Option<&Value>) -> Result<TokenCounts, String> {
    let Some(value) = value.filter(|value| !value.is_null()) else {
        return Ok(TokenCounts::default());
    };
    if !value.is_object() {
        return Err("Codex returned invalid token counts.".into());
    }
    Ok(TokenCounts {
        total_tokens: bounded_number(value, "totalTokens")?,
        input_tokens: bounded_number(value, "inputTokens")?,
        cached_input_tokens: bounded_number(value, "cachedInputTokens")?,
        output_tokens: bounded_number(value, "outputTokens")?,
        reasoning_output_tokens: bounded_number(value, "reasoningOutputTokens")?,
        cache_write_input_tokens: bounded_number(value, "cacheWriteInputTokens")?,
    })
}

fn merge_window(old: Option<Window>, value: &Value) -> Result<Window, String> {
    if !value.is_object() {
        return Err("Codex returned an invalid usage window.".into());
    }
    let mut next = old.unwrap_or_default();
    if value.get("usedPercent").is_some() {
        next.used_percent = percent(value, "usedPercent")?;
    }
    if value.get("windowDurationMins").is_some() {
        next.window_duration_mins = bounded_number(value, "windowDurationMins")?;
    }
    if value.get("resetsAt").is_some() {
        next.resets_at = bounded_number(value, "resetsAt")?;
    }
    Ok(next)
}

fn merge_limit(limits: &mut Vec<RateLimit>, id: &str, value: &Value, now_unix: u64) -> Result<(), String> {
    if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) || !value.is_object() {
        return Err("Codex returned an invalid usage bucket.".into());
    }
    let existing = limits.iter().position(|limit| limit.id == id);
    let mut next = existing.and_then(|index| limits.get(index)).cloned().unwrap_or_else(|| RateLimit { id: id.into(), ..Default::default() });
    if value.get("limitName").is_some() {
        next.name = optional_text(value, "limitName", 256)?;
    }
    if value.get("planType").is_some() {
        next.plan_type = optional_text(value, "planType", 128)?;
    }
    for (key, target) in [("primary", &mut next.primary), ("secondary", &mut next.secondary)] {
        if let Some(window) = value.get(key) {
            *target = if window.is_null() { None } else { Some(merge_window(target.clone(), window)?) };
        }
    }
    if let Some(credits) = value.get("credits") {
        if credits.is_null() {
            next.credits = None;
        } else if !credits.is_object() {
            return Err("Codex returned invalid credit information.".into());
        } else {
            let mut merged = next.credits.clone().unwrap_or_default();
            if credits.get("hasCredits").is_some() {
                merged.has_credits = optional_bool(credits, "hasCredits")?;
            }
            if credits.get("unlimited").is_some() {
                merged.unlimited = optional_bool(credits, "unlimited")?;
            }
            if credits.get("balance").is_some() {
                merged.balance = optional_text(credits, "balance", 128)?;
            }
            next.credits = Some(merged);
        }
    }
    if value.get("spendControlReached").is_some() {
        next.spend_control_reached = optional_bool(value, "spendControlReached")?;
    }
    if value.get("rateLimitReachedType").is_some() {
        next.reached_type = optional_text(value, "rateLimitReachedType", 128)?;
    }
    if let Some(individual) = value.get("individualLimit") {
        if individual.is_null() {
            next.individual_limit = None;
        } else if !individual.is_object() {
            return Err("Codex returned an invalid individual limit.".into());
        } else {
            let mut merged = next.individual_limit.clone().unwrap_or_default();
            if individual.get("remainingPercent").is_some() {
                merged.remaining_percent = percent(individual, "remainingPercent")?.map(|remaining| remaining.clamp(0.0, 100.0));
            }
            if individual.get("limit").is_some() {
                merged.limit = optional_text(individual, "limit", 128)?;
            }
            if individual.get("used").is_some() {
                merged.used = optional_text(individual, "used", 128)?;
            }
            if individual.get("resetsAt").is_some() {
                merged.resets_at = bounded_number(individual, "resetsAt")?;
            }
            next.individual_limit = Some(merged);
        }
    }
    next.observed_at_unix = Some(now_unix);
    if let Some(index) = existing {
        if let Some(limit) = limits.get_mut(index) {
            *limit = next;
        }
    } else if limits.len() < MAX_LIMITS {
        limits.push(next);
    } else {
        return Err("Codex returned too many usage buckets.".into());
    }
    Ok(())
}

/// A dynamic catalog selector. No models or reasoning levels are guessed locally.
pub fn model_ui(ui: &mut Ui, workspace: &Workspace, selected: &mut Option<String>, reasoning: &mut Option<String>, busy: bool) -> bool {
    let t = crate::ai_sidebar::palette(ui);
    let mut changed = false;
    ui.add_enabled_ui(!busy, |ui| {
        ui.horizontal_wrapped(|ui| {
            let label = workspace
                .selected_model(selected.as_deref())
                .map(|model| model.display_name.clone())
                .unwrap_or_else(|| selected.clone().unwrap_or_else(|| "Codex default".into()));
            craft_ui::ComboBox::from_id_salt("codex-model")
                .width(165.0)
                .selected_text(label)
                .show_ui(ui, |ui| {
                    changed |= ui.selectable_value(selected, None, "Codex default").changed();
                    for model in workspace.models.iter().filter(|model| !model.hidden) {
                        changed |= ui.selectable_value(selected, Some(model.model.clone()), &model.display_name).on_hover_text(&model.description).changed();
                    }
                    if workspace.models.iter().any(|model| model.hidden) {
                        ui.separator();
                        ui.label(RichText::new("Additional catalog models").small().color(t.text_dim));
                        for model in workspace.models.iter().filter(|model| model.hidden) {
                            changed |= ui
                                .selectable_value(selected, Some(model.model.clone()), &model.display_name)
                                .on_hover_text(format!(
                                    "{}\nCodex hides this model from its normal picker. Supported uses and access are determined by the runtime.",
                                    model.description
                                ))
                                .changed();
                        }
                    }
                    if !workspace.models.is_empty() {
                        ui.separator();
                        ui.label(RichText::new("Runtime catalog · server freshness unreported").small().color(t.text_dim))
                            .on_hover_text(catalog_observation_label(workspace));
                    }
                })
                .response
                .on_hover_text(catalog_observation_label(workspace));
            if let Some(model) = workspace.selected_model(selected.as_deref()) {
                if reasoning.as_deref().is_some_and(|effort| !model.supports_reasoning(effort)) {
                    *reasoning = None;
                    changed = true;
                }
                if !model.supported_reasoning_efforts.is_empty() {
                    craft_ui::ComboBox::from_id_salt("codex-reasoning").width(78.0).selected_text(reasoning.as_deref().unwrap_or("Default")).show_ui(ui, |ui| {
                        changed |= ui.selectable_value(reasoning, None, "Default reasoning").changed();
                        for effort in &model.supported_reasoning_efforts {
                            changed |= ui.selectable_value(reasoning, Some(effort.id.clone()), &effort.id).on_hover_text(&effort.description).changed();
                        }
                    });
                }
            }
        });
    });
    if workspace.models.is_empty() {
        ui.label(RichText::new("Refresh to load available models.").small().color(t.text_faint));
    } else if selected.is_some() && workspace.selected_model(selected.as_deref()).is_none() {
        ui.label(RichText::new("Selected model is unavailable. Choose another model.").small().color(t.warning));
    }
    changed
}

fn catalog_observation_label(workspace: &Workspace) -> String {
    let checked = workspace.models_observed_at_unix.and_then(utc_timestamp).unwrap_or_else(|| "not yet checked".into());
    let pages = workspace
        .model_catalog
        .as_ref()
        .and_then(|info| info.page_count)
        .map(|count| format!("{count} pages"))
        .unwrap_or_else(|| "page count not reported".into());
    let followup = match workspace.model_catalog.as_ref().and_then(|info| info.followup_succeeded) {
        Some(true) => "The same runtime process was re-queried after its startup refresh opportunity.",
        Some(false) => "The follow-up query failed; the first complete catalog is retained.",
        None => "The runtime reports its catalog; network/cache origin is not exposed by this interface.",
    };
    format!(
        "Catalog checked: {checked} · {pages}. Server/cache freshness is unreported.\n{followup}\nA catalog listing does not guarantee access to every model."
    )
}

/// Always-visible context and compaction status; account details stay in a popover.
pub fn status_ui(ui: &mut Ui, workspace: &Workspace, now_unix: u64) -> Vec<Action> {
    let t = crate::ai_sidebar::palette(ui);
    let mut actions = Vec::new();
    ui.horizontal_wrapped(|ui| {
        let mut label = match workspace.token_usage.as_ref() {
            Some(usage) => match (usage.last.total_tokens, usage.model_context_window) {
                (Some(used), Some(total)) => format!("Context {} / {}", compact_count(used), compact_count(total)),
                (Some(used), None) => format!("Context {} tokens", compact_count(used)),
                _ => "Context unavailable".into(),
            },
            None => "Context unavailable".into(),
        };
        if workspace.token_usage.is_some() && is_stale(workspace.tokens_observed_at_unix, now_unix) {
            label.push_str(" · last reported");
        }
        ui.label(RichText::new(label).small().color(t.text_dim)).on_hover_text("Last-turn context usage reported by Codex. Cumulative conversation token totals appear in Usage. Context tokens are separate from account credits.");
        let button = ui.small_button("Usage");
        egui::Popup::menu(&button).show(|ui| {
            ui.set_max_width(310.0);
            egui::ScrollArea::vertical().max_height(400.0).show(ui, |ui| { actions.extend(usage_ui(ui, workspace, now_unix)); });
        });
        match &workspace.compaction {
            Compaction::Idle => { ui.label(RichText::new("Auto compact").small().color(t.text_faint)).on_hover_text("Codex compacts context automatically when needed. Use /compact to request it now."); }
            Compaction::Running => { ui.spinner(); ui.label(RichText::new("Compacting…").small().color(t.text_dim)); }
            Compaction::Complete => { ui.label(RichText::new("Compacted").small().color(t.text_dim)); }
            Compaction::Failed(message) => { ui.label(RichText::new("Compaction failed").small().color(t.warning)).on_hover_text(message); }
        }
    });
    if let Some(fraction) = workspace.token_usage.as_ref().and_then(TokenUsage::context_fraction) {
        ui.add(egui::ProgressBar::new(fraction).desired_height(4.0).fill(if fraction >= 0.9 { t.warning } else { t.accent }));
    }
    if let Some(warning) = workspace.warnings().first() {
        let stale = is_stale(workspace.usage_observed_at_unix, now_unix);
        ui.label(RichText::new(format!("{}{warning}", if stale { "Last reported: " } else { "" })).small().color(t.warning))
            .on_hover_text(workspace.warnings().join("\n"));
    }
    actions
}

/// Expanded account and context view, shared with the `/status` command.
pub fn usage_ui(ui: &mut Ui, workspace: &Workspace, now_unix: u64) -> Vec<Action> {
    let t = crate::ai_sidebar::palette(ui);
    let mut actions = Vec::new();
    ui.horizontal(|ui| {
        ui.label(RichText::new("Account usage").strong());
        if ui.small_button("Refresh").clicked() {
            actions.push(Action::Refresh);
        }
    });
    if let Some(account) = &workspace.account {
        ui.label(match (&account.account_type, &account.plan_type) {
            (Some(kind), Some(plan)) => format!("{kind} · {plan}"),
            (Some(kind), None) => kind.clone(),
            _ => "Account details unavailable".into(),
        });
    }
    ui.label(RichText::new(freshness(workspace.usage_observed_at_unix, now_unix)).small().color(t.text_faint));
    if workspace.rate_limits.is_empty() {
        ui.label("Usage is unavailable. Refresh after connecting to ChatGPT.");
    }
    for limit in &workspace.rate_limits {
        ui.separator();
        ui.label(RichText::new(limit.name.as_deref().unwrap_or(&limit.id)).strong());
        if let Some(plan) = &limit.plan_type {
            ui.label(RichText::new(format!("Plan: {plan}")).small().color(t.text_dim));
        }
        if limit.observed_at_unix != workspace.usage_observed_at_unix {
            ui.label(RichText::new(freshness(limit.observed_at_unix, now_unix)).small().color(t.text_faint));
        }
        for (label, window) in [("Primary", &limit.primary), ("Secondary", &limit.secondary)] {
            if let Some(window) = window {
                window_ui(ui, label, window, now_unix);
            }
        }
        ui.label(credits_label(limit.credits.as_ref()));
        if let Some(individual) = &limit.individual_limit {
            ui.label(match individual.remaining_percent {
                Some(remaining) => format!("Individual allowance: {remaining:.0}% remaining"),
                None => "Individual allowance: unavailable".into(),
            });
            if let (Some(used), Some(total)) = (&individual.used, &individual.limit) {
                ui.label(format!("Reported spend: {used} / {total}"));
            }
            ui.label(RichText::new(reset_label(individual.resets_at, now_unix)).small().color(t.text_dim));
        }
    }
    if let Some(count) = workspace.reset_credits_available {
        ui.label(format!("Available account reset credits: {count}"));
    }
    for warning in workspace.warnings() {
        ui.label(RichText::new(warning).small().color(t.warning));
    }
    for error in workspace.errors.iter().take(8) {
        ui.label(RichText::new(error).small().color(t.warning));
    }
    ui.separator();
    ui.label(RichText::new("Conversation context").strong());
    if let Some(usage) = &workspace.token_usage {
        counts_ui(ui, "Last turn", &usage.last);
        counts_ui(ui, "Conversation total", &usage.total);
        ui.label(match usage.model_context_window {
            Some(size) => format!("Model context window: {} tokens", compact_count(size)),
            None => "Model context window: unavailable".into(),
        });
        ui.label(RichText::new(freshness(workspace.tokens_observed_at_unix, now_unix)).small().color(t.text_faint));
    } else {
        ui.label("Token usage is reported after a Codex turn.");
    }
    if ui.add_enabled(workspace.compaction != Compaction::Running, egui::Button::new("Compact context now")).clicked() {
        actions.push(Action::Compact);
    }
    ui.label(
        RichText::new("Codex also compacts automatically. Context tokens do not measure account credits. Unavailable values do not mean zero or unlimited.")
            .small()
            .color(t.text_faint),
    );
    actions
}

fn window_ui(ui: &mut Ui, label: &str, window: &Window, now_unix: u64) {
    let t = crate::ai_sidebar::palette(ui);
    let duration = window.window_duration_mins.map(|minutes| format!(" · {} window", duration_label(minutes.saturating_mul(60)))).unwrap_or_default();
    ui.label(match (window.used_percent, window.remaining_percent()) {
        (Some(used), Some(remaining)) => format!("{label}{duration}: {remaining:.0}% remaining ({used:.0}% used)"),
        _ => format!("{label}{duration}: usage unavailable"),
    });
    if let Some(remaining) = window.remaining_percent() {
        ui.add(egui::ProgressBar::new((remaining / 100.0) as f32).desired_height(5.0).fill(if remaining <= 20.0 { t.warning } else { t.accent }));
    }
    ui.label(RichText::new(reset_label(window.resets_at, now_unix)).small().color(t.text_dim));
}

fn counts_ui(ui: &mut Ui, label: &str, counts: &TokenCounts) {
    let number = |value: Option<u64>| value.map(compact_count).unwrap_or_else(|| "unavailable".into());
    ui.label(format!("{label}: {} tokens", number(counts.total_tokens)));
    ui.label(
        RichText::new(format!(
            "Input {} · cached {} · output {} · reasoning {}",
            number(counts.input_tokens),
            number(counts.cached_input_tokens),
            number(counts.output_tokens),
            number(counts.reasoning_output_tokens)
        ))
        .small(),
    );
}

pub fn credits_label(credits: Option<&Credits>) -> String {
    match credits {
        None => "Credits: unavailable".into(),
        Some(credits) if credits.unlimited == Some(true) => "Credits: unlimited (reported by Codex)".into(),
        Some(credits) if credits.exhausted() => "Credits: none available (reported by Codex)".into(),
        Some(Credits { balance: Some(balance), .. }) => format!("Credit balance: {balance}"),
        Some(Credits { has_credits: Some(true), .. }) => "Credits available · balance not reported".into(),
        Some(_) => "Credits: unavailable".into(),
    }
}

pub fn compact_count(count: u64) -> String {
    if count >= 1_000_000 {
        format!("{:.1}M", count as f64 / 1_000_000.0)
    } else if count >= 1_000 {
        format!("{:.1}k", count as f64 / 1_000.0)
    } else {
        count.to_string()
    }
}

fn duration_label(seconds: u64) -> String {
    if seconds >= 86_400 {
        format!("{}d {}h", seconds / 86_400, (seconds % 86_400) / 3_600)
    } else if seconds >= 3_600 {
        format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{seconds}s")
    }
}

pub fn is_stale(observed: Option<u64>, now_unix: u64) -> bool {
    observed.is_none_or(|time| now_unix.checked_sub(time).is_none_or(|age| age > STALE_AFTER_SECONDS))
}

pub fn freshness(observed: Option<u64>, now_unix: u64) -> String {
    match observed.and_then(|time| now_unix.checked_sub(time)) {
        Some(age) if age <= 5 => "Updated just now".into(),
        Some(age) if age <= STALE_AFTER_SECONDS => format!("Updated {} ago", duration_label(age)),
        Some(age) => format!("Last reported {} ago · refresh for current usage", duration_label(age)),
        None => "Last update unavailable · refresh for current usage".into(),
    }
}

pub fn reset_label(reset: Option<u64>, now_unix: u64) -> String {
    let Some(reset) = reset else {
        return "Reset time unavailable".into();
    };
    let Some(absolute) = utc_timestamp(reset) else {
        return "Reset time unavailable".into();
    };
    if reset <= now_unix {
        format!("Reset was {absolute} · refresh usage")
    } else {
        format!("Resets in {} · {absolute}", duration_label(reset.saturating_sub(now_unix)))
    }
}

/// UTC civil-date conversion, bounded through year 9999 and independent of host APIs.
fn utc_timestamp(seconds: u64) -> Option<String> {
    if seconds > 253_402_300_799 {
        return None;
    }
    let days = i64::try_from(seconds / 86_400).ok()? + 719_468;
    let era = days / 146_097;
    let day_of_era = days - era * 146_097;
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    Some(format!("{year:04}-{month:02}-{day:02} {:02}:{:02} UTC", (seconds % 86_400) / 3_600, (seconds % 3_600) / 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn context_uses_last_turn_not_cumulative_conversation_tokens() {
        let mut workspace = Workspace::default();
        workspace.apply_token_usage(&json!({"threadId":"test","tokenUsage":{"last":{"totalTokens":25000,"inputTokens":20000,"outputTokens":5000},"total":{"totalTokens":900000},"modelContextWindow":100000}}), 100).unwrap();
        let usage = workspace.token_usage.as_ref().unwrap();
        // 25,000 of 100,000, less Codex's 12,000-token baseline on both sides.
        let quarter = Some((13_000.0 / 88_000.0) as f32);
        assert_eq!(usage.context_fraction(), quarter);
        assert_eq!(usage.total.total_tokens, Some(900000));
        assert_eq!(usage.last.cached_input_tokens, None);
        assert!(workspace.apply_token_usage(&json!({"last":{"totalTokens":-1}}), 101).is_err());
        assert_eq!(workspace.token_usage.as_ref().unwrap().context_fraction(), quarter);
        assert!(workspace.apply_token_usage(&json!({"last":{"totalTokens":1},"modelContextWindow":0}), 101).is_err());
        workspace.apply_compaction(Compaction::Complete, 102);
        assert_eq!(workspace.tokens_observed_at_unix, None);
        assert_eq!(workspace.token_usage.as_ref().unwrap().context_fraction(), quarter);
        workspace.apply_token_usage(&json!({"last":{"totalTokens":1000},"modelContextWindow":100000}), 103).unwrap();
        workspace.apply_compaction(Compaction::Complete, 104);
        assert_eq!(workspace.tokens_observed_at_unix, Some(103), "duplicate completion must preserve newer post-compaction usage");
    }

    #[test]
    fn missing_credits_never_mean_zero_or_unlimited() {
        assert_eq!(credits_label(None), "Credits: unavailable");
        assert!(!Credits::default().exhausted());
        assert!(!Credits { has_credits: Some(false), ..Default::default() }.exhausted());
        assert!(Credits { has_credits: Some(false), unlimited: Some(false), balance: None }.exhausted());
        assert!(credits_label(Some(&Credits { unlimited: Some(true), ..Default::default() })).contains("unlimited"));
        let mut workspace = Workspace::default();
        workspace.apply_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":null}}}), 100).unwrap();
        assert_eq!(workspace.rate_limits[0].primary.as_ref().unwrap().remaining_percent(), None);
        assert!(workspace.warnings().is_empty());
    }

    #[test]
    fn sparse_usage_updates_preserve_other_windows_and_buckets() {
        let mut workspace = Workspace::default();
        workspace.apply_rate_limits(&json!({"rateLimitsByLimitId":{"codex":{"primary":{"usedPercent":80,"resetsAt":300},"secondary":{"usedPercent":10},"credits":{"hasCredits":true,"unlimited":false,"balance":"123"},"spendControlReached":true},"reviews":{"primary":{"usedPercent":40}}}}), 100).unwrap();
        workspace.apply_rate_limits(&json!({"rateLimits":{"limitId":"codex","primary":{"usedPercent":95},"spendControlReached":null}}), 110).unwrap();
        assert_eq!(workspace.rate_limits.len(), 2);
        let codex = workspace.rate_limits.iter().find(|limit| limit.id == "codex").unwrap();
        assert_eq!(codex.primary.as_ref().unwrap().remaining_percent(), Some(5.0));
        assert_eq!(codex.primary.as_ref().unwrap().resets_at, Some(300));
        assert_eq!(codex.secondary.as_ref().unwrap().used_percent, Some(10.0));
        assert_eq!(codex.credits.as_ref().unwrap().balance.as_deref(), Some("123"));
        assert_eq!(codex.spend_control_reached, None);
        assert_eq!(workspace.rate_limits.iter().find(|limit| limit.id == "reviews").unwrap().observed_at_unix, Some(100));
        assert!(workspace.warnings().iter().any(|warning| warning.contains("almost used up")));
        workspace.apply_rate_limits(&json!({"rateLimitsByLimitId":{},"rateLimits":{"limitId":"codex","primary":{"usedPercent":50}}}), 111).unwrap();
        assert_eq!(workspace.rate_limits.iter().find(|limit| limit.id == "codex").unwrap().primary.as_ref().unwrap().used_percent, Some(50.0));
        workspace.apply_rate_limits(&json!({"rateLimits":{"limitId":"codex","limitName":"Codex","planType":"pro","individualLimit":{"remainingPercent":10,"limit":"100","used":"90","resetsAt":300}},"rateLimitResetCredits":{"availableCount":2}}), 112).unwrap();
        workspace.apply_rate_limits(&json!({"rateLimits":{"limitId":"codex","limitName":null,"planType":null,"primary":{"usedPercent":null,"resetsAt":null,"windowDurationMins":null},"secondary":null,"credits":{"hasCredits":null,"unlimited":null,"balance":null},"individualLimit":{"remainingPercent":null,"limit":null,"used":null,"resetsAt":null}},"rateLimitResetCredits":{"availableCount":null}}), 113).unwrap();
        let codex = workspace.rate_limits.iter().find(|limit| limit.id == "codex").unwrap();
        assert_eq!(codex.name, None);
        assert_eq!(codex.plan_type, None);
        assert_eq!(codex.primary, Some(Window::default()));
        assert_eq!(codex.secondary, None);
        assert_eq!(codex.credits, Some(Credits::default()));
        assert_eq!(codex.individual_limit, Some(SpendLimit::default()));
        assert_eq!(workspace.reset_credits_available, None);
        assert!(workspace.warnings().is_empty(), "unknown values must not retain stale warning states");
        workspace.apply_rate_limits(&json!({"rateLimits":{"limitId":"codex","primary":null,"credits":null,"individualLimit":null}}), 114).unwrap();
        let codex = workspace.rate_limits.iter().find(|limit| limit.id == "codex").unwrap();
        assert_eq!(codex.primary, None);
        assert_eq!(codex.credits, None);
        assert_eq!(codex.individual_limit, None);
    }

    #[test]
    fn malformed_usage_update_does_not_replace_valid_snapshot() {
        let mut workspace = Workspace::default();
        workspace.apply_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":40}}}), 100).unwrap();
        let original = workspace.clone();
        for malformed in [
            json!({"rateLimits":{"primary":{"usedPercent":-1}}}),
            json!({"rateLimitsByLimitId":{"codex":{"credits":{"hasCredits":"yes"}}}}),
            json!({"rateLimits":{"credits":{"balance":"x".repeat(129)}}}),
            json!({"rateLimitsByLimitId":[]}),
        ] {
            assert!(workspace.apply_rate_limits(&malformed, 101).is_err());
            assert_eq!(workspace, original);
        }
    }

    #[test]
    fn warnings_distinguish_thresholds_and_explicit_credit_exhaustion() {
        for (used, text) in [(80, "running low"), (95, "almost used up"), (100, "limit reached")] {
            let mut workspace = Workspace::default();
            workspace.apply_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":used}}}), 100).unwrap();
            assert!(workspace.warnings().iter().any(|warning| warning.contains(text)));
        }
        let mut workspace = Workspace::default();
        workspace.apply_rate_limits(&json!({"rateLimits":{"credits":{"balance":"0","hasCredits":false}}}), 100).unwrap();
        assert!(workspace.warnings().is_empty());
        workspace.apply_rate_limits(&json!({"rateLimits":{"credits":{"unlimited":false}}}), 101).unwrap();
        assert!(workspace.warnings().iter().any(|warning| warning.contains("no available credits")));
        workspace.apply_rate_limits(&json!({"rateLimits":{"credits":{"hasCredits":null}}}), 102).unwrap();
        assert!(workspace.warnings().is_empty(), "explicit unknown credit availability clears the old exhausted warning");
        workspace.apply_rate_limits(&json!({"rateLimits":{"credits":{"hasCredits":false}}}), 103).unwrap();
        assert!(!workspace.warnings().is_empty());
        workspace.apply_rate_limits(&json!({"rateLimits":{"credits":null}}), 104).unwrap();
        assert_eq!(workspace.rate_limits.first().unwrap().credits, None);
        assert!(workspace.warnings().is_empty());
        workspace.apply_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":60},"credits":{"hasCredits":false,"unlimited":false}}}), 105).unwrap();
        assert!(workspace.warnings().is_empty(), "reported allowance remains usable even when the reported credit balance is empty");
        assert_eq!(credits_label(workspace.rate_limits.first().unwrap().credits.as_ref()), "Credits: none available (reported by Codex)");
        workspace.apply_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":95}}}), 106).unwrap();
        assert!(workspace.warnings().iter().any(|warning| warning.contains("almost used up")));
        assert!(!workspace.warnings().iter().any(|warning| warning.contains("no available credits")));
        workspace.apply_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":100}}}), 107).unwrap();
        assert!(workspace.warnings().iter().any(|warning| warning.contains("no available credits")));
        workspace.apply_rate_limits(&json!({"rateLimits":{"secondary":{"usedPercent":10}}}), 108).unwrap();
        assert!(!workspace.warnings().iter().any(|warning| warning.contains("no available credits")));
        assert!(workspace.warnings().iter().any(|warning| warning.contains("limit reached")));
        workspace.apply_rate_limits(&json!({"rateLimits":{"spendControlReached":true}}), 109).unwrap();
        assert!(workspace.warnings().iter().any(|warning| warning.contains("no available credits")));
        let mut restriction = Workspace::default();
        restriction.apply_rate_limits(&json!({"ordinaryUsageAllowed":false}), 102).unwrap();
        assert_eq!(restriction.ordinary_usage_allowed, Some(false));
        assert!(restriction.warnings().iter().any(|warning| warning.contains("restricted")));
        assert!(!restriction.warnings().iter().any(|warning| warning.contains("credits")));
        restriction.apply_rate_limits(&json!({"rateLimits":{"primary":{"usedPercent":10}}}), 103).unwrap();
        assert_eq!(restriction.ordinary_usage_allowed, Some(false), "absent sparse fields retain the last reported restriction");
        restriction.apply_rate_limits(&json!({"ordinaryUsageAllowed":true}), 104).unwrap();
        assert!(restriction.warnings().is_empty());
        restriction.apply_rate_limits(&json!({"ordinaryUsageAllowed":false}), 105).unwrap();
        restriction.apply_rate_limits(&json!({"ordinaryUsageAllowed":null}), 106).unwrap();
        assert_eq!(restriction.ordinary_usage_allowed, None);
        assert!(restriction.warnings().is_empty());
        assert!(restriction.apply_rate_limits(&json!({"ordinaryUsageAllowed":"unknown"}), 107).is_err());
    }

    #[test]
    fn reset_dates_have_utc_timezone_and_never_assume_replenishment() {
        assert_eq!(utc_timestamp(0).as_deref(), Some("1970-01-01 00:00 UTC"));
        assert_eq!(utc_timestamp(951_782_400).as_deref(), Some("2000-02-29 00:00 UTC"));
        assert_eq!(utc_timestamp(253_402_300_799).as_deref(), Some("9999-12-31 23:59 UTC"));
        assert_eq!(utc_timestamp(u64::MAX), None);
        assert!(reset_label(Some(3600), 0).contains("Resets in 1h 0m"));
        assert!(reset_label(Some(3600), 3601).contains("refresh usage"));
        assert_eq!(reset_label(None, 0), "Reset time unavailable");
        assert!(is_stale(None, 100));
        assert!(is_stale(Some(200), 100));
        assert!(!is_stale(Some(0), 120));
        assert!(is_stale(Some(0), 121));
    }

    #[test]
    fn model_catalog_controls_reasoning_and_image_capabilities_without_guesses() {
        let mut workspace = Workspace::default();
        workspace.apply_models(&json!({"data":[{"id":"catalog-id","model":"wire-model","displayName":"Model A","isDefault":true,"supportedReasoningEfforts":[{"reasoningEffort":"medium","description":"Balanced"},{"reasoningEffort":"high","description":"Thorough"}],"defaultReasoningEffort":"medium","inputModalities":["text"]},{"id":"image-model","supportedReasoningEfforts":[],"inputModalities":["text","image"]},{"id":"unknown-capabilities","supportedReasoningEfforts":[]},{"id":"hidden-model","hidden":true}]}), 100).unwrap();
        assert_eq!(workspace.models.len(), 4);
        assert_eq!(workspace.selected_model(None).unwrap().model, "wire-model");
        assert!(workspace.validate_selection_with_image(Some("wire-model"), Some("high"), false).is_ok());
        assert!(workspace.validate_selection_with_image(Some("wire-model"), Some("ultra"), false).is_err());
        assert!(workspace.validate_selection_with_image(Some("wire-model"), None, true).is_err());
        assert!(workspace.validate_selection_with_image(Some("image-model"), None, true).is_ok());
        assert_eq!(workspace.selected_model(Some("unknown-capabilities")).unwrap().supports_image_input, None);
        assert!(workspace.validate_selection_with_image(Some("missing-model"), None, false).is_err());
        let old = workspace.models.clone();
        assert!(workspace.apply_models(&json!({"data":[{"id":"bad","supportedReasoningEfforts":[],"defaultReasoningEffort":"imaginary"}]}), 101).is_err());
        assert_eq!(workspace.models, old);
    }

    #[test]
    fn additional_models_are_preserved_and_catalog_age_is_not_server_freshness() {
        let mut workspace = Workspace::default();
        workspace.apply_models(&json!({"data":[
            {"id":"expert","model":"expert-model","displayName":"Expert","hidden":true},
            {"id":"normal","model":"normal-model","isDefault":true}
        ],"nextCursor":null,"catalogInfo":{"source":"runtime_reported","freshness":"not_reported","complete":true,"pageCount":2,"includesHidden":true,"followupQueries":1,"followupSucceeded":true}}), 100).unwrap();
        assert_eq!(workspace.models.len(), 2);
        assert!(workspace.selected_model(Some("expert")).unwrap().hidden);
        assert!(workspace.validate_selection(Some("expert-model"), None).is_ok());
        assert_eq!(workspace.selected_model(None).unwrap().model, "normal-model");
        assert_eq!(workspace.snapshot().pointer("/modelCatalog/page_count"), Some(&json!(2)));
        let label = catalog_observation_label(&workspace);
        assert!(label.contains("1970-01-01 00:01 UTC"));
        assert!(label.contains("freshness is unreported"));
        assert!(label.contains("does not guarantee access"));
        assert!(!label.contains("up to date"));
        let mut old_model = serde_json::to_value(&workspace.models[1]).unwrap();
        old_model.as_object_mut().unwrap().remove("hidden");
        assert!(!serde_json::from_value::<Model>(old_model).unwrap().hidden, "older saved model metadata remains readable");
    }

    #[test]
    fn visible_alias_wins_and_partial_catalogs_never_replace_previous_choices() {
        let mut workspace = Workspace::default();
        workspace
            .apply_models(
                &json!({"data":[
                    {"id":"hidden-alias","model":"same-model","hidden":true},
                    {"id":"normal-alias","model":"same-model","hidden":false,"isDefault":true}
                ]}),
                100,
            )
            .unwrap();
        assert_eq!(workspace.models.len(), 1);
        assert!(!workspace.models[0].hidden);
        assert_eq!(workspace.models[0].id, "normal-alias");
        let previous = workspace.clone();
        for invalid in [
            json!({"data":[],"nextCursor":"unread-page"}),
            json!({"data":[],"catalogInfo":{"complete":false}}),
            json!({"data":[],"catalogInfo":{"pageCount":9}}),
            json!({"data":[],"catalogInfo":{"followupQueries":2}}),
            json!({"data":[{"id":"bad","hidden":"yes"}]}),
        ] {
            assert!(workspace.apply_models(&invalid, 101).is_err());
            assert_eq!(workspace, previous);
        }
    }

    #[test]
    fn account_snapshot_omits_identity_and_rejects_bad_values() {
        let mut workspace = Workspace::default();
        workspace
            .apply_account(&json!({"account":{"type":"chatgpt","planType":"pro","email":"private@example.test","accessToken":"private-secret"}}), 100)
            .unwrap();
        let snapshot = workspace.snapshot().to_string();
        assert!(snapshot.contains("chatgpt"));
        assert!(!snapshot.contains("private"));
        assert!(workspace.apply_account(&json!({"account":42}), 101).is_err());
        assert_eq!(workspace.account.as_ref().unwrap().plan_type.as_deref(), Some("pro"));
        workspace.apply_compaction(Compaction::Failed("Synthetic compaction failure".into()), 101);
        assert_eq!(workspace.snapshot().get("compaction"), Some(&json!({"state":"failed","message":"Synthetic compaction failure"})));
    }

    #[test]
    fn token_history_uses_strict_bounded_schema() {
        let usage =
            TokenUsage { last: TokenCounts { total_tokens: Some(10), ..Default::default() }, total: TokenCounts::default(), model_context_window: Some(100) };
        let encoded = serde_json::to_value(&usage).unwrap();
        assert_eq!(serde_json::from_value::<TokenUsage>(encoded.clone()).unwrap(), usage);
        let mut extra = encoded;
        extra["accessToken"] = json!("secret");
        assert!(serde_json::from_value::<TokenUsage>(extra).is_err());
        assert!(!TokenUsage { model_context_window: Some(0), ..Default::default() }.valid());
    }
}
