use crate::schema::PrRef;
use anyhow::{Context, Result};
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub code: i32,
    pub stdout: String,
    pub stderr: String,
}

pub fn now_iso() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

pub fn make_job_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    format!("av_{}_{}", base36(millis), base36(pid as u128))
}

fn base36(mut value: u128) -> String {
    if value == 0 {
        return "0".to_string();
    }
    let mut out = Vec::new();
    while value > 0 {
        let digit = (value % 36) as u8;
        out.push(match digit {
            0..=9 => b'0' + digit,
            _ => b'a' + digit - 10,
        });
        value /= 36;
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_else(|_| "0".to_string())
}

pub fn truncate(value: impl ToString, length: usize) -> String {
    let text = value
        .to_string()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if text.len() <= length {
        return text;
    }
    let keep = length.saturating_sub(3);
    let mut end = keep.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", text[..end].trim_end())
}

pub fn title_from_prompt(prompt: &str) -> String {
    let cleaned = prompt
        .split_whitespace()
        .filter(|part| !part.starts_with("http://") && !part.starts_with("https://"))
        .collect::<Vec<_>>()
        .join(" ")
        .replace(['#', '*', '_', '`', '>', '[', ']', '(', ')'], "");
    truncate(
        if cleaned.trim().is_empty() {
            "untitled task"
        } else {
            cleaned.trim()
        },
        48,
    )
}

pub fn slugify(value: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in value.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash && !out.is_empty() {
            out.push('-');
            last_dash = true;
        }
        if out.len() >= 36 {
            break;
        }
    }
    let slug = out.trim_matches('-').to_string();
    if slug.is_empty() {
        "task".to_string()
    } else {
        slug
    }
}

pub fn relative_time(iso: &str) -> String {
    let Ok(dt) = chrono::DateTime::parse_from_rfc3339(iso) else {
        return String::new();
    };
    let diff = chrono::Utc::now()
        .signed_duration_since(dt.with_timezone(&chrono::Utc))
        .num_seconds()
        .max(0);
    if diff < 60 {
        format!("{diff}s")
    } else if diff < 3600 {
        format!("{}m", diff / 60)
    } else if diff < 86400 {
        format!("{}h", diff / 3600)
    } else {
        format!("{}d", diff / 86400)
    }
}

pub fn strip_ansi(value: &str) -> String {
    static ANSI: OnceLock<Regex> = OnceLock::new();
    ANSI.get_or_init(|| Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap())
        .replace_all(value, "")
        .to_string()
}

pub fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

pub fn path_exists(path: impl AsRef<Path>) -> bool {
    path.as_ref().exists()
}

pub fn run_command(command: &str, args: &[&str], cwd: Option<&Path>) -> Result<CommandOutput> {
    let mut cmd = Command::new(command);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    let output = cmd
        .output()
        .with_context(|| format!("failed to run {command}"))?;
    Ok(CommandOutput {
        code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

pub fn command_exists(command: &str) -> bool {
    run_command(
        "sh",
        &["-lc", &format!("command -v {}", shell_quote(command))],
        None,
    )
    .map(|output| output.code == 0)
    .unwrap_or(false)
}

pub fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub fn extract_pr_refs(text: &str) -> Vec<PrRef> {
    static PR_RE: OnceLock<Regex> = OnceLock::new();
    let re = PR_RE.get_or_init(|| {
        Regex::new(r"https://github\.com/([A-Za-z0-9_.-]+)/([A-Za-z0-9_.-]+)/pull/(\d+)").unwrap()
    });
    let mut refs = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for cap in re.captures_iter(text) {
        let url = cap.get(0).unwrap().as_str().to_string();
        if !seen.insert(url.clone()) {
            continue;
        }
        refs.push(PrRef {
            url,
            owner: cap[1].to_string(),
            repo: cap[2].to_string(),
            number: cap[3].parse().unwrap_or(0),
            status: "unknown".to_string(),
        });
    }
    refs
}

pub fn merge_pr_refs(existing: &[PrRef], next: &[PrRef]) -> Vec<PrRef> {
    let mut by_url = std::collections::BTreeMap::new();
    for item in existing {
        by_url.insert(item.url.clone(), item.clone());
    }
    for item in next {
        let should_keep_existing = by_url
            .get(&item.url)
            .is_some_and(|existing| existing.status != "unknown" && item.status == "unknown");
        if !should_keep_existing {
            by_url.insert(item.url.clone(), item.clone());
        }
    }
    by_url.into_values().collect()
}

pub fn format_pr_refs(refs: &[PrRef]) -> String {
    refs.iter()
        .map(|pr| format!("{} [{}]", pr.url, pr.status))
        .collect::<Vec<_>>()
        .join(", ")
}

pub fn pr_status_indicator(refs: &[PrRef]) -> Option<String> {
    if refs.is_empty() {
        return None;
    }
    let status = refs
        .iter()
        .map(|pr| pr.status.as_str())
        .find(|status| !status.trim().is_empty() && *status != "unknown")
        .unwrap_or("unknown");
    let prefix = if refs.len() == 1 {
        "pr".to_string()
    } else {
        format!("{}prs", refs.len())
    };
    Some(format!("{prefix}:{status}"))
}

pub fn extract_thread_id(event: &Value) -> Option<String> {
    for key in [
        "threadId",
        "thread_id",
        "conversationId",
        "conversation_id",
        "sessionId",
        "session_id",
        "id",
    ] {
        if let Some(value) = find_string_by_key(event, key) {
            if looks_like_session_id(&value) {
                return Some(value);
            }
        }
    }
    static UUID_RE: OnceLock<Regex> = OnceLock::new();
    let re = UUID_RE.get_or_init(|| {
        Regex::new(r"[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}")
            .unwrap()
    });
    re.find(&event.to_string()).map(|m| m.as_str().to_string())
}

fn looks_like_session_id(value: &str) -> bool {
    static UUID_RE: OnceLock<Regex> = OnceLock::new();
    UUID_RE
        .get_or_init(|| {
            Regex::new(
                r"^[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}$",
            )
            .unwrap()
        })
        .is_match(value)
}

fn find_string_by_key(value: &Value, wanted: &str) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key.eq_ignore_ascii_case(wanted) {
                    if let Some(text) = child.as_str() {
                        if !text.trim().is_empty() {
                            return Some(text.to_string());
                        }
                    }
                }
            }
            for child in map.values() {
                if let Some(found) = find_string_by_key(child, wanted) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(values) => values
            .iter()
            .find_map(|child| find_string_by_key(child, wanted)),
        _ => None,
    }
}

pub fn summarize_event(event: &Value) -> Option<String> {
    if let Some(command) = find_first_string(event, &["command", "cmd"]) {
        return Some(truncate(format!("Run {command}"), 120));
    }
    if let Some(text) = find_first_string(
        event,
        &[
            "delta", "text", "message", "content", "summary", "output", "preview",
        ],
    ) {
        if text.trim().len() > 2 {
            return Some(truncate(strip_ansi(&text), 120));
        }
    }
    find_first_string(event, &["method", "type", "event", "name"]).and_then(|m| {
        if is_low_value_lifecycle_event(&m) {
            None
        } else {
            Some(truncate(m, 120))
        }
    })
}

fn find_first_string(value: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(found) = find_string_by_key(value, key) {
            return Some(found);
        }
    }
    None
}

fn is_low_value_lifecycle_event(value: &str) -> bool {
    matches!(
        value,
        "thread.started" | "turn.started" | "turn.completed" | "item.started"
    )
}

pub fn event_needs_input(event: &Value) -> bool {
    let text = event.to_string().to_lowercase();
    [
        "requestapproval",
        "request_approval",
        "requestuserinput",
        "request_user_input",
        "waitingonapproval",
        "waitingonuserinput",
        "needs_input",
    ]
    .iter()
    .any(|needle| text.contains(needle))
}

pub fn event_failed(event: &Value) -> bool {
    let text = event.to_string().to_lowercase();
    text.contains("\"failed\"") || text.contains("\"error\"")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn helper_extracts_modern_thread_ids_and_prs() {
        let event = json!({
            "type": "thread.started",
            "thread_id": "019e21d5-4369-7010-b2f7-fcc3b2b66ca9",
            "message": "Opened https://github.com/acme/app/pull/42"
        });
        assert_eq!(
            extract_thread_id(&event).as_deref(),
            Some("019e21d5-4369-7010-b2f7-fcc3b2b66ca9")
        );
        assert_eq!(extract_pr_refs(&event.to_string())[0].number, 42);
        assert_eq!(
            summarize_event(&event).as_deref(),
            Some("Opened https://github.com/acme/app/pull/42")
        );
    }

    #[test]
    fn pr_helpers_preserve_known_status_and_format_indicator() {
        let existing = vec![PrRef {
            url: "https://github.com/acme/app/pull/42".to_string(),
            owner: "acme".to_string(),
            repo: "app".to_string(),
            number: 42,
            status: "green".to_string(),
        }];
        let unknown = extract_pr_refs("see https://github.com/acme/app/pull/42");

        let merged = merge_pr_refs(&existing, &unknown);

        assert_eq!(merged[0].status, "green");
        assert_eq!(
            format_pr_refs(&merged),
            "https://github.com/acme/app/pull/42 [green]"
        );
        assert_eq!(pr_status_indicator(&merged).as_deref(), Some("pr:green"));
    }

    #[test]
    fn truncate_uses_ascii_ellipsis() {
        assert_eq!(truncate("abcdefghijklmnopqrstuvwxyz", 10), "abcdefg...");
    }

    #[test]
    fn lifecycle_events_do_not_replace_useful_summaries() {
        let event = json!({ "type": "turn.completed" });
        assert_eq!(summarize_event(&event), None);
    }
}
