//! Event spool shared by the `toranj-hook` binary (writer) and the Toranj app
//! (reader).
//!
//! One append-only JSONL file per Claude session in
//! `$XDG_STATE_HOME/toranj/events` (default `~/.local/state/toranj/events`).
//! Records hold only what the status engine needs: never full tool input,
//! tool output or file contents, and short details are redacted.

use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Serialize, Deserialize, Debug, Clone, Default, PartialEq)]
pub struct Record {
    /// Unix time in milliseconds, taken by the hook.
    pub ts: u64,
    pub event: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tool: String,
    /// Notification type, SessionStart source or SessionEnd reason.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    /// Short, single-line, redacted description of what the agent waits for.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub detail: String,
    /// Number of background tasks still running (Stop events).
    #[serde(default)]
    pub bg: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub tab: String,
}

pub fn dir() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/state"));
    base.join("toranj/events")
}

/// Project names Toranj shows, for Claude's status line (see toranj-hook):
/// `{"tabs": {tab: {"label", "color"}}, "projects": {path: {...}}}`.
pub fn labels_path() -> PathBuf {
    dir().parent().map(|p| p.join("labels.json")).unwrap_or_else(|| dir().join("labels.json"))
}

/// Session ids become file names, so only allow a safe character set.
pub fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

pub fn file(session_id: &str) -> Option<PathBuf> {
    file_in(&dir(), session_id)
}

fn file_in(dir: &Path, session_id: &str) -> Option<PathBuf> {
    valid_session_id(session_id).then(|| dir.join(format!("{session_id}.jsonl")))
}

/// Tab names come from Toranj (`<project>__<command>[-n]`); anything else is dropped.
pub fn valid_tab(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
}

/// Creates the spool directory owner-only and refuses to use it if it is a
/// symlink or belongs to someone else.
pub fn ensure_dir() -> Option<PathBuf> {
    ensure_dir_at(&dir())
}

fn ensure_dir_at(d: &Path) -> Option<PathBuf> {
    let d = d.to_path_buf();
    if let Some(parent) = d.parent() {
        std::fs::create_dir_all(parent).ok()?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let _ = std::fs::DirBuilder::new().mode(0o700).create(&d);
        let meta = std::fs::symlink_metadata(&d).ok()?;
        if !meta.is_dir() || meta.uid() != unsafe { libc::getuid() } {
            return None;
        }
        if meta.mode() & 0o077 != 0 {
            std::fs::set_permissions(&d, std::fs::Permissions::from_mode(0o700)).ok()?;
        }
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(&d).ok()?;
    Some(d)
}

/// Appends one record. Owner-only file, never through a symlink.
pub fn append(rec: &Record) -> Option<()> {
    append_in(&dir(), rec)
}

fn append_in(dir: &Path, rec: &Record) -> Option<()> {
    let path = file_in(dir, &rec.session_id)?;
    ensure_dir_at(dir)?;
    let mut line = serde_json::to_string(rec).ok()?;
    line.push('\n');
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    opts.open(path).ok()?.write_all(line.as_bytes()).ok()
}

/// Newest record of a session (by hook timestamp, since background hooks can
/// land out of order), reading only the tail of the file.
pub fn last(session_id: &str) -> Option<Record> {
    last_in(&dir(), session_id)
}

fn last_in(dir: &Path, session_id: &str) -> Option<Record> {
    use std::io::{Read, Seek, SeekFrom};
    let mut f = std::fs::File::open(file_in(dir, session_id)?).ok()?;
    let len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(len.saturating_sub(16 * 1024)))
        .ok()?;
    let mut bytes = Vec::new();
    f.read_to_end(&mut bytes).ok()?;
    // The tail may start mid-line or mid-character; that line fails to parse.
    String::from_utf8_lossy(&bytes)
        .lines()
        .filter_map(|l| serde_json::from_str::<Record>(l).ok())
        .max_by_key(|r| r.ts)
}

/// Deletes spool files untouched for `days`.
pub fn prune(days: u64) {
    let Ok(entries) = std::fs::read_dir(dir()) else {
        return;
    };
    let limit = std::time::Duration::from_secs(days * 86_400);
    for e in entries.flatten() {
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|age| age > limit);
        if old && e.path().extension().is_some_and(|x| x == "jsonl") {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// First line, at most `max` characters, with likely secrets masked:
/// URL query strings, `KEY=value`, `Bearer …`, credential flags, and long
/// opaque or well-known token strings.
pub fn redact(text: &str, max: usize) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let mut out: Vec<String> = Vec::new();
    let mut mask_next = false;
    for word in line.split_whitespace() {
        if mask_next {
            out.push("•••".into());
            mask_next = false;
            continue;
        }
        let lower = word.to_ascii_lowercase();
        let assignment = word.split_once('=').filter(|(k, v)| {
            let k_l = k.to_ascii_lowercase();
            !k.is_empty()
                && !v.is_empty()
                && (!k.starts_with('-')
                    || ["pass", "token", "secret", "key", "auth", "cred"]
                        .iter()
                        .any(|s| k_l.contains(s)))
        });
        let w = if let Some(url) = mask_url(word) {
            url
        } else if let Some((k, _)) = assignment {
            format!("{k}=•••")
        } else if looks_secret(word) {
            "•••".into()
        } else {
            word.to_string()
        };
        let bare = lower.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
        if matches!(
            bare,
            "bearer" | "basic" | "token" | "--user" | "--password" | "--token"
        ) {
            mask_next = true;
        }
        out.push(w);
    }
    let joined = out.join(" ");
    let mut cut: String = joined.chars().take(max).collect();
    if joined.chars().count() > max {
        cut.push('…');
    }
    cut
}

/// `scheme://user:pass@host/path?query` → `scheme://•••@host/path?•••`.
fn mask_url(word: &str) -> Option<String> {
    let (scheme, rest) = word.split_once("://")?;
    let (rest, had_query) = match rest.split_once('?') {
        Some((r, _)) => (r, true),
        None => (rest, false),
    };
    let rest = match rest.split_once('@').filter(|(user, _)| !user.contains('/')) {
        Some((_, host)) => format!("•••@{host}"),
        None => rest.to_string(),
    };
    Some(format!(
        "{scheme}://{rest}{}",
        if had_query { "?•••" } else { "" }
    ))
}

fn looks_secret(word: &str) -> bool {
    let w = word.trim_matches(|c: char| "\"'`,;()[]{}".contains(c));
    let known = [
        "sk-",
        "ghp_",
        "gho_",
        "github_pat_",
        "xox",
        "AKIA",
        "eyJ",
        "glpat-",
        "npm_",
    ];
    if known.iter().any(|p| w.starts_with(p)) && w.len() >= 12 {
        return true;
    }
    // Long run of key-like characters with both letters and digits.
    w.len() >= 32
        && !w.contains('/')
        && w.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_+=.".contains(c))
        && w.chars().any(|c| c.is_ascii_digit())
        && w.chars().any(|c| c.is_ascii_alphabetic())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_common_secrets() {
        assert_eq!(redact("git push origin main", 80), "git push origin main");
        assert_eq!(
            redact("export OPENAI_API_KEY=sk-abc123", 80),
            "export OPENAI_API_KEY=•••"
        );
        assert_eq!(
            redact(
                "curl -H \"Authorization: Bearer abc.def\" https://x.io/a?token=1",
                80
            ),
            "curl -H \"Authorization: Bearer ••• https://x.io/a?•••"
        );
        assert_eq!(redact("echo ghp_1234567890abcdefghij", 80), "echo •••");
        assert_eq!(
            redact("mysql --password=hunter2 --user root", 80),
            "mysql --password=••• --user •••"
        );
        assert_eq!(
            redact("git clone https://me:pw@github.com/a/b", 80),
            "git clone https://•••@github.com/a/b"
        );
        assert_eq!(
            redact("curl -H 'Authorization: Basic abc'", 80),
            "curl -H 'Authorization: Basic •••"
        );
        assert_eq!(
            redact("mkdir -p dir && git push -u origin main", 80),
            "mkdir -p dir && git push -u origin main"
        );
        assert_eq!(redact("first\nsecond", 80), "first");
        assert_eq!(redact("ls --color=auto", 80), "ls --color=auto");
    }

    #[test]
    fn last_picks_newest_by_ts() {
        let dir = std::env::temp_dir().join(format!("toranj-spool-test-{}", std::process::id()));
        let mk = |ts, event: &str| Record {
            ts,
            event: event.into(),
            session_id: "s1".into(),
            ..Default::default()
        };
        append_in(&dir, &mk(20, "PermissionRequest")).unwrap();
        append_in(&dir, &mk(10, "PostToolUse")).unwrap(); // background hook landing late
        assert_eq!(last_in(&dir, "s1").unwrap().event, "PermissionRequest");
        assert!(file_in(&dir, "../x").is_none());
        assert!(valid_tab("toranj__claude-2") && !valid_tab("a b") && !valid_tab("x/y"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
