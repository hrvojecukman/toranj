//! Status per tab, from adapters.
//! - Claude adapter: `claude agents --json` (source of truth for Claude sessions).
//! - Generic adapter: process in the pane, recent output, terminal bell.

use crate::tmux::Session;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Clone, Debug)]
pub struct TabStatus {
    pub name: String,
    pub project: String,
    pub cwd: String,
    pub agent: String,  // "claude" | "codex" | "gemini" | ... | "shell" | <command>
    pub state: String,  // running | waiting | idle | done | error
    pub detail: String, // e.g. what the agent waits for
    /// Conversation title (Claude only), "" when unknown.
    pub title: String,
    pub since: u64,     // unix seconds of last activity
    /// The tab only shows a `claude --bg` session (`claude attach`): closing
    /// the tab leaves that session running unless it is stopped too.
    pub background: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct ExternalAgent {
    pub name: String,
    pub cwd: String,
    pub state: String,
    pub detail: String,
    pub title: String,
    pub session_id: String,
    pub background: bool,
    /// A Claude process that `claude agents` does not list (it never
    /// registered, e.g. started by an older version). Its conversation id is
    /// unknown, so it can only be moved by hand; `session_id` is `pid-<pid>`.
    pub unlisted: bool,
    pub pid: u32,
}

#[derive(Serialize, Clone, Debug)]
pub struct Snapshot {
    pub tabs: Vec<TabStatus>,
    pub external: Vec<ExternalAgent>,
    /// Git state per project path of the open tabs.
    pub git: std::collections::HashMap<String, crate::git::GitInfo>,
}

#[derive(Deserialize, Debug)]
struct ClaudeRow {
    #[serde(default)]
    pid: Option<u32>,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "waitingFor")]
    waiting_for: Option<serde_json::Value>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default, rename = "sessionId")]
    session_id: Option<String>,
    /// "interactive" (a claude in some terminal) or "background" (`claude --bg`).
    #[serde(default)]
    kind: Option<String>,
    /// Short id of a background session, for `claude attach`.
    #[serde(default)]
    id: Option<String>,
    /// Unix milliseconds.
    #[serde(default, rename = "startedAt")]
    started_at: Option<u64>,
}

const SHELLS: &[&str] = &["zsh", "bash", "fish", "sh", "-zsh", "-bash", "login"];
const AGENTS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "aider",
    "opencode",
    "cursor-agent",
    "amp",
];

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn claude_bin() -> String {
    let home = dirs::home_dir().unwrap_or_default();
    for p in [
        home.join(".local/bin/claude"),
        "/opt/homebrew/bin/claude".into(),
        "/usr/local/bin/claude".into(),
    ] {
        if p.exists() {
            return p.to_string_lossy().into_owned();
        }
    }
    "claude".into()
}

fn claude_rows() -> Vec<ClaudeRow> {
    let Ok(out) = Command::new(claude_bin())
        .args(["agents", "--json"])
        .output()
    else {
        return vec![];
    };
    serde_json::from_slice(&out.stdout).unwrap_or_default()
}

/// pid -> (ppid, command name)
fn process_table() -> HashMap<u32, (u32, String)> {
    let Ok(out) = Command::new("/bin/ps")
        .args(["-axo", "pid=,ppid=,comm="])
        .output()
    else {
        return HashMap::new();
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let pid = it.next()?.parse().ok()?;
            let ppid = it.next()?.parse().ok()?;
            let comm = it.collect::<Vec<_>>().join(" ");
            let base = comm.rsplit('/').next().unwrap_or(&comm).to_string();
            Some((pid, (ppid, base)))
        })
        .collect()
}

fn descends_from(mut pid: u32, root: u32, procs: &HashMap<u32, (u32, String)>) -> bool {
    for _ in 0..32 {
        if pid == root {
            return true;
        }
        match procs.get(&pid) {
            Some((ppid, _)) if *ppid > 1 => pid = *ppid,
            _ => return false,
        }
    }
    false
}

/// First known agent among the pane's descendants (agents often run under node).
fn agent_in_pane(pane_pid: u32, procs: &HashMap<u32, (u32, String)>) -> Option<String> {
    procs
        .iter()
        .filter(|(pid, _)| **pid != pane_pid && descends_from(**pid, pane_pid, procs))
        .find_map(|(_, (_, comm))| AGENTS.iter().find(|a| comm == *a).map(|a| a.to_string()))
}

/// Claude status: `claude agents --json` is the source of truth (it notices
/// interrupts and crashes); the hook spool adds detail and fills the gap
/// between polls when the agent stops to ask for something.
fn claude_state(r: &ClaudeRow) -> (String, String) {
    let mut detail = match &r.waiting_for {
        Some(serde_json::Value::String(s)) => s.clone(),
        Some(v) if !v.is_null() => v.to_string(),
        _ => String::new(),
    };
    let s = r.status.as_deref().or(r.state.as_deref()).unwrap_or("idle");
    let mut state = match s {
        "busy" | "working" => "running",
        "waiting" | "blocked" => "waiting",
        "failed" => "error",
        "done" => "done",
        _ => "idle",
    };
    if let Some(last) = r.session_id.as_deref().and_then(crate::spool::last) {
        let age_ms = now_ms().saturating_sub(last.ts);
        let asks = matches!(last.event.as_str(), "PermissionRequest" | "Elicitation")
            || (last.event == "Notification"
                && matches!(
                    last.kind.as_str(),
                    "permission_prompt" | "elicitation_dialog" | "agent_needs_input"
                ));
        match state {
            // Bridges the gap until `claude agents` reports the prompt. An old
            // record must not pin a running tool (or a denied prompt) to "waiting".
            "running" if asks && age_ms < 10_000 => state = "waiting",
            "idle" if last.event == "StopFailure" => state = "error",
            // Background work after Stop usually resumes the turn soon; if it
            // never does, fall back to what `claude agents` says.
            "idle" if last.event == "Stop" && last.bg > 0 && age_ms < 10 * 60_000 => {
                state = "running"
            }
            _ => {}
        }
        if (state == "waiting" || state == "error") && !last.detail.is_empty() && detail.is_empty()
        {
            detail = last.detail;
        }
    }
    (state.into(), detail)
}

fn row_title(r: &ClaudeRow) -> String {
    let t = crate::titles::title(
        r.session_id.as_deref().unwrap_or(""),
        r.cwd.as_deref().unwrap_or(""),
    );
    if !t.is_empty() || r.kind.as_deref() != Some("background") {
        return t;
    }
    // A background session's name is its title; interactive ones get
    // generated names like "toranj-2c".
    r.name.clone().unwrap_or_default()
}

/// The background job an interactive Claude handed its conversation to
/// (Claude records `parkedJobId` in `<config>/sessions/<pid>.json`); its
/// terminal keeps showing that conversation.
fn parked_job(pid: u32) -> Option<String> {
    let f = claude_config_dir().join("sessions").join(format!("{pid}.json"));
    let text = std::fs::read_to_string(f).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    let job = v.get("parkedJobId")?.as_str()?;
    if !safe_id(job) {
        return None;
    }
    // The file outlives a Claude that crashed: only trust it while the pid
    // is still the process that wrote it (same start time).
    let wrote = v.get("procStart")?.as_str()?;
    // Claude writes it in UTC.
    let out = Command::new("/bin/ps").env("TZ", "UTC").args(["-o", "lstart=", "-p", &pid.to_string()]).output().ok()?;
    let now = String::from_utf8_lossy(&out.stdout);
    (now.split_whitespace().eq(wrote.split_whitespace())).then(|| job.to_string())
}

/// The background row whose conversation a Claude in this pane parked. A
/// parked Claude drops out of `claude agents`, so the pane's processes are
/// checked.
fn parked_in_pane(pane_pid: u32, procs: &HashMap<u32, (u32, String)>, rows: &[ClaudeRow]) -> Option<usize> {
    // Newest process first, so the answer doesn't depend on map order.
    let mut pids: Vec<u32> = procs
        .keys()
        .copied()
        .filter(|&p| p != pane_pid && descends_from(p, pane_pid, procs))
        .collect();
    pids.sort_unstable_by(|a, b| b.cmp(a));
    let job = pids.into_iter().find_map(parked_job)?;
    rows.iter().position(|b| {
        b.kind.as_deref() == Some("background")
            && (b.id.as_deref() == Some(job.as_str())
                || b.session_id.as_deref().is_some_and(|s| s.starts_with(&job)))
    })
}

pub fn snapshot(sessions: &[Session]) -> Snapshot {
    let procs = process_table();
    let rows = claude_rows();
    let mut used = vec![false; rows.len()];
    let t = now();

    let tabs: Vec<TabStatus> = sessions
        .iter()
        .map(|s| {
            // Claude adapter: a `claude agents` row whose pid lives in this
            // pane, or the `claude --bg` session this tab attached to (its
            // process runs outside the pane).
            // Sent to the background from this tab: the tab still shows that
            // conversation, so it speaks for it (and it isn't "outside").
            let parked = parked_in_pane(s.pane_pid, &procs, &rows);
            if let Some((i, r)) = parked.map(|j| (j, &rows[j])).or_else(|| rows.iter().enumerate().find(|(_, r)| {
                let attached_here = !s.attached.is_empty()
                    && r.kind.as_deref() == Some("background")
                    && r.session_id.as_deref() == Some(s.attached.as_str());
                attached_here
                    || r.pid
                        .map(|p| descends_from(p, s.pane_pid, &procs))
                        .unwrap_or(false)
            })) {
                used[i] = true;
                let (state, detail) = claude_state(r);
                return TabStatus {
                    name: s.name.clone(),
                    project: s.project.clone(),
                    cwd: s.cwd.clone(),
                    agent: "claude".into(),
                    state,
                    detail,
                    title: row_title(r),
                    since: s.activity,
                    background: !s.attached.is_empty(),
                };
            }
            // Generic adapter.
            let agent = agent_in_pane(s.pane_pid, &procs).unwrap_or_else(|| {
                if SHELLS.contains(&s.command.as_str()) {
                    "shell".into()
                } else {
                    s.command.clone()
                }
            });
            let recent = t.saturating_sub(s.activity) < 3;
            let state = if s.bell {
                "waiting"
            } else if agent == "shell" {
                "idle"
            } else if recent {
                "running"
            } else if AGENTS.contains(&agent.as_str()) {
                "idle"
            } else {
                "running" // long-running command (dev server) without output
            };
            TabStatus {
                name: s.name.clone(),
                project: s.project.clone(),
                cwd: s.cwd.clone(),
                agent,
                state: state.into(),
                detail: if s.bell {
                    "terminal rang the bell".into()
                } else {
                    String::new()
                },
                title: String::new(),
                since: s.activity,
                background: !s.attached.is_empty(),
            }
        })
        .collect();

    let external = rows
        .iter()
        .enumerate()
        .filter(|(i, r)| !used[*i] && (r.pid.is_some() || r.kind.as_deref() == Some("background")))
        .map(|(_, r)| {
            let (state, detail) = claude_state(r);
            ExternalAgent {
                name: r.name.clone().unwrap_or_default(),
                cwd: r.cwd.clone().unwrap_or_default(),
                state,
                detail,
                title: row_title(r),
                session_id: r.session_id.clone().unwrap_or_default(),
                background: r.kind.as_deref() == Some("background"),
                unlisted: false,
                pid: r.pid.unwrap_or(0),
            }
        })
        .chain(unlisted_claudes(&rows, sessions, &procs))
        .collect();

    // An empty list usually means `claude agents` failed: keep the cache.
    if !rows.is_empty() {
        crate::titles::retain(&rows.iter().filter_map(|r| r.session_id.as_deref()).collect());
    }

    // Git only runs for projects Toranj opened tabs in (absolute paths from
    // the project picker), never for wherever a shell happens to be.
    let mut git = std::collections::HashMap::new();
    for t in &tabs {
        if t.project.starts_with('/') && !git.contains_key(&t.project)
            && let Some(g) = crate::git::info(&t.project) {
                git.insert(t.project.clone(), g);
            }
    }
    crate::git::retain(
        &git.keys()
            .cloned()
            .collect::<std::collections::HashSet<_>>(),
    );

    Snapshot {
        tabs,
        external,
        git,
    }
}

#[cfg(test)]
mod tests {
    /// Live smoke test against the local tmux server and `claude agents`.
    /// Run with: cargo test -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_snapshot() {
        let sessions = crate::tmux::list();
        let snap = super::snapshot(&sessions);
        println!("SNAPSHOT_JSON={}", serde_json::to_string(&snap).unwrap());
    }
}

/// Claude subcommands that are helpers, not a conversation the user runs.
const CLAUDE_HELPERS: &[&str] = &[
    "agents",
    "attach",
    "daemon",
    "bg-pty-host",
    "bg-spare",
    "mcp",
    "update",
    "doctor",
    "-p",
    "--print",
];

/// cwd per pid, so lsof runs once per new process, not on every snapshot.
static CWDS: std::sync::Mutex<Option<HashMap<u32, String>>> = std::sync::Mutex::new(None);

fn cwds(pids: &[u32]) -> HashMap<u32, String> {
    let mut cache = CWDS.lock().unwrap_or_else(|e| e.into_inner());
    let cache = cache.get_or_insert_with(HashMap::new);
    cache.retain(|p, _| pids.contains(p));
    let missing: Vec<String> = pids
        .iter()
        .filter(|p| !cache.contains_key(p))
        .map(|p| p.to_string())
        .collect();
    if !missing.is_empty() {
        if let Ok(out) = Command::new("/usr/sbin/lsof")
            .args(["-a", "-d", "cwd", "-Fpn", "-p", &missing.join(",")])
            .output()
        {
            let mut pid = 0;
            for l in String::from_utf8_lossy(&out.stdout).lines() {
                if let Some(p) = l.strip_prefix('p') {
                    pid = p.parse().unwrap_or(0);
                } else if let Some(n) = l.strip_prefix('n') {
                    cache.insert(pid, n.to_string());
                }
            }
        }
        // Remember failures too, so a process without a readable cwd is not
        // asked again every two seconds.
        for p in &missing {
            cache.entry(p.parse().unwrap_or(0)).or_default();
        }
    }
    pids.iter()
        .filter_map(|p| cache.get(p).map(|c| (*p, c.clone())))
        .collect()
}

/// Is this command line an interactive Claude conversation?
fn is_claude_conversation(args: &str) -> bool {
    let base = |a: &str| a.rsplit('/').next().unwrap_or(a).to_string();
    let mut it = args.split_whitespace().peekable();
    let Some(bin) = it.next() else { return false };
    // npm installs run as `node /path/to/claude …`.
    if base(bin) == "node" && it.peek().map(|a| base(a)).as_deref() == Some("claude") {
        it.next();
    } else if base(bin) != "claude" {
        return false;
    }
    let rest: Vec<&str> = it.collect();
    // One-shot `claude -p …` runs are not conversations to move.
    if rest.iter().any(|a| *a == "-p" || *a == "--print") {
        return false;
    }
    rest.first().is_none_or(|a| !CLAUDE_HELPERS.contains(a))
}

/// The user's Claude processes that `claude agents` does not list and that
/// don't run inside a Toranj tab.
fn unlisted_claudes(
    rows: &[ClaudeRow],
    sessions: &[Session],
    procs: &HashMap<u32, (u32, String)>,
) -> Vec<ExternalAgent> {
    // -x without -a: only this user's processes.
    let Ok(out) = Command::new("/bin/ps").args(["-xo", "pid=,args="]).output() else {
        return vec![];
    };
    let listed: std::collections::HashSet<u32> = rows.iter().filter_map(|r| r.pid).collect();
    let pids: Vec<u32> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let l = l.trim_start();
            let (pid, args) = l.split_once(char::is_whitespace)?;
            let pid: u32 = pid.parse().ok()?;
            is_claude_conversation(args.trim()).then_some(pid)
        })
        .filter(|p| *p != std::process::id() && !listed.contains(p))
        .filter(|p| {
            !sessions
                .iter()
                .any(|s| descends_from(*p, s.pane_pid, procs))
        })
        .collect();
    let cwds = cwds(&pids);
    pids.into_iter()
        .map(|pid| {
            let started = elapsed_secs(pid).map(|e| now().saturating_sub(e));
            ExternalAgent {
                name: String::new(),
                cwd: cwds.get(&pid).cloned().unwrap_or_default(),
                state: "idle".into(),
                detail: started
                    .map(|s| format!("started {} ago", ago(now().saturating_sub(s))))
                    .unwrap_or_default(),
                title: String::new(),
                session_id: format!("pid-{pid}"),
                background: false,
                unlisted: true,
                pid,
            }
        })
        .collect()
}

fn ago(secs: u64) -> String {
    match secs {
        s if s >= 2 * 86_400 => format!("{} d", s / 86_400),
        s if s >= 3600 => format!("{} h", s / 3600),
        s => format!("{} min", s / 60),
    }
}

/// Session ids end up in a command line typed into a shell, so only a strict
/// UUID-ish character set is accepted.
pub(crate) fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-')
}

/// EPERM means the process exists but belongs to someone else: still alive.
pub(crate) fn alive(pid: u32) -> bool {
    // kill(0 or a negative pid) would address process groups.
    if pid == 0 || pid > i32::MAX as u32 {
        return false;
    }
    let ok = unsafe { libc::kill(pid as libc::pid_t, 0) } == 0;
    ok || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Seconds since the process started, from `ps -o etime` ([[dd-]hh:]mm:ss).
fn elapsed_secs(pid: u32) -> Option<u64> {
    let out = std::process::Command::new("/bin/ps")
        .args(["-o", "etime=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let t = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let (days, rest) = match t.split_once('-') {
        Some((d, r)) => (d.parse::<u64>().ok()?, r.to_string()),
        None => (0, t),
    };
    let parts: Vec<u64> = rest
        .split(':')
        .map(|x| x.parse().ok())
        .collect::<Option<_>>()?;
    let (h, m, s) = match parts.as_slice() {
        [m, s] => (0, *m, *s),
        [h, m, s] => (*h, *m, *s),
        _ => return None,
    };
    Some(days * 86_400 + h * 3600 + m * 60 + s)
}

/// The pid from `claude agents` is still that Claude process (not a reused
/// pid): its command is claude and it started when the session did.
fn is_that_claude(pid: u32, started_at_ms: Option<u64>) -> bool {
    let args = std::process::Command::new("/bin/ps")
        .args(["-o", "args=", "-p", &pid.to_string()])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).to_lowercase())
        .unwrap_or_default();
    if !args
        .split_whitespace()
        .take(3)
        .any(|a| a.rsplit('/').next() == Some("claude"))
    {
        return false;
    }
    match (started_at_ms, elapsed_secs(pid)) {
        // The process can be older than the session (/clear or /resume start a
        // new session in the same process), but a reused pid belongs to a
        // process that started after the session did.
        (Some(ms), Some(el)) => now().saturating_sub(el) <= ms / 1000 + 15,
        _ => false,
    }
}

pub(crate) fn claude_config_dir() -> std::path::PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(std::path::PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".claude"))
}

fn has_transcript(session_id: &str) -> bool {
    std::fs::read_dir(claude_config_dir().join("projects"))
        .into_iter()
        .flatten()
        .flatten()
        .any(|d| d.path().join(format!("{session_id}.jsonl")).is_file())
}

/// Quotes a path for a shell command shown to the user.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Where a project that no longer exists at `old` lives now: the one known
/// project whose path ends with the same parent/name (e.g. Shop/shop_mobile
/// moved from ~/Documents/Projects to ~/Developer/Projects).
fn moved_project(old: &str) -> Option<String> {
    let tail = |p: &str| -> Option<String> {
        let mut it = p.trim_end_matches('/').rsplit('/');
        let name = it.next().filter(|n| !n.is_empty())?;
        Some(format!("{}/{name}", it.next().unwrap_or("")))
    };
    let want = tail(old)?;
    let mut hits = crate::projects::scan()
        .into_iter()
        .filter(|p| tail(&p.path).as_deref() == Some(want.as_str()));
    let hit = hits.next()?;
    hits.next().is_none().then_some(hit.path)
}

/// Stops the `claude --bg` session with this id; its conversation is kept.
///
/// What to do is decided from `claude agents`, never from command output:
/// - no such session listed: it is already gone, nothing to do;
/// - its process runs: `claude stop`, and any failure is an error;
/// - no process (it exited but is still listed): `claude rm` takes it off the
///   list. Claude refuses that while its worktree has unpushed or uncommitted
///   work, and Toranj never passes the flags that would override it.
pub fn stop_background(session_id: &str) -> Result<(), String> {
    if !safe_id(session_id) {
        return Err("invalid session id".into());
    }
    let rows = claude_rows();
    let Some(r) = rows.iter().find(|r| {
        r.session_id.as_deref() == Some(session_id) && r.kind.as_deref() == Some("background")
    }) else {
        return Ok(());
    };
    // `claude stop` and `rm` take the short id, not the full session id.
    let short =
        r.id.clone()
            .filter(|i| safe_id(i) && !i.starts_with('-'))
            .ok_or("session has no short id")?;
    let running = r.pid.is_some_and(alive);
    let cmd = if running { "stop" } else { "rm" };
    let out = Command::new(claude_bin())
        .args([cmd, short.as_str()])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        return Ok(());
    }
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");
    Err(if running {
        format!("Claude did not stop the session: {first}")
    } else {
        format!(
            "the session is no longer running, but Claude did not remove it from the list (its worktree may have unsaved changes): {first}"
        )
    })
}

/// Ids being moved right now, so a second click can't start a second copy.
static MOVING: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

struct MoveGuard(String);

impl MoveGuard {
    fn take(id: &str) -> Option<Self> {
        let mut m = MOVING.lock().unwrap_or_else(|e| e.into_inner());
        m.get_or_insert_with(Default::default)
            .insert(id.to_string())
            .then(|| MoveGuard(id.to_string()))
    }
}

impl Drop for MoveGuard {
    fn drop(&mut self) {
        if let Some(m) = MOVING.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
            m.remove(&self.0);
        }
    }
}

/// Moves a Claude session that runs outside Toranj into a new Toranj tab and
/// returns the tab name.
///
/// - Background session (`claude --bg`): a tab that runs `claude attach <id>`;
///   nothing is stopped, and the tab remembers the id so the session is no
///   longer listed as outside.
/// - Interactive session that is idle (same rule as the sidebar, so a session
///   with background work counts as running): its process is checked to be
///   that same Claude (command and start time, guarding against pid reuse),
///   its saved conversation must exist, then it gets SIGTERM and, once it
///   has exited, the tab runs `claude --resume <id>`.
/// - A session that is working, or already inside a Toranj tab, is never touched
///   unless `force` is set (the user confirmed interrupting the current turn).
/// - A background session whose folder was moved or deleted is refused with
///   the fix (link the old path, or `claude rm`): Claude can't wake it.
pub fn move_external(session_id: &str, force: bool) -> Result<String, String> {
    if !safe_id(session_id) {
        return Err("invalid session id".into());
    }
    let _guard = MoveGuard::take(session_id).ok_or("this session is already being moved")?;
    let rows = claude_rows();
    let r = rows
        .iter()
        .find(|r| r.session_id.as_deref() == Some(session_id))
        .ok_or("session no longer exists")?;
    let background = r.kind.as_deref() == Some("background");
    let cwd = match r.cwd.as_deref() {
        Some(c) if std::path::Path::new(c).is_dir() => c.to_string(),
        // Claude wakes a background session only in the folder it started
        // in ("Workspace not trusted … could not be resolved on disk"), so an
        // attach tab elsewhere would never work: explain how to fix it.
        missing if background => {
            let old = missing.unwrap_or("?");
            let short = r.id.as_deref().filter(|i| safe_id(i)).unwrap_or(session_id);
            let fix = match moved_project(old) {
                Some(new) => {
                    let parent = std::path::Path::new(old)
                        .parent()
                        .map(|p| p.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    format!(
                        "If the project was moved to {new}, put a link at the old location and try again:\nmkdir -p {} && ln -s {} {}\n",
                        sh_quote(&parent),
                        sh_quote(&new),
                        sh_quote(old)
                    )
                }
                None => "Put the folder back at that location and try again.\n".into(),
            };
            return Err(format!(
                "Claude cannot wake this background session because its folder no longer exists ({old}).\n{fix}Or remove it from the list (the conversation stays saved): claude rm {short}"
            ));
        }
        missing => {
            return Err(format!(
                "the session folder no longer exists ({}); Claude ties a conversation to its folder, so continue it manually from the project's new folder: claude --resume {session_id}",
                missing.unwrap_or("?")
            ));
        }
    };
    let sessions = crate::tmux::list();

    if background {
        if sessions.iter().any(|s| s.attached == session_id) {
            return Err("this session is already open in Toranj".into());
        }
        // Sent to the background from a Toranj tab, which still shows it.
        let procs = process_table();
        let me = rows.iter().position(|b| std::ptr::eq(b, r));
        if me.is_some() && sessions.iter().any(|s| parked_in_pane(s.pane_pid, &procs, &rows) == me) {
            return Err("this session is already open in Toranj: its terminal sent it to the background".into());
        }
        let short =
            r.id.clone()
                .filter(|i| safe_id(i))
                .unwrap_or_else(|| session_id.to_string());
        let tab = crate::tmux::create(&cwd, &cwd, &format!("claude attach {short}"))?;
        crate::tmux::set_option(&tab, "@toranj_attached", session_id)?;
        return Ok(tab);
    }

    let pid = r.pid.ok_or("session has no process")?;
    let procs = process_table();
    if sessions
        .iter()
        .any(|s| descends_from(pid, s.pane_pid, &procs))
    {
        return Err("this session is already in Toranj".into());
    }
    if !force && claude_state(r).0 != "idle" {
        return Err("the agent is working right now; move it when it finishes".into());
    }
    if !is_that_claude(pid, r.started_at) {
        return Err("the process cannot be confirmed as that Claude session; leaving it alone".into());
    }
    if !has_transcript(session_id) {
        return Err(
            "the session has no saved conversation (it has not received a message yet); close it manually".into(),
        );
    }
    if pid == 0 || pid > i32::MAX as u32 {
        return Err("invalid process id".into());
    }
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        return Err(format!(
            "Could not close Claude: {}",
            std::io::Error::last_os_error()
        ));
    }
    let start = std::time::Instant::now();
    while alive(pid) {
        if start.elapsed() > std::time::Duration::from_secs(8) {
            return Err("Claude did not exit; close it manually and try again".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    // Claude is already closed here, so a failure must say how to get the
    // conversation back.
    crate::tmux::create(&cwd, &cwd, &format!("claude --resume {session_id}")).map_err(|e| {
        format!("Claude was closed, but the tab did not open ({e}). Continue the conversation manually: claude --resume {session_id}")
    })
}

#[cfg(test)]
mod move_tests {
    /// Moves the session in TORANJ_MOVE_ID. Run by hand:
    /// TORANJ_MOVE_ID=<uuid> cargo test -p toranj live_move -- --ignored --nocapture
    #[test]
    #[ignore]
    fn live_move() {
        // Skipped unless asked for: scripts/dump-mock.sh runs every ignored test.
        let Ok(id) = std::env::var("TORANJ_MOVE_ID") else {
            return;
        };
        println!("MOVED_TO={:?}", super::move_external(&id, false));
    }

    #[test]
    #[ignore]
    fn live_moved_project() {
        let Ok(old) = std::env::var("TORANJ_OLD_CWD") else {
            return;
        };
        println!("MOVED_PROJECT={:?}", super::moved_project(&old));
    }

    #[test]
    fn quotes_paths_for_the_shell() {
        assert_eq!(super::sh_quote("/a b/c"), "'/a b/c'");
        assert_eq!(super::sh_quote("x'y"), r"'x'\''y'");
    }

    #[test]
    fn spots_claude_conversations() {
        use super::is_claude_conversation as c;
        assert!(c("claude"));
        assert!(c("claude --resume"));
        assert!(c("/Users/x/.local/bin/claude --chrome"));
        assert!(!c("claude agents --json"));
        assert!(!c("claude bg-spare --bg-spare /tmp/x"));
        assert!(!c("claude attach a7f41851"));
        assert!(!c("/x/claude daemon run"));
        assert!(!c("node /x/claude-helper"));
        assert!(!c("vim claude"));
        assert!(c("node /usr/local/lib/node_modules/.bin/claude --resume"));
        assert!(!c("claude --model opus -p hello"));
        assert!(!c("/Applications/Claude.app/Contents/MacOS/Claude"));
    }

    #[test]
    fn parses_etime() {
        assert!(super::elapsed_secs(std::process::id()).is_some());
    }

    #[test]
    fn second_move_of_same_id_is_refused() {
        let a = super::MoveGuard::take("abc-1");
        assert!(a.is_some());
        assert!(super::MoveGuard::take("abc-1").is_none());
        drop(a);
        assert!(super::MoveGuard::take("abc-1").is_some());
    }

    #[test]
    fn rejects_unsafe_ids() {
        assert!(super::move_external("abc; rm -rf ~", false).is_err());
        assert!(super::move_external("", false).is_err());
    }
}
