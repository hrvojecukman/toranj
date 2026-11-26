//! What the phone needs beyond the terminal itself: a tab's Claude
//! conversation as plain messages (easy to read and copy on a small screen),
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
/// in turn until at least MIN_MESSAGES are found.
const TAILS: [u64; 3] = [4 << 20, 16 << 20, 48 << 20];
const MIN_MESSAGES: usize = 20;
const MAX_MESSAGES: usize = 60;
/// A longer clipboard is cut; it's for text, not files.
const CLIP_MAX: usize = 256 << 10;
pub const UPLOAD_MAX: usize = 30 << 20;
/// Uploaded files are removed after this long.
const UPLOAD_KEEP: Duration = Duration::from_secs(3 * 86400);

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------- conversation ----------

/// Messages last read per transcript, with the file length they were read at.
static CACHE: LazyLock<Mutex<HashMap<PathBuf, (u64, Vec<Message>)>>> = LazyLock::new(Default::default);

#[derive(Serialize, Debug, PartialEq, Clone)]
pub struct Message {
    pub id: String,
    /// "user" or "assistant".
    pub role: &'static str,
    pub text: String,
    /// ISO 8601, as Claude writes it.
    pub at: String,
}

/// The latest user and assistant text messages of a Claude conversation,
/// oldest first. Tool calls, tool output and thinking are left out.
pub fn messages(session_id: &str, cwd: &str) -> Result<Vec<Message>, String> {
    if !crate::status::safe_id(session_id) {
        return Err("unknown conversation".into());
    }
    let path = crate::titles::locate(session_id, cwd).ok_or("no transcript yet")?;
    // The phone asks every few seconds; an unchanged transcript is not read again.
    let len = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
    if let Some((l, m)) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).get(&path) {
        if *l == len {
            return Ok(m.clone());
        }
    }
    // Long tool output can push messages far back: read more of the end
    // until enough messages are in, or the whole file.
    let mut out = Vec::new();
    for tail in TAILS {
        let (msgs, whole) = tail_messages(&path, tail)?;
        out = msgs;
        if whole || out.len() >= MIN_MESSAGES {
            break;
        }
    }
    if out.len() > MAX_MESSAGES {
        out.drain(..out.len() - MAX_MESSAGES);
    }
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.len() > 16 {
        cache.clear();
    }
    cache.insert(path, (len, out.clone()));
    Ok(out)
}

/// The messages in the last `tail` bytes, and whether that was the whole file.
fn tail_messages(path: &Path, tail: u64) -> Result<(Vec<Message>, bool), String> {
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
    Ok((body.split(|&b| b == b'\n').filter_map(parse_line).collect(), start == 0))
}

fn parse_line(line: &[u8]) -> Option<Message> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    let role = match v.get("type")?.as_str()? {
        "user" => "user",
        "assistant" => "assistant",
        _ => return None,
    };
    if v.get("isSidechain").and_then(|x| x.as_bool()) == Some(true) || v.get("isMeta").and_then(|x| x.as_bool()) == Some(true) {
        return None;
    }
    let content = v.get("message")?.get("content")?;
    let text = match content {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(parts) => parts
            .iter()
            .filter(|p| p.get("type").and_then(|t| t.as_str()) == Some("text"))
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("\n\n"),
        _ => return None,
    };
    let text = text.trim();
    if text.is_empty() || (role == "user" && !is_prompt(text)) {
        return None;
    }
    Some(Message {
        id: v.get("uuid").and_then(|x| x.as_str()).unwrap_or("").to_string(),
        role,
        text: text.to_string(),
        at: v.get("timestamp").and_then(|x| x.as_str()).unwrap_or("").to_string(),
    })
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
}

/// When the Mac's clipboard was last seen changing: (hash, unix seconds).
static MAC_SEEN: Mutex<Option<(u64, u64)>> = Mutex::new(None);

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
    let mac = cut(mac_clipboard());
    let h = hash(&mac);
    let mac_at = {
        let mut g = MAC_SEEN.lock().unwrap_or_else(|e| e.into_inner());
        match *g {
            Some((old, at)) if old == h => at,
            _ => {
                // First look counts as now: what's on the clipboard is the
                // freshest thing we know of.
                let at = now();
                *g = Some((h, at));
                at
            }
        }
    };
    if let Some((at, text)) = crate::tmux::newest_buffer() {
        if at > mac_at && !text.trim().is_empty() && text != mac {
            let text = cut(text);
            let id = format!("t{:x}", hash(&text));
            return Clip { text, from: "terminal", id };
        }
    }
    Clip { text: mac, from: "mac", id: format!("m{h:x}") }
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

fn sweep(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > UPLOAD_KEEP);
        if old {
            let _ = std::fs::remove_file(e.path());
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
        let user = br#"{"parentUuid":"a","type":"user","uuid":"u1","timestamp":"t","message":{"role":"user","content":"fix the bug"}}"#;
        assert_eq!(parse_line(user).unwrap().text, "fix the bug");
        let reply = br#"{"parentUuid":"a","type":"assistant","uuid":"u2","timestamp":"t","message":{"content":[{"type":"thinking","thinking":"x"},{"type":"text","text":"Done."}]}}"#;
        let m = parse_line(reply).unwrap();
        assert_eq!((m.role, m.text.as_str()), ("assistant", "Done."));
        let tool = br#"{"parentUuid":"a","type":"user","message":{"content":[{"type":"tool_result","content":"x"}]}}"#;
        assert!(parse_line(tool).is_none());
        let cmd = br#"{"parentUuid":"a","type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#;
        assert!(parse_line(cmd).is_none());
        let html = br#"{"parentUuid":"a","type":"user","message":{"content":"<div> is off by one"}}"#;
        assert!(parse_line(html).is_some());
        let meta = br#"{"parentUuid":"a","type":"user","isMeta":true,"message":{"content":"hi"}}"#;
        assert!(parse_line(meta).is_none());
        let side = br#"{"parentUuid":"a","type":"assistant","isSidechain":true,"message":{"content":[{"type":"text","text":"x"}]}}"#;
        assert!(parse_line(side).is_none());
    }
}

