//! Recent Claude conversations of a folder, so New terminal can continue one
//! (`claude --resume <id>`). Read from `<config>/projects/<dir>/*.jsonl`.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::status::{alive, claude_config_dir, safe_id};

/// How many conversations the list offers, and how many files it looks at
/// to find them (empty transcripts are skipped).
const SHOW: usize = 12;
const LOOK: usize = 40;
/// The first prompt is looked for in this much of the transcript's start.
const HEAD: u64 = 256 << 10;

#[derive(Serialize)]
pub struct Past {
    id: String,
    title: String,
    /// Last change, ms since the epoch.
    modified: u64,
}

#[tauri::command]
pub async fn recent_sessions(cwd: String) -> Vec<Past> {
    tauri::async_runtime::spawn_blocking(move || recent(&cwd))
        .await
        .unwrap_or_default()
}

fn recent(cwd: &str) -> Vec<Past> {
    if !cwd.starts_with('/') {
        return vec![];
    }
    let dir = claude_config_dir()
        .join("projects")
        .join(crate::titles::project_dir(cwd));
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut files: Vec<(u64, String, std::path::PathBuf)> = rd
        .flatten()
        .filter_map(|d| {
            let p = d.path();
            let id = p.file_stem()?.to_str()?.to_string();
            if p.extension()? != "jsonl" || !safe_id(&id) {
                return None;
            }
            let m = std::fs::symlink_metadata(&p).ok()?;
            if !m.is_file() {
                return None;
            }
            let ms = m
                .modified()
                .ok()?
                .duration_since(UNIX_EPOCH)
                .ok()?
                .as_millis() as u64;
            Some((ms, id, p))
        })
        .collect();
    files.sort_unstable_by_key(|a| std::cmp::Reverse(a.0));
    let live = running();
    files
        .into_iter()
        .filter(|(_, id, _)| !live.contains(id))
        .take(LOOK)
        .filter_map(|(modified, id, p)| {
            let mut title = crate::titles::tail_title(&p);
            if title.is_empty() {
                title = first_prompt(&p)?;
            }
            Some(Past {
                id,
                title,
                modified,
            })
        })
        .take(SHOW)
        .collect()
}

/// Conversations a running Claude has open right now (resuming one would
/// fork it): `<config>/sessions/<pid>.json` of live processes.
fn running() -> HashSet<String> {
    let mut out = HashSet::new();
    let Ok(rd) = std::fs::read_dir(claude_config_dir().join("sessions")) else {
        return out;
    };
    for d in rd.flatten() {
        let p = d.path();
        let Some(pid) = p
            .file_stem()
            .and_then(|s| s.to_str())
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        if !alive(pid) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&p) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        for key in ["sessionId", "parkedJobId"] {
            if let Some(s) = v.get(key).and_then(|s| s.as_str()) {
                out.insert(s.to_string());
            }
        }
    }
    out
}

/// The first thing the user typed, for conversations without a title yet.
/// None for transcripts with no prompt (nothing to continue).
fn first_prompt(path: &Path) -> Option<String> {
    let f = std::fs::File::open(path).ok()?;
    let r = BufReader::new(f.take(HEAD));
    for line in r.split(b'\n').map_while(Result::ok) {
        if let Some(t) = prompt_in(&line) {
            return Some(t);
        }
    }
    None
}

fn prompt_in(line: &[u8]) -> Option<String> {
    let v: serde_json::Value = serde_json::from_slice(line).ok()?;
    if v.get("type")?.as_str()? != "user" || v.get("isMeta").and_then(|m| m.as_bool()) == Some(true)
    {
        return None;
    }
    let c = v.get("message")?.get("content")?;
    let text = match c {
        serde_json::Value::String(s) => s.as_str(),
        serde_json::Value::Array(a) => a
            .iter()
            .find(|b| b.get("type").and_then(|t| t.as_str()) == Some("text"))?
            .get("text")?
            .as_str()?,
        _ => return None,
    };
    let text = text.trim();
    // Command echoes, caveats and hook output are wrapped in tags.
    if text.is_empty() || text.starts_with('<') {
        return None;
    }
    Some(crate::titles::one_line(text))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_typed_prompt() {
        assert_eq!(
            prompt_in(
                br#"{"type":"user","message":{"role":"user","content":"  fix the\nlogin  bug"}}"#
            )
            .as_deref(),
            Some("fix the login bug")
        );
        assert_eq!(
            prompt_in(br#"{"type":"user","message":{"content":[{"type":"image"},{"type":"text","text":"what is this"}]}}"#).as_deref(),
            Some("what is this")
        );
        assert_eq!(
            prompt_in(br#"{"type":"user","isMeta":true,"message":{"content":"x"}}"#),
            None
        );
        assert_eq!(
            prompt_in(
                br#"{"type":"user","message":{"content":"<command-name>/clear</command-name>"}}"#
            ),
            None
        );
        assert_eq!(
            prompt_in(br#"{"type":"assistant","message":{"content":"hi"}}"#),
            None
        );
    }

    #[test]
    fn ignores_relative_folders() {
        assert!(recent("relative/path").is_empty());
    }
}
