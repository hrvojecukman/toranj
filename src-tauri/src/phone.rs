//! What the phone (and the desktop's Chat view) needs beyond the terminal
//! itself: a tab's Claude conversation as messages and tool calls (easy to
//! read and copy),
//! the Mac's clipboard (so what an agent copied reaches the phone), and files
//! the phone uploads to attach to a message.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;

/// How much of a transcript's end is read for the latest messages, tried
/// in turn until at least MIN_MESSAGES text messages are found.
/// (A transcript is re-read whenever it grows, every few seconds while
/// Claude works, so the most read stays moderate.)
const TAILS: [u64; 2] = [4 << 20, 16 << 20];
const MIN_MESSAGES: usize = 20;
/// Text messages and tool calls together.
const MAX_ITEMS: usize = 150;
/// A tool call's input and output are cut to this many characters.
const TOOL_TEXT: usize = 1500;
/// A longer clipboard is cut; it's for text, not files.
const CLIP_MAX: usize = 256 << 10;
pub const UPLOAD_MAX: usize = 30 << 20;
/// At most this much is kept in uploads; the oldest go first.
const UPLOAD_TOTAL: u64 = 500 << 20;
/// Uploaded files are removed after this long.
const UPLOAD_KEEP: Duration = Duration::from_secs(3 * 86400);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- conversation ----------

/// Items last read per transcript, with the file length they were read at.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, (u64, Vec<Message>)>>> = LazyLock::new(Default::default);

#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct Message {
    pub id: String,
    /// "user", "assistant" or "tool" (a tool call Claude made).
    pub role: &'static str,
    /// The message; for a tool call, a short title of what it does.
    pub text: String,
    /// ISO 8601, as Claude writes it.
    pub at: String,
    /// Tool calls: the tool's name, its input (e.g. the command) and its
    /// output, `None` while it runs.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub tool: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    /// A message sent while Claude was busy, waiting in Claude Code's queue.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub queued: bool,
}

#[derive(Serialize)]
pub struct Conversation {
    /// The transcript's length: asked again with it, an unchanged
    /// conversation answers without items.
    pub len: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub items: Option<Vec<Message>>,
}

/// The latest messages and tool calls of a Claude conversation, oldest
/// first. Thinking is left out. `known` is the length the caller already
/// has: if the transcript is still that long, no items are sent.
pub fn conversation_since(session_id: &str, cwd: &str, known: u64) -> Result<Conversation, String> {
    if !crate::status::safe_id(session_id) {
        return Err("unknown conversation".into());
    }
    let path = crate::titles::locate(session_id, cwd).ok_or("no transcript yet")?;
    let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if known == len {
        return Ok(Conversation { len, items: None });
    }
    // Asked every few seconds; an unchanged transcript is not read again.
    if let Some((l, m)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&path) {
        if *l == len {
            return Ok(Conversation { len, items: Some(m.clone()) });
        }
    }
    // Long tool output can push messages far back: read more of the end
    // until enough messages are in, or the whole file.
    let mut out = Vec::new();
    for tail in TAILS {
        let (items, whole) = tail_items(&path, tail)?;
        out = items;
        if whole || out.iter().filter(|m| m.role != "tool").count() >= MIN_MESSAGES {
            break;
        }
    }
    if out.len() > MAX_ITEMS {
        out.drain(..out.len() - MAX_ITEMS);
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 16 {
        cache.clear();
    }
    cache.insert(path, (len, out.clone()));
    Ok(Conversation { len, items: Some(out) })
}

/// The desktop's Chat view: the same as the phone's.
#[tauri::command]
pub async fn conversation(session_id: String, cwd: String, known: u64) -> Result<Conversation, String> {
    tauri::async_runtime::spawn_blocking(move || conversation_since(&session_id, &cwd, known))
        .await
        .map_err(|e| e.to_string())?
}

enum Item {
    Msg(Message),
    /// A tool's output, for the call with this id.
    Result(String, String),
    /// Claude Code's queue of messages sent while it was busy: one added
    /// (text, time), the oldest taken, or one taken by its text.
    Enqueue(String, String),
    /// A queued message given to Claude mid-turn.
    Absorbed(Message),
    Dequeue,
    Remove(String),
}

/// The items in the last `tail` bytes, and whether that was the whole file.
fn tail_items(path: &Path, tail: u64) -> Result<(Vec<Message>, bool), String> {
    let mut f = File::open(path).map_err(|e| e.to_string())?;
    let len = f.metadata().map_err(|e| e.to_string())?.len();
    let start = len.saturating_sub(tail);
    f.seek(SeekFrom::Start(start)).map_err(|e| e.to_string())?;
    let mut buf = Vec::with_capacity((len - start) as usize);
    f.read_to_end(&mut buf).map_err(|e| e.to_string())?;
    // Started mid-line: the first (partial) line is skipped.
    let body = if start > 0 {
        buf.iter().position(|&b| b == b'\n').map(|i| &buf[i + 1..]).unwrap_or(&[])
    } else {
        &buf[..]
    };
    let mut out: Vec<Message> = Vec::new();
    let mut calls: HashMap<String, usize> = HashMap::new();
    let mut queue: Vec<(String, String)> = Vec::new();
    for item in body.split(|&b| b == b'\n').flat_map(parse_line) {
        match item {
            Item::Msg(m) => {
                if m.role == "tool" {
                    calls.insert(m.id.clone(), out.len());
                } else if m.role == "user" {
                    // A prompt is sent only once the queue is empty.
                    queue.clear();
                }
                out.push(m);
            }
            Item::Result(id, text) => {
                if let Some(&i) = calls.get(&id) {
                    out[i].result = Some(text);
                }
            }
            Item::Enqueue(text, at) => queue.push((text, at)),
            Item::Absorbed(m) => {
                queue.retain(|(t, _)| t.trim() != m.text);
                out.push(m);
            }
            Item::Dequeue => {
                if !queue.is_empty() {
                    queue.remove(0);
                }
            }
            Item::Remove(text) => {
                if let Some(i) = queue.iter().position(|(t, _)| *t == text) {
                    queue.remove(i);
                }
            }
        }
    }
    for (n, (text, at)) in queue.into_iter().enumerate() {
        if is_prompt(text.trim()) && !text.trim().is_empty() {
            out.push(Message { id: format!("queued:{n}:{at}"), role: "user", text: text.trim().to_string(), at, tool: String::new(), detail: String::new(), result: None, queued: true });
        }
    }
    Ok((out, start == 0))
}

fn clip(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

/// What a tool call does, in a few words.
fn tool_title(name: &str, input: &serde_json::Value) -> String {
    let s = |k: &str| input.get(k).and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    let file = |k: &str| {
        let p = s(k);
        p.rsplit('/').next().unwrap_or(&p).to_string()
    };
    let t = match name {
        "Bash" | "Monitor" => {
            let d = s("description");
            if d.is_empty() { s("command").lines().next().unwrap_or("").to_string() } else { d }
        }
        "Read" | "Write" | "Edit" | "MultiEdit" => format!("{name} {}", file("file_path")),
        "NotebookEdit" => format!("Edit {}", file("notebook_path")),
        "Grep" => format!("Search for “{}”", s("pattern")),
        "Glob" => format!("Find {}", s("pattern")),
        "Task" | "Agent" => s("description"),
        "WebFetch" => format!("Fetch {}", s("url")),
        "WebSearch" => format!("Search the web for “{}”", s("query")),
        "TodoWrite" => "Update the to-do list".into(),
        _ => ["description", "title", "action", "query", "url", "path", "name"]
            .iter()
            .map(|k| s(k))
            .find(|v| !v.is_empty())
            .unwrap_or_default(),
    };
    clip(if t.is_empty() { name } else { &t }, 140)
}

/// A tool's name as shown: `mcp__server__tool` becomes `server: tool`.
fn tool_name(name: &str) -> String {
    match name.strip_prefix("mcp__").and_then(|r| r.split_once("__")) {
        Some((server, tool)) => format!("{}: {tool}", server.trim_start_matches("claude_ai_").replace('_', " ")),
        None => name.to_string(),
    }
}

fn tool_detail(name: &str, input: &serde_json::Value) -> String {
    let d = match (name, input.get("command").and_then(|v| v.as_str())) {
        ("Bash" | "Monitor", Some(cmd)) => cmd.to_string(),
        _ => serde_json::to_string_pretty(input).unwrap_or_default(),
    };
    clip(&d, TOOL_TEXT)
}

fn result_text(content: Option<&serde_json::Value>) -> String {
    let text = match content {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(serde_json::Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| match p.get("type").and_then(|t| t.as_str()) {
                Some("text") => p.get("text").and_then(|t| t.as_str()).map(str::to_string),
                Some("image") => Some("[image]".into()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    };
    clip(text.trim(), TOOL_TEXT)
}

fn parse_line(line: &[u8]) -> Vec<Item> {
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else { return vec![] };
    let flag = |k: &str| v.get(k).and_then(|x| x.as_bool()) == Some(true);
    let at = v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let uuid = v.get("uuid").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let str_of = |x: &serde_json::Value, k: &str| x.get(k).and_then(|t| t.as_str()).unwrap_or("").to_string();
    let role = match v.get("type").and_then(|t| t.as_str()) {
        Some("user") => "user",
        Some("assistant") => "assistant",
        Some("queue-operation") => {
            return match str_of(&v, "operation").as_str() {
                "enqueue" => vec![Item::Enqueue(str_of(&v, "content"), at)],
                "dequeue" => vec![Item::Dequeue],
                "remove" => vec![Item::Remove(str_of(&v, "content"))],
                _ => vec![],
            };
        }
        // A message sent while Claude was working, given to it mid-turn.
        Some("attachment") if !flag("isSidechain") => {
            let Some(a) = v.get("attachment") else { return vec![] };
            let text = str_of(a, "prompt");
            let text = text.trim();
            let human = a.get("origin").and_then(|o| o.get("kind")).and_then(|k| k.as_str()).is_none_or(|k| k == "human");
            if str_of(a, "type") != "queued_command" || str_of(a, "commandMode") != "prompt" || !human || text.is_empty() || !is_prompt(text) {
                return vec![];
            }
            return vec![Item::Absorbed(Message { id: uuid, role: "user", text: text.to_string(), at, tool: String::new(), detail: String::new(), result: None, queued: false })];
        }
        _ => return vec![],
    };
    if flag("isSidechain") {
        return vec![];
    }
    let Some(content) = v.get("message").and_then(|m| m.get("content")) else { return vec![] };
    let msg = |role: &'static str, id: String, text: String| Message { id, role, text, at: at.clone(), tool: String::new(), detail: String::new(), result: None, queued: false };
    let mut out = Vec::new();
    let mut texts = Vec::new();
    match content {
        serde_json::Value::String(s) => texts.push(s.as_str()),
        serde_json::Value::Array(parts) => {
            for p in parts {
                match p.get("type").and_then(|t| t.as_str()) {
                    Some("text") => texts.extend(p.get("text").and_then(|t| t.as_str())),
                    Some("tool_use") if role == "assistant" => {
                        let name = p.get("name").and_then(|t| t.as_str()).unwrap_or("tool");
                        let input = p.get("input").cloned().unwrap_or_default();
                        let mut m = msg("tool", p.get("id").and_then(|t| t.as_str()).unwrap_or("").to_string(), tool_title(name, &input));
                        m.tool = tool_name(name);
                        m.detail = tool_detail(name, &input);
                        out.push(Item::Msg(m));
                    }
                    Some("tool_result") => {
                        let id = p.get("tool_use_id").and_then(|t| t.as_str()).unwrap_or("").to_string();
                        out.push(Item::Result(id, result_text(p.get("content"))));
                    }
                    _ => {}
                }
            }
        }
        _ => {}
    }
    let text = texts.join("\n\n");
    let text = text.trim();
    if !text.is_empty() && !flag("isMeta") && (role == "assistant" || is_prompt(text)) {
        out.insert(0, Item::Msg(msg(role, uuid, text.to_string())));
    }
    out
}

/// What the user typed, not what Claude Code adds on their behalf (slash
/// command records, `!` shell output, interruptions, reminders).
fn is_prompt(text: &str) -> bool {
    const ADDED: [&str; 11] = [
        "<command-", "<local-command", "<bash-", "<system-reminder", "<task-notification", "<user-prompt-submit-hook",
        "<user-memory", "[Request interrupted", "Caveat:", "[Image: source:", "<persisted-output",
    ];
    !ADDED.iter().any(|p| text.starts_with(p))
}

// ---------- clipboard ----------

#[derive(Serialize)]
pub struct Clip {
    pub text: String,
    /// "mac" (the Mac's clipboard) or "terminal" (a tmux copy).
    pub from: &'static str,
    /// Changes whenever the text does.
    pub id: String,
    /// Seconds since it was copied; large when not known (copied before
    /// Toranj started). The phone offers only fresh copies on its own.
    pub age: u64,
}

/// When the Mac's clipboard last changed, unix seconds (0: not since
/// Toranj started), kept by `watch_clipboard`.
static MAC_CHANGED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Notes when the Mac's clipboard changes, from its change count (cheap,
/// no contents read), so a copy's age is known even while no phone looks.
pub fn watch_clipboard() {
    static STARTED: std::sync::Once = std::sync::Once::new();
    STARTED.call_once(|| {
        std::thread::spawn(|| {
            // A plain thread has no autorelease pool: give each look its own.
            let count = || objc2::rc::autoreleasepool(|_| objc2_app_kit::NSPasteboard::generalPasteboard().changeCount());
            let mut last = count();
            loop {
                std::thread::sleep(Duration::from_secs(2));
                let c = count();
                if c != last {
                    last = c;
                    MAC_CHANGED.store(now(), std::sync::atomic::Ordering::Relaxed);
                }
            }
        });
    });
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish()
}

/// Password managers mark what they copy as concealed or transient
/// (nspasteboard.org); such a copy never goes to the phone.
fn concealed() -> bool {
    use objc2_app_kit::NSPasteboard;
    let pb = NSPasteboard::generalPasteboard();
    let Some(types) = pb.types() else { return false };
    types.iter().any(|t| {
        let t = t.to_string();
        t == "org.nspasteboard.ConcealedType" || t == "org.nspasteboard.TransientType" || t == "org.nspasteboard.AutoGeneratedType"
    })
}

fn mac_clipboard() -> String {
    if concealed() {
        return String::new();
    }
    std::process::Command::new("/usr/bin/pbpaste")
        .env("LANG", "en_US.UTF-8")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn cut(mut s: String) -> String {
    if s.len() > CLIP_MAX {
        let mut end = CLIP_MAX;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
    }
    s
}

/// The newest copy on the Mac: its clipboard (an agent's `pbcopy`, or
/// anything copied in Toranj) or, if newer, a tmux copy that never reached
/// the clipboard (an agent's OSC 52 copy while Toranj's window was closed).
pub fn clipboard() -> Clip {
    watch_clipboard();
    let mac = cut(mac_clipboard());
    let h = hash(&mac);
    let mac_at = MAC_CHANGED.load(std::sync::atomic::Ordering::Relaxed);
    let t = now();
    if let Some((at, text)) = crate::tmux::newest_buffer() {
        if at > mac_at && !text.trim().is_empty() && text != mac {
            let text = cut(text);
            let id = format!("t{:x}", hash(&text));
            return Clip { text, from: "terminal", id, age: t.saturating_sub(at) };
        }
    }
    let age = if mac_at == 0 { u64::MAX } else { t.saturating_sub(mac_at) };
    Clip { text: mac, from: "mac", id: format!("m{h:x}"), age }
}

// ---------- uploads ----------

fn upload_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("Toranj").join("uploads")
}

/// A file name safe for a path pasted into a terminal: letters, digits and
/// `._-` only, never hidden, with its extension kept.
fn safe_name(name: &str) -> String {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    let mut s: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_' { c } else { '-' })
        .collect();
    s = s.trim_start_matches(['.', '-']).to_string();
    if s.len() > 80 {
        let ext = s.rsplit_once('.').map(|(_, e)| e.to_string()).filter(|e| e.len() <= 8).unwrap_or_default();
        s.truncate(70);
        if !ext.is_empty() {
            s = format!("{s}.{ext}");
        }
    }
    if s.is_empty() || s == "." {
        "file".into()
    } else {
        s
    }
}

/// Saves an upload and returns its absolute path. Old uploads are removed.
pub fn save_upload(name: &str, bytes: &[u8], tag: &str) -> Result<String, String> {
    if bytes.is_empty() || bytes.len() > UPLOAD_MAX {
        return Err("file too large or empty".into());
    }
    let dir = upload_dir();
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    sweep(&dir);
    let path = dir.join(format!("{tag}-{}", safe_name(name)));
    std::fs::write(&path, bytes).map_err(|e| e.to_string())?;
    Ok(path.to_string_lossy().into_owned())
}

/// Removes uploads older than UPLOAD_KEEP, then the oldest ones until the
/// folder holds at most UPLOAD_TOTAL (room for one more upload).
fn sweep(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut files: Vec<(SystemTime, u64, PathBuf)> = rd
        .flatten()
        .filter_map(|e| {
            let m = e.metadata().ok()?;
            Some((m.modified().ok()?, m.len(), e.path()))
        })
        .collect();
    files.sort_by_key(|f| f.0);
    let mut total: u64 = files.iter().map(|f| f.1).sum();
    for (at, len, path) in files {
        let old = at.elapsed().is_ok_and(|a| a > UPLOAD_KEEP);
        if old || total + UPLOAD_MAX as u64 > UPLOAD_TOTAL {
            if std::fs::remove_file(&path).is_ok() {
                total -= len;
            }
        }
    }
}

/// A path the phone may attach: one of its own uploads that still exists.
pub fn upload_ok(path: &str) -> bool {
    let p = Path::new(path);
    let dir = upload_dir();
    p.parent() == Some(dir.as_path())
        && p.file_name().and_then(|n| n.to_str()).is_some_and(|n| {
            !n.starts_with(['.', '-']) && n.bytes().all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        })
        && p.is_file()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_safe_to_paste() {
        assert_eq!(safe_name("IMG 0042.HEIC"), "IMG-0042.HEIC");
        assert_eq!(safe_name("../../etc/passwd"), "passwd");
        assert_eq!(safe_name(".hidden"), "hidden");
        assert_eq!(safe_name("x$(rm -rf ~).png"), "x--rm--rf---.png");
        assert_eq!(safe_name(""), "file");
        assert!(safe_name(&format!("{}.jpeg", "a".repeat(200))).ends_with(".jpeg"));
    }

    #[test]
    fn only_own_uploads_attach() {
        let long = save_upload(&format!("{}.png", "x".repeat(120)), b"hi", "test").unwrap();
        assert!(upload_ok(&long));
        std::fs::remove_file(&long).unwrap();
        let p = save_upload("a b.txt", b"hi", "test").unwrap();
        assert!(p.ends_with("/test-a-b.txt"));
        assert!(upload_ok(&p));
        assert!(!upload_ok("/etc/passwd"));
        assert!(!upload_ok(&format!("{}/../x", upload_dir().display())));
        std::fs::remove_file(&p).unwrap();
        assert!(!upload_ok(&p));
    }

    #[test]
    fn keeps_typed_prompts_and_replies() {
        let one = |l: &[u8]| parse_line(l).into_iter().find_map(|i| if let Item::Msg(m) = i { Some(m) } else { None });
        let user = br#"{"parentUuid":"a","type":"user","uuid":"u1","timestamp":"t","message":{"role":"user","content":"fix the bug"}}"#;
        assert_eq!(one(user).unwrap().text, "fix the bug");
        let reply = br#"{"parentUuid":"a","type":"assistant","uuid":"u2","timestamp":"t","message":{"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Done."}]}}"#;
        let m = one(reply).unwrap();
        assert_eq!((m.role, m.text.as_str()), ("assistant", "Done."));
        let tool = br#"{"parentUuid":"a","type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#;
        assert!(one(tool).is_none());
        let cmd = br#"{"parentUuid":"a","type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#;
        assert!(one(cmd).is_none());
        let html = br#"{"parentUuid":"a","type":"user","message":{"content":"<div> is off by one"}}"#;
        assert!(one(html).is_some());
        let meta = br#"{"parentUuid":"a","type":"user","isMeta":true,"message":{"content":"hi"}}"#;
        assert!(one(meta).is_none());
        let side = br#"{"parentUuid":"a","type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"x"}]}}"#;
        assert!(one(side).is_none());
        let call = br#"{"type":"assistant","uuid":"u3","message":{"content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"npm test","description":"Run the tests"}}]}}"#;
        let m = one(call).unwrap();
        assert_eq!((m.role, m.text.as_str(), m.tool.as_str(), m.detail.as_str()), ("tool", "Run the tests", "Bash", "npm test"));
        let res = br#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"t1","content":"ok"}]}}"#;
        assert!(matches!(parse_line(res).as_slice(), [Item::Result(id, t)] if id == "t1" && t == "ok"));
        assert_eq!(tool_name("mcp__claude_ai_Slack__slack_read_thread"), "Slack: slack_read_thread");
    }

    #[test]
    fn messages_sent_while_busy_show() {
        let dir = std::env::temp_dir().join(format!("toranj-queue-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.jsonl");
        let lines = [
            r#"{"type":"user","message":{"role":"user","content":"start"},"uuid":"u1","timestamp":"t0"}"#,
            r#"{"type":"queue-operation","operation":"enqueue","timestamp":"t1","content":"cek, a staging?"}"#,
            r#"{"type":"queue-operation","operation":"enqueue","timestamp":"t2","content":"<task-notification>x</task-notification>"}"#,
            r#"{"type":"queue-operation","operation":"enqueue","timestamp":"t3","content":"i jos nesto"}"#,
        ];
        let run = |l: &[&str]| {
            std::fs::write(&path, l.join("\n")).unwrap();
            tail_items(&path, 1 << 20).unwrap().0.into_iter().map(|m| (m.text, m.queued)).collect::<Vec<_>>()
        };
        let s = |t: &str, q: bool| (t.to_string(), q);
        assert_eq!(run(&lines), [s("start", false), s("cek, a staging?", true), s("i jos nesto", true)]);
        // Absorbed mid-turn: an attachment, no longer queued.
        let mut l = lines.to_vec();
        l.push(r#"{"type":"queue-operation","operation":"remove","timestamp":"t4","content":"cek, a staging?","reason":"absorbed_mid_turn"}"#);
        l.push(r#"{"type":"attachment","attachment":{"type":"queued_command","prompt":"cek, a staging?","commandMode":"prompt","origin":{"kind":"human"}},"uuid":"a1","timestamp":"t4"}"#);
        assert_eq!(run(&l), [s("start", false), s("cek, a staging?", false), s("i jos nesto", true)]);
        // The turn ended: the rest is sent as a prompt.
        l.push(r#"{"type":"queue-operation","operation":"dequeue","timestamp":"t5"}"#);
        l.push(r#"{"type":"queue-operation","operation":"dequeue","timestamp":"t5"}"#);
        l.push(r#"{"type":"user","message":{"role":"user","content":"i jos nesto"},"uuid":"u2","timestamp":"t5"}"#);
        assert_eq!(run(&l), [s("start", false), s("cek, a staging?", false), s("i jos nesto", false)]);
        std::fs::remove_dir_all(&dir).ok();
    }
}
