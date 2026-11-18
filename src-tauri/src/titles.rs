//! Conversation titles of Claude sessions, read from their transcripts.
//!
//! Claude appends title records to `<config>/projects/<dir>/<session>.jsonl`:
//! `custom-title` (the user's `/rename`), `agent-name` (background sessions)
//! and `ai-title` (generated from the conversation). Transcripts grow to tens
//! of MB, so each one is streamed once and afterwards only the bytes appended
//! since the last look are read.

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Title records are short; longer lines are only checked for their start.
const HEAD: usize = 4096;
/// How long a session without a transcript waits before the next search.
const RETRY: Duration = Duration::from_secs(30);

#[derive(Default, Clone)]
struct Found {
    custom: String,
    agent: String,
    ai: String,
}

impl Found {
    fn best(&self) -> &str {
        [&self.custom, &self.agent, &self.ai]
            .into_iter()
            .find(|s| !s.is_empty())
            .map(|s| s.as_str())
            .unwrap_or("")
    }
}

enum Entry {
    /// Transcript found; `pos` is where the next read starts (a line start).
    Known { path: PathBuf, pos: u64, found: Found },
    Missing { at: Instant },
}

static CACHE: Mutex<Option<HashMap<String, Entry>>> = Mutex::new(None);

/// Claude names a project folder after the cwd with every character other
/// than ASCII letters and digits replaced by '-'.
pub(crate) fn project_dir(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn locate(session_id: &str, cwd: &str) -> Option<PathBuf> {
    let root = crate::status::claude_config_dir().join("projects");
    let file = format!("{session_id}.jsonl");
    if !cwd.is_empty() {
        let p = root.join(project_dir(cwd)).join(&file);
        if p.is_file() {
            return Some(p);
        }
    }
    std::fs::read_dir(&root)
        .ok()?
        .flatten()
        .map(|d| d.path().join(&file))
        .find(|p| p.is_file())
}

/// Reads the complete lines from `pos` on and records the newest title of
/// each kind. Returns the offset after the last complete line; a line still
/// being written is read again next time.
fn scan(path: &PathBuf, pos: u64, found: &mut Found) -> std::io::Result<u64> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if len < pos {
        // Rewritten or truncated: start over.
        *found = Found::default();
        return scan(path, 0, found);
    }
    if len == pos {
        return Ok(pos);
    }
    f.seek(SeekFrom::Start(pos))?;
    let mut r = BufReader::with_capacity(256 << 10, f.take(len - pos));
    let mut at = pos;
    let mut line = Vec::new();
    loop {
        line.clear();
        // Only the line start matters, so long lines (tool output, images)
        // are skipped without being kept in memory.
        let n = read_head(&mut r, &mut line)?;
        if n == 0 || line.last() != Some(&b'\n') {
            return Ok(at);
        }
        at += n as u64;
        read_line(&line[..line.len() - 1], found);
    }
}

/// Reads one line, keeping at most HEAD bytes of it (plus the final '\n').
/// Returns the number of bytes consumed.
fn read_head(r: &mut impl BufRead, out: &mut Vec<u8>) -> std::io::Result<usize> {
    let mut n = 0;
    loop {
        let buf = r.fill_buf()?;
        if buf.is_empty() {
            return Ok(n);
        }
        let (take, done) = match buf.iter().position(|&b| b == b'\n') {
            Some(i) => (i + 1, true),
            None => (buf.len(), false),
        };
        let room = HEAD.saturating_sub(out.len());
        out.extend_from_slice(&buf[..take.min(room)]);
        if done && take > room {
            out.push(b'\n'); // cut a long line short, but keep it complete
        }
        r.consume(take);
        n += take;
        if done {
            return Ok(n);
        }
    }
}

fn read_line(line: &[u8], found: &mut Found) {
    // Title records start with their type; matching the prefix keeps tool
    // output that merely mentions these words from being parsed.
    let (kind, key) = if line.starts_with(br#"{"type":"custom-title""#) {
        (0, "customTitle")
    } else if line.starts_with(br#"{"type":"agent-name""#) {
        (1, "agentName")
    } else if line.starts_with(br#"{"type":"ai-title""#) {
        (2, "aiTitle")
    } else {
        return;
    };
    let Ok(v) = serde_json::from_slice::<serde_json::Value>(line) else {
        return;
    };
    let Some(t) = v.get(key).and_then(|s| s.as_str()) else {
        return;
    };
    let t = clean(t);
    match kind {
        0 => found.custom = t,
        1 => found.agent = t,
        _ => found.ai = t,
    }
}

/// One line, no control characters, at most 80 characters.
fn clean(s: &str) -> String {
    let one: String = s
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    if one.chars().count() > 80 {
        one.chars().take(79).collect::<String>() + "…"
    } else {
        one
    }
}

/// The best known title of a Claude session ("" if it has none yet).
pub fn title(session_id: &str, cwd: &str) -> String {
    if session_id.is_empty() || !crate::status::safe_id(session_id) {
        return String::new();
    }
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    let cache = guard.get_or_insert_with(HashMap::new);
    let entry = cache.remove(session_id);
    let (path, pos, mut found) = match entry {
        Some(Entry::Known { path, pos, found }) => (path, pos, found),
        Some(Entry::Missing { at }) if at.elapsed() < RETRY => {
            cache.insert(session_id.into(), Entry::Missing { at });
            return String::new();
        }
        _ => match locate(session_id, cwd) {
            Some(p) => (p, 0, Found::default()),
            None => {
                cache.insert(session_id.into(), Entry::Missing { at: Instant::now() });
                return String::new();
            }
        },
    };
    let pos = match scan(&path, pos, &mut found) {
        Ok(p) => p,
        Err(_) => {
            // Moved or deleted: look for it again later.
            cache.insert(session_id.into(), Entry::Missing { at: Instant::now() });
            return found.best().to_string();
        }
    };
    let best = found.best().to_string();
    cache.insert(session_id.into(), Entry::Known { path, pos, found });
    best
}

/// The newest title in the last part of a transcript (titles are appended
/// as the conversation goes), without the per-session cache.
pub(crate) fn tail_title(path: &std::path::Path) -> String {
    const TAIL: u64 = 1 << 20;
    let Ok(mut f) = File::open(path) else {
        return String::new();
    };
    let len = f.metadata().map(|m| m.len()).unwrap_or(0);
    let start = len.saturating_sub(TAIL);
    if f.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut r = BufReader::new(f.take(TAIL));
    let mut found = Found::default();
    let mut line = Vec::new();
    let mut first = start > 0; // the first line is cut: skip it
    loop {
        line.clear();
        match read_head(&mut r, &mut line) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        if std::mem::take(&mut first) {
            continue;
        }
        let l = line.strip_suffix(b"\n").unwrap_or(&line);
        read_line(l, &mut found);
    }
    found.best().to_string()
}

/// Shortens text to one clean line for a list.
pub(crate) fn one_line(s: &str) -> String {
    clean(s)
}

/// Forgets sessions that are no longer running.
pub fn retain(live: &std::collections::HashSet<&str>) {
    let mut guard = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(c) = guard.as_mut() {
        c.retain(|k, _| live.contains(k.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn names_project_dirs_like_claude() {
        assert_eq!(
            project_dir("/Users/a/My_Apps/shop_mobile"),
            "-Users-a-My-Apps-shop-mobile"
        );
    }

    #[test]
    fn prefers_custom_title_and_reads_appends() {
        let dir = std::env::temp_dir().join(format!("toranj-titles-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        let mut f = File::create(&p).unwrap();
        writeln!(f, r#"{{"type":"user","message":"{{\"type\":\"ai-title\"}}"}}"#).unwrap();
        writeln!(f, r#"{{"type":"ai-title","aiTitle":"First  title\n","sessionId":"x"}}"#).unwrap();
        let mut found = Found::default();
        let pos = scan(&p, 0, &mut found).unwrap();
        assert_eq!(found.best(), "First title");

        // A half-written line is left for the next read.
        write!(f, r#"{{"type":"custom-title","customTitle":"Mine"#).unwrap();
        f.flush().unwrap();
        let pos2 = scan(&p, pos, &mut found).unwrap();
        assert_eq!(pos2, pos);
        assert_eq!(found.best(), "First title");

        writeln!(f, r#"","sessionId":"x"}}"#).unwrap();
        writeln!(f, r#"{{"type":"ai-title","aiTitle":"Later","sessionId":"x"}}"#).unwrap();
        let pos3 = scan(&p, pos2, &mut found).unwrap();
        assert_eq!(pos3, std::fs::metadata(&p).unwrap().len());
        assert_eq!(found.best(), "Mine");
        assert_eq!(found.ai, "Later");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn skips_long_lines() {
        let dir = std::env::temp_dir().join(format!("toranj-titles-long-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("t.jsonl");
        let mut f = File::create(&p).unwrap();
        writeln!(f, r#"{{"type":"custom-title","customTitle":"Early","sessionId":"x"}}"#).unwrap();
        writeln!(f, r#"{{"type":"user","x":"{}"}}"#, "y".repeat(3 * HEAD)).unwrap();
        writeln!(f, r#"{{"type":"ai-title","aiTitle":"Late","sessionId":"x"}}"#).unwrap();
        let mut found = Found::default();
        let pos = scan(&p, 0, &mut found).unwrap();
        assert_eq!(pos, std::fs::metadata(&p).unwrap().len());
        assert_eq!((found.custom.as_str(), found.ai.as_str()), ("Early", "Late"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn shortens_long_titles() {
        let t = clean(&"x".repeat(200));
        assert_eq!(t.chars().count(), 80);
        assert!(t.ends_with('…'));
    }
}
