//! Claude Code hook: reads the hook event JSON on stdin and appends one small
//! record to Toranj's spool. It must never slow down or break Claude Code, so
//! every failure is silent and the exit code is always 0 with no stdout.
//! Kept free of GUI dependencies so it starts in a few milliseconds.

use std::io::Read;
use toranj_spool::{Record, append, redact, valid_session_id, valid_tab};

const MAX_STDIN: u64 = 4 * 1024 * 1024;

fn main() {
    if std::env::args().nth(1).as_deref() == Some("statusline") {
        let _ = statusline();
    } else {
        let _ = run();
    }
}

/// Claude Code status line: the project's name as Toranj shows it, in its
/// color, for sessions Toranj knows (its tabs, or a project folder it has
/// open). Prints nothing anywhere else.
fn statusline() -> Option<()> {
    let mut input = String::new();
    std::io::stdin().take(MAX_STDIN).read_to_string(&mut input).ok()?;
    let v: serde_json::Value = serde_json::from_str(&input).unwrap_or_default();
    let text = std::fs::read_to_string(toranj_spool::labels_path()).ok()?;
    let labels: serde_json::Value = serde_json::from_str(&text).ok()?;
    let tab = std::env::var("TORANJ_TAB").unwrap_or_default();
    let by_tab = labels.get("tabs").and_then(|t| t.get(tab.as_str())).filter(|_| !tab.is_empty());
    let cwd = v
        .get("workspace")
        .and_then(|w| w.get("current_dir"))
        .or_else(|| v.get("cwd"))
        .and_then(|c| c.as_str())
        .unwrap_or_default();
    let by_dir = || {
        let projects = labels.get("projects")?.as_object()?;
        projects
            .iter()
            .filter(|(p, _)| !cwd.is_empty() && (cwd == p.as_str() || cwd.starts_with(&format!("{p}/"))))
            .max_by_key(|(p, _)| p.len())
            .map(|(_, l)| l)
    };
    let l = by_tab.or_else(by_dir)?;
    let label: String = l.get("label")?.as_str()?.chars().filter(|c| !c.is_control()).take(80).collect();
    if label.is_empty() {
        return None;
    }
    let (r, g, b) = l
        .get("color")
        .and_then(|c| c.as_str())
        .and_then(|c| {
            let h = c.strip_prefix('#')?;
            (h.len() == 6 && h.is_ascii()).then_some(())?;
            Some((
                u8::from_str_radix(&h[0..2], 16).ok()?,
                u8::from_str_radix(&h[2..4], 16).ok()?,
                u8::from_str_radix(&h[4..6], 16).ok()?,
            ))
        })
        .unwrap_or((110, 150, 220));
    print!("\x1b[38;2;{r};{g};{b}m■\x1b[0m \x1b[1;38;2;{r};{g};{b}m{label}\x1b[0m");
    Some(())
}

fn s(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(|x| x.as_str())
        .unwrap_or_default()
        .to_string()
}

fn detail(event: &str, v: &serde_json::Value) -> String {
    match event {
        "PermissionRequest" => {
            let tool = redact(&s(v, "tool_name"), 40);
            // First line of a shell command, path or URL, redacted and short:
            // enough for "waiting for: Bash: git push", never file contents.
            let what = v
                .get("tool_input")
                .and_then(|i| {
                    i.get("command")
                        .or_else(|| i.get("file_path"))
                        .or_else(|| i.get("url"))
                })
                .and_then(|x| x.as_str())
                .map(|c| redact(c, 80))
                .unwrap_or_default();
            if what.is_empty() {
                tool
            } else {
                format!("{tool}: {what}")
            }
        }
        "Notification" | "Elicitation" => redact(&s(v, "message"), 80),
        "StopFailure" => redact(&s(v, "error"), 80),
        _ => String::new(),
    }
}

fn run() -> Option<()> {
    let mut input = String::new();
    std::io::stdin()
        .take(MAX_STDIN)
        .read_to_string(&mut input)
        .ok()?;
    let v: serde_json::Value = serde_json::from_str(&input).ok()?;

    let session_id = s(&v, "session_id");
    if !valid_session_id(&session_id) {
        return None;
    }
    let event = s(&v, "hook_event_name");
    let kind = match event.as_str() {
        "Notification" => s(&v, "notification_type"),
        "SessionStart" => s(&v, "source"),
        "SessionEnd" => s(&v, "reason"),
        _ => String::new(),
    };
    append(&Record {
        ts: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0),
        detail: detail(&event, &v),
        tool: redact(&s(&v, "tool_name"), 64),
        kind: redact(&kind, 64),
        bg: v
            .get("background_tasks")
            .and_then(|b| b.as_array())
            .map(|a| a.len() as u32)
            .unwrap_or(0),
        tab: std::env::var("TORANJ_TAB")
            .ok()
            .filter(|t| valid_tab(t))
            .unwrap_or_default(),
        event: redact(&event, 64),
        session_id,
    })
}
