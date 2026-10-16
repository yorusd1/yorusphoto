//! Saved embedded Codex conversations, separate from preferences and authentication.
//!
//! Validate before any write, retain the preceding valid snapshot, and use the same flushed
//! temporary-file replacement as document saves. A damaged history is left available for
//! recovery; neither loading nor an unsuccessful save rewrites it.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

const MAX_BYTES: usize = 16 << 20;
const MAX_TEXT_BYTES: usize = 128 << 10;
const FILE_NAME: &str = "codex-chat.json";
const BACKUP_NAME: &str = "codex-chat.previous.json";

pub(super) fn load() -> Result<Option<String>, String> {
    load_at(&history_path()?)
}

pub(super) fn save(text: &str) -> Result<(), String> {
    save_at(&history_path()?, text)
}

fn history_path() -> Result<PathBuf, String> {
    super::config_dir().map(|dir| dir.join(FILE_NAME)).ok_or_else(|| "No configuration directory is available for the Codex conversation.".into())
}

fn validate(text: &str) -> Result<(), String> {
    if text.len() > MAX_BYTES {
        return Err("The saved Codex conversations exceed the 16 MiB limit; the previous history was not changed.".into());
    }
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|_| "The saved Codex conversation is not valid JSON; its existing file was not changed.".to_owned())?;
    let valid = match value.get("version").and_then(serde_json::Value::as_u64) {
        Some(1) => value.as_object().is_some_and(|object| object.len() == 4 && conversation_fields_valid(&value)),
        Some(2) => value.as_object().is_some_and(|object| {
            object.len() == 3
                && object.get("current_id").and_then(serde_json::Value::as_str).is_some_and(|current| {
                    object.get("chats").and_then(serde_json::Value::as_array).is_some_and(|chats| {
                        let mut ids = std::collections::HashSet::new();
                        !chats.is_empty()
                            && chats.len() <= 100
                            && chats.iter().all(|value| {
                                value.as_object().is_some_and(|chat| {
                                    chat.len() == 10
                                        && chat.get("id").and_then(serde_json::Value::as_str).is_some_and(|id| {
                                            !id.is_empty()
                                                && id.len() <= 64
                                                && id.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
                                                && ids.insert(id)
                                        })
                                        && chat.get("title").and_then(serde_json::Value::as_str).is_some_and(|title| {
                                            !title.trim().is_empty() && title.chars().count() <= 80 && !title.chars().any(char::is_control)
                                        })
                                        && chat.get("renamed").is_some_and(serde_json::Value::is_boolean)
                                        && chat.get("archived").is_some_and(serde_json::Value::is_boolean)
                                        && conversation_fields_valid(value)
                                        && chat.get("model").is_some_and(nullable_short)
                                        && chat.get("reasoning").is_some_and(nullable_short)
                                        && chat.get("token_stats").is_some_and(token_stats_valid)
                                })
                            })
                            && ids.contains(current)
                    })
                })
        }),
        _ => false,
    };
    if !valid {
        return Err("The saved Codex conversation has an unsupported or damaged format; its existing file was not changed.".into());
    }
    Ok(())
}

fn nullable_short(value: &serde_json::Value) -> bool {
    value.is_null() || value.as_str().is_some_and(|text| !text.trim().is_empty() && text.len() <= 256 && !text.chars().any(char::is_control))
}

fn conversation_fields_valid(value: &serde_json::Value) -> bool {
    value.get("thread_id").is_some_and(nullable_short)
        && value.get("draft").and_then(serde_json::Value::as_str).is_some_and(|text| text.len() <= MAX_TEXT_BYTES)
        && value.get("messages").and_then(serde_json::Value::as_array).is_some_and(|messages| {
            messages.len() <= 100
                && messages.iter().all(|message| {
                    message.as_object().is_some_and(|message| {
                        message.len() == 2
                            && message.get("role").and_then(serde_json::Value::as_str).is_some_and(|role| matches!(role, "You" | "Codex"))
                            && message.get("text").and_then(serde_json::Value::as_str).is_some_and(|text| text.len() <= MAX_TEXT_BYTES)
                    })
                })
        })
}

fn token_stats_valid(value: &serde_json::Value) -> bool {
    value.is_null()
        || value.as_object().is_some_and(|stats| {
            stats.keys().all(|key| matches!(key.as_str(), "last" | "total" | "model_context_window"))
                && stats.get("last").is_some_and(serde_json::Value::is_object)
                && stats.get("total").is_some_and(serde_json::Value::is_object)
                && stats.iter().all(|(key, value)| {
                    if key == "model_context_window" {
                        value.is_null() || value.as_u64().is_some_and(|count| count > 0 && count <= 1_000_000_000_000)
                    } else {
                        value.as_object().is_some_and(|counts| {
                            counts.keys().all(|key| {
                                matches!(
                                    key.as_str(),
                                    "total_tokens"
                                        | "input_tokens"
                                        | "cached_input_tokens"
                                        | "output_tokens"
                                        | "reasoning_output_tokens"
                                        | "cache_write_input_tokens"
                                )
                            }) && counts.values().all(|value| value.is_null() || value.as_u64().is_some_and(|count| count <= 1_000_000_000_000))
                        })
                    }
                })
        })
}

fn load_at(path: &Path) -> Result<Option<String>, String> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("Could not read the saved Codex conversation: {error}")),
    };
    let metadata = file.metadata().map_err(|error| format!("Could not inspect the saved Codex conversation: {error}"))?;
    if !metadata.is_file() {
        return Err("The saved Codex conversation path is not a regular file; it was not changed.".into());
    }
    if metadata.len() > MAX_BYTES as u64 {
        return Err("The saved Codex conversations exceed the 16 MiB limit; their existing file was not changed.".into());
    }
    // Also cap the actual read: another process can grow the file after metadata was read.
    let mut bytes = Vec::new();
    (&mut file).take((MAX_BYTES + 1) as u64).read_to_end(&mut bytes).map_err(|error| format!("Could not read the saved Codex conversation: {error}"))?;
    if bytes.len() > MAX_BYTES {
        return Err("The saved Codex conversations exceed the 16 MiB limit; their existing file was not changed.".into());
    }
    let text = String::from_utf8(bytes).map_err(|_| "The saved Codex conversation is not UTF-8; its existing file was not changed.".to_owned())?;
    validate(&text)?;
    Ok(Some(text))
}

fn save_at(path: &Path, text: &str) -> Result<(), String> {
    save_with(path, text, super::write_atomic)
}

fn save_with(path: &Path, text: &str, write: impl Fn(&Path, &[u8]) -> Result<(), String>) -> Result<(), String> {
    validate(text)?;
    if let Some(previous) = load_at(path)? {
        if previous == text {
            return Ok(());
        }
        // Copy, never rename away, the old primary. If either atomic write fails the old
        // primary remains intact; a locked backup also prevents replacing the only snapshot.
        write(&path.with_file_name(BACKUP_NAME), previous.as_bytes())
            .map_err(|error| format!("Could not preserve the previous Codex conversation: {error}"))?;
    }
    write(path, text.as_bytes()).map_err(|error| format!("Could not save the Codex conversation: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Temp(PathBuf);

    impl Temp {
        fn new(tag: &str) -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!("yorusphoto-codex-history-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn file(&self) -> PathBuf {
            self.0.join(FILE_NAME)
        }
    }

    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn state(thread: &str) -> String {
        serde_json::json!({"version":1,"thread_id":thread,"messages":[{"role":"You","text":"Add a blue layer"}],"draft":""}).to_string()
    }

    fn multichat() -> serde_json::Value {
        serde_json::json!({"version":2,"current_id":"chat-1","chats":[{
            "id":"chat-1","title":"Blue layer","renamed":false,"archived":false,"thread_id":"thread-one",
            "messages":[{"role":"You","text":"Add a blue layer"}],"draft":"Later",
            "model":"model-one","reasoning":"high","token_stats":{
                "last":{"total_tokens":42},"total":{"input_tokens":40,"output_tokens":2},"model_context_window":1000
            }
        }]})
    }

    #[test]
    fn migration_to_multiple_chats_keeps_the_legacy_file_as_backup() {
        let dir = Temp::new("migration");
        let legacy = state("thread-one");
        save_at(&dir.file(), &legacy).unwrap();
        let mut new = multichat();
        let mut second = new["chats"][0].clone();
        second["id"] = "chat-2".into();
        second["thread_id"] = "thread-two".into();
        second["archived"] = true.into();
        new["chats"].as_array_mut().unwrap().push(second);
        let new = new.to_string();
        save_at(&dir.file(), &new).unwrap();
        assert_eq!(load_at(&dir.file()).unwrap().as_deref(), Some(new.as_str()));
        assert_eq!(std::fs::read_to_string(dir.0.join(BACKUP_NAME)).unwrap(), legacy);
    }

    #[test]
    fn invalid_multichat_or_token_statistics_never_replace_existing_history() {
        let dir = Temp::new("invalid-multichat");
        let original = state("thread-one");
        save_at(&dir.file(), &original).unwrap();
        let good = multichat();
        let mut bad_values = Vec::new();
        let mut missing_active = good.clone();
        missing_active["current_id"] = "missing".into();
        bad_values.push(missing_active);
        let mut duplicate = good.clone();
        let chat = duplicate["chats"][0].clone();
        duplicate["chats"].as_array_mut().unwrap().push(chat);
        bad_values.push(duplicate);
        let mut extra = good.clone();
        extra["chats"][0]["access_token"] = "never-persist".into();
        bad_values.push(extra);
        let mut context = good.clone();
        context["chats"][0]["token_stats"]["model_context_window"] = 0.into();
        bad_values.push(context);
        let mut huge_count = good.clone();
        huge_count["chats"][0]["token_stats"]["last"]["total_tokens"] = (1_000_000_000_000_u64 + 1).into();
        bad_values.push(huge_count);
        let mut credits = good.clone();
        credits["chats"][0]["token_stats"]["credits"] = serde_json::json!({"balance":"42"});
        bad_values.push(credits);
        let mut long_message = good.clone();
        long_message["chats"][0]["messages"][0]["text"] = "x".repeat(MAX_TEXT_BYTES + 1).into();
        bad_values.push(long_message);
        for bad in bad_values {
            assert!(save_at(&dir.file(), &bad.to_string()).is_err(), "accepted {bad}");
            assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), original);
            assert!(!dir.0.join(BACKUP_NAME).exists());
        }
    }

    #[test]
    fn missing_history_round_trips_and_keeps_the_previous_snapshot() {
        let dir = Temp::new("roundtrip");
        assert_eq!(load_at(&dir.file()).unwrap(), None);
        let first = state("thread-a");
        let second = state("thread-b");
        save_at(&dir.file(), &first).unwrap();
        assert_eq!(load_at(&dir.file()).unwrap(), Some(first.clone()));
        assert!(!dir.0.join(BACKUP_NAME).exists());
        save_at(&dir.file(), &second).unwrap();
        assert_eq!(load_at(&dir.file()).unwrap(), Some(second.clone()));
        assert_eq!(std::fs::read_to_string(dir.0.join(BACKUP_NAME)).unwrap(), first);
        save_at(&dir.file(), &second).unwrap();
        assert_eq!(std::fs::read_to_string(dir.0.join(BACKUP_NAME)).unwrap(), first, "an unchanged save does not replace the backup");
    }

    #[test]
    fn invalid_and_oversized_new_history_leave_the_previous_file_unchanged() {
        let dir = Temp::new("invalid-new");
        let original = state("thread-a");
        save_at(&dir.file(), &original).unwrap();
        let oversized = format!("{{\"text\":\"{}\"}}", "x".repeat(MAX_BYTES));
        for text in [
            "{",
            "[]",
            "null",
            r#"{"version":2,"thread_id":null,"messages":[],"draft":""}"#,
            r#"{"version":1,"thread_id":null,"messages":{},"draft":""}"#,
            r#"{"version":1,"thread_id":null,"messages":[],"draft":"","access_token":"must not persist"}"#,
            &oversized,
        ] {
            assert!(save_at(&dir.file(), text).is_err());
            assert_eq!(std::fs::read_to_string(dir.file()).unwrap(), original);
            assert!(!dir.0.join(BACKUP_NAME).exists());
        }
    }

    #[test]
    fn corrupt_or_oversized_existing_history_is_reported_and_never_overwritten() {
        let dir = Temp::new("invalid-existing");
        let oversized = vec![b'x'; MAX_BYTES + 1];
        for bytes in [&b"{not-json"[..], &b"\xff\xfe"[..], &oversized] {
            std::fs::write(dir.file(), bytes).unwrap();
            assert!(load_at(&dir.file()).is_err());
            assert!(save_at(&dir.file(), &state("thread-new")).is_err());
            assert_eq!(std::fs::read(dir.file()).unwrap(), bytes);
            assert!(!dir.0.join(BACKUP_NAME).exists());
        }
    }

    #[test]
    fn backup_or_primary_write_errors_keep_the_original_history() {
        let dir = Temp::new("write-errors");
        let original = state("thread-a");
        save_at(&dir.file(), &original).unwrap();
        let next = state("thread-b");
        assert!(save_with(&dir.file(), &next, |_, _| Err("simulated full disk".into())).is_err());
        assert_eq!(load_at(&dir.file()).unwrap(), Some(original.clone()));
        assert!(
            save_with(&dir.file(), &next, |path, bytes| {
                if path.file_name().is_some_and(|name| name == FILE_NAME) {
                    Err("simulated locked primary".into())
                } else {
                    super::super::write_atomic(path, bytes)
                }
            })
            .is_err()
        );
        assert_eq!(load_at(&dir.file()).unwrap(), Some(original.clone()));
        assert_eq!(std::fs::read_to_string(dir.0.join(BACKUP_NAME)).unwrap(), original);
    }

    #[test]
    fn unreadable_path_is_an_error_instead_of_a_missing_conversation() {
        let dir = Temp::new("directory");
        std::fs::create_dir(dir.file()).unwrap();
        assert!(load_at(&dir.file()).is_err());
        assert!(save_at(&dir.file(), &state("thread-new")).is_err());
        assert!(dir.file().is_dir());
    }
}
