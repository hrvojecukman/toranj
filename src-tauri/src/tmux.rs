//! tmux session host. Every Toranj tab is one tmux session on a private socket,
//! so agents keep running when the app closes or crashes.

use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

pub const SOCKET: &str = "toranj";

const CONF: &str = r#"# Managed by Toranj. Changes are overwritten on start.
set -g status off
set -g mouse on
set -g history-limit 50000
set -g escape-time 0
set -g default-terminal "tmux-256color"
set -as terminal-features ",xterm-256color:RGB"
set -g set-clipboard on
set -g allow-passthrough off
set -gq get-clipboard off
set -g focus-events on
set -g destroy-unattached off
set -g detach-on-destroy off
set -g monitor-bell on
set -g bell-action any
set -g visual-bell off
# A bell in the only window of a session never sets window_bell_flag, so keep our own flag.
set-hook -g alert-bell 'set-option -w @toranj_bell 1'
# Selecting with the mouse copies to the macOS clipboard, like iTerm's
# "copy on select", and leaves copy mode so typing goes to the program.
bind -T copy-mode MouseDragEnd1Pane send -X copy-pipe-and-cancel "pbcopy"
bind -T copy-mode-vi MouseDragEnd1Pane send -X copy-pipe-and-cancel "pbcopy"
"#;

/// Absolute path to tmux. GUI apps on macOS start with a minimal PATH,
/// so look in the usual install locations first.
pub fn bin() -> &'static Path {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        for p in [
            "/opt/homebrew/bin/tmux",
            "/usr/local/bin/tmux",
            "/usr/bin/tmux",
        ] {
            if Path::new(p).exists() {
                return PathBuf::from(p);
            }
        }
        PathBuf::from("tmux")
    })
}

/// Writes the config once per app run. `-f` only applies when a server
/// starts, so a server left running by an older Toranj reloads it here.
fn conf_path() -> &'static Path {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(|| {
        let dir = dirs::config_dir()
            .unwrap_or_else(std::env::temp_dir)
            .join("toranj");
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tmux.conf");
        let _ = std::fs::write(&path, CONF);
        // Fails harmlessly when no server is running yet.
        let _ = Command::new(bin())
            .arg("-L")
            .arg(SOCKET)
            .arg("source-file")
            .arg(&path)
            .output();
        path
    })
}

fn tmux() -> Command {
    let mut c = Command::new(bin());
    c.arg("-L").arg(SOCKET).arg("-f").arg(conf_path());
    c
}

fn run(args: &[&str]) -> Result<String, String> {
    let out = tmux()
        .args(args)
        .output()
        .map_err(|e| format!("tmux did not start: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[derive(Serialize, Clone, Debug)]
pub struct Session {
    pub name: String,
    pub project: String,
    pub cwd: String,
    pub pane_pid: u32,
    pub command: String,
    pub created: u64,
    pub activity: u64,
    pub bell: bool,
    /// Session id of a `claude --bg` session this tab attached to.
    pub attached: String,
}

/// A per-call token neither a path nor a command can guess: paths and
/// process names may contain newlines and \x1f, so rows and fields are split
/// on separators that carry it.
fn nonce() -> String {
    use std::hash::{BuildHasher, Hasher};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0));
    format!("{:016x}", h.finish())
}

pub fn list() -> Vec<Session> {
    let tok = nonce();
    let sep = format!("\u{1f}{tok}\u{1f}");
    let fields = [
        "#{session_name}",
        "#{@toranj_project}",
        "#{pane_current_path}",
        "#{pane_pid}",
        "#{pane_current_command}",
        "#{session_created}",
        "#{window_activity}",
        "#{@toranj_bell}",
        "#{@toranj_attached}",
    ]
    .join(&sep);
    let fmt = format!("{tok}{sep}{fields}");
    // Errors here mostly mean "no server running yet", which is an empty list.
    let Ok(out) = run(&["list-panes", "-a", "-F", &fmt]) else {
        return vec![];
    };
    let row_start = format!("{tok}{sep}");
    let mut seen = std::collections::HashSet::new();
    out.strip_suffix('\n')
        .unwrap_or(&out)
        .split(&format!("\n{row_start}"))
        .map(|r| r.strip_prefix(&row_start).unwrap_or(r))
        .filter_map(|l| {
            let f: Vec<&str> = l.split(&sep).collect();
            // Only sessions with Toranj's own name format and a real pid.
            if f.len() != 9 || !crate::spool::valid_tab(f[0]) || f[3].parse::<u32>().is_err() || !seen.insert(f[0].to_string()) {
                return None;
            }
            Some(Session {
                name: f[0].into(),
                // Empty for sessions Toranj did not create; never guessed from cwd.
                project: f[1].into(),
                cwd: f[2].into(),
                pane_pid: f[3].parse().unwrap_or(0),
                command: f[4].into(),
                created: f[5].parse().unwrap_or(0),
                activity: f[6].parse().unwrap_or(0),
                bell: f[7] == "1",
                attached: f[8].into(),
            })
        })
        .collect()
}

fn slug(s: &str) -> String {
    let s: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    s.trim_matches('-').to_string()
}

/// Creates a session running the user's login shell in `cwd`, then types
/// `command` into it. Starting through the shell gives the command the user's
/// full PATH and leaves a shell behind when the command exits.
/// Longest command the "new terminal" dialog may start.
const MAX_COMMAND: usize = 500;

/// The command is typed into the new shell as one line. Control characters
/// (a newline would submit extra commands, escape sequences could drive the
/// shell's line editor) are refused, so the shell runs exactly what the
/// dialog showed.
fn check_command(command: &str) -> Result<(), String> {
    if command.chars().count() > MAX_COMMAND {
        return Err(format!(
            "command is too long (at most {MAX_COMMAND} characters)"
        ));
    }
    if command.chars().any(char::is_control) {
        return Err("command must not contain control characters or multiple lines".into());
    }
    Ok(())
}

pub fn create(project: &str, cwd: &str, command: &str) -> Result<String, String> {
    check_command(command)?;
    if !Path::new(cwd).is_dir() {
        return Err("folder does not exist".into());
    }
    // Kept short enough to stay a valid tab name with a "-N" suffix.
    let cut = |s: String, n: usize| s.chars().take(n).collect::<String>();
    let base = format!(
        "{}__{}",
        cut(slug(
            project
                .rsplit('/')
                .find(|p| !p.is_empty())
                .unwrap_or(project)
        ), 60),
        cut(slug(command.split_whitespace().next().unwrap_or("shell")), 40)
    );
    let existing: std::collections::HashSet<String> = list().into_iter().map(|s| s.name).collect();
    let mut name = base.clone();
    let mut i = 2;
    while existing.contains(&name) {
        name = format!("{base}-{i}");
        i += 1;
    }
    let env = format!("TORANJ_TAB={name}");
    // tmux expands #{…} and #(…) in -c: a folder name must stay a name.
    let start_dir = escape_format(cwd);
    run(&[
        "new-session",
        "-d",
        "-s",
        &name,
        "-c",
        &start_dir,
        "-x",
        "160",
        "-y",
        "48",
        "-e",
        &env,
    ])?;
    run(&["set-option", "-t", &exact(&name), "@toranj_project", project])?;
    if !command.trim().is_empty() {
        run(&["send-keys", "-t", &exact(&name), "-l", command])?;
        run(&["send-keys", "-t", &exact(&name), "Enter"])?;
    }
    Ok(name)
}

/// `s` with tmux formats disarmed: '#' doubled, except in a run of '#'
/// followed by '[' (style markup), which tmux already leaves as it is.
fn escape_format(s: &str) -> String {
    let b: Vec<char> = s.chars().collect();
    let mut out = String::with_capacity(s.len() + 8);
    let mut i = 0;
    while i < b.len() {
        if b[i] == '#' {
            let start = i;
            while i < b.len() && b[i] == '#' {
                i += 1;
            }
            let run = i - start;
            let literal = b.get(i) == Some(&'[');
            out.extend(std::iter::repeat_n('#', if literal { run } else { run * 2 }));
        } else {
            out.push(b[i]);
            i += 1;
        }
    }
    out
}

/// Exact tmux target for a session. A bare name falls back to prefix and
/// pattern matching when that session is gone, so "proj__claude" could hit
/// "proj__claude-2"; `=` forces an exact match.
fn exact(name: &str) -> String {
    format!("={name}")
}

/// Exact target for the session's active pane (or window).
fn exact_pane(name: &str) -> String {
    format!("={name}:")
}

pub fn set_option(session: &str, key: &str, value: &str) -> Result<(), String> {
    run(&["set-option", "-t", &exact_pane(session), key, value]).map(|_| ())
}

pub fn kill(name: &str) -> Result<(), String> {
    run(&["kill-session", "-t", &exact(name)]).map(|_| ())
}

pub fn clear_bell(name: &str) {
    let _ = run(&[
        "set-option",
        "-w",
        "-t",
        &exact_pane(name),
        "@toranj_bell",
        "0",
    ]);
}

#[cfg(test)]
mod format_tests {
    use super::escape_format;
    #[test]
    fn disarms_formats_but_keeps_style_runs() {
        assert_eq!(escape_format("/a/#{x}/#(y)"), "/a/##{x}/##(y)");
        assert_eq!(escape_format("p#[fg=red]q"), "p#[fg=red]q");
        assert_eq!(escape_format("h##k#"), "h####k##");
        assert_eq!(escape_format("plain"), "plain");
    }
}

#[cfg(test)]
mod tests {
    use super::check_command;

    #[test]
    fn accepts_normal_commands() {
        for c in [
            "",
            "claude",
            "npm run dev",
            "flutter run --flavor dev -t lib/main_dev.dart",
            "echo 'a; b' | grep č",
        ] {
            assert!(check_command(c).is_ok(), "{c}");
        }
    }

    #[test]
    fn refuses_control_characters_and_long_input() {
        for c in [
            "claude\nrm -rf ~",
            "a\rb",
            "x\u{1b}[200~",
            "tab\there",
            "nul\0",
        ] {
            assert!(check_command(c).is_err(), "{c:?}");
        }
        assert!(check_command(&"a".repeat(501)).is_err());
        assert!(check_command(&"č".repeat(500)).is_ok());
    }
}

/// One choice of a select prompt on screen, e.g. `❯ 1. Yes` in Claude Code.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Choice {
    pub key: String,
    pub label: String,
}

/// A select prompt at the bottom of a tab's screen.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Prompt {
    /// Up to three lines above the choices: what is being asked or approved
    /// (e.g. the command), so a card never asks for a blind "Yes".
    pub context: Vec<String>,
    pub choices: Vec<Choice>,
    /// Identifies this exact prompt (context + choices). Answers carry it
    /// back, so a click never lands in a newer prompt with the same labels.
    pub fingerprint: String,
}

/// What a waiting tab shows: its last lines and, when one is waiting at the
/// bottom of the screen, a select prompt.
#[derive(Serialize, Clone, Debug)]
pub struct Peek {
    pub lines: Vec<String>,
    pub prompt: Option<Prompt>,
}

/// The visible screen only: old, already answered prompts in the scrollback
/// must not count.
fn screen(name: &str) -> Result<String, String> {
    run(&["capture-pane", "-p", "-J", "-t", &exact_pane(name)])
        .map_err(|_| "tab no longer exists".to_string())
}

fn clean(raw: &str) -> &str {
    raw.trim().trim_matches(|c| c == '│' || c == '|').trim()
}

/// `(has_cursor, digit, label)` for a line like `❯ 1. Yes`.
fn choice_line(line: &str) -> Option<(bool, String, String)> {
    let (cursor, rest) = match line.strip_prefix('❯') {
        Some(r) => (true, r.trim_start()),
        None => (false, line),
    };
    let (num, label) = rest.split_once(". ")?;
    if num.len() != 1 || !num.chars().all(|c| ('1'..='9').contains(&c)) {
        return None;
    }
    Some((cursor, num.to_string(), label.trim().to_string()))
}

/// The select prompt at the bottom of the screen: a contiguous numbered list
/// starting at 1 with the `❯` cursor on one entry, followed by at most a few
/// hint lines. Numbered lists in an answer have no cursor and are ignored.
fn prompt(screen: &str) -> Option<Prompt> {
    let lines: Vec<&str> = screen
        .lines()
        .map(clean)
        .filter(|l| !l.is_empty())
        .collect();
    let last = lines.iter().rposition(|l| choice_line(l).is_some())?;
    // Only hint lines ("Esc to cancel", a box border) may follow the list.
    if lines.len() - 1 - last > 3 {
        return None;
    }
    let mut first = last;
    while first > 0 && choice_line(lines[first - 1]).is_some() {
        first -= 1;
    }
    let parsed: Vec<(bool, String, String)> = lines[first..=last]
        .iter()
        .filter_map(|l| choice_line(l))
        .collect();
    let in_order = parsed
        .iter()
        .enumerate()
        .all(|(i, (_, k, _))| *k == (i + 1).to_string());
    if !in_order || !parsed.iter().any(|(c, _, _)| *c) {
        return None;
    }
    let context: Vec<String> = lines[first.saturating_sub(3)..first]
        .iter()
        .map(|l| l.to_string())
        .collect();
    let choices: Vec<Choice> = parsed
        .into_iter()
        .map(|(_, key, label)| Choice { key, label })
        .collect();
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    context.hash(&mut h);
    for c in &choices {
        c.key.hash(&mut h);
        c.label.hash(&mut h);
    }
    Some(Prompt {
        context,
        choices,
        fingerprint: format!("{:016x}", h.finish()),
    })
}

pub fn peek(name: &str) -> Result<Peek, String> {
    let s = screen(name)?;
    let mut lines: Vec<String> = s.lines().map(|l| l.trim_end().to_string()).collect();
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let keep = lines.len().saturating_sub(14);
    Ok(Peek {
        prompt: prompt(&s),
        lines: lines.split_off(keep),
    })
}

/// Picks choice `key` ("1"-"9") of the prompt the user saw, identified by
/// its fingerprint; refuses when the screen shows anything else.
pub fn answer(name: &str, key: &str, fingerprint: &str) -> Result<(), String> {
    let now = prompt(&screen(name)?).ok_or("the question is no longer on screen")?;
    if now.fingerprint != fingerprint || !now.choices.iter().any(|c| c.key == key) {
        return Err("the question changed; open the tab".into());
    }
    run(&["send-keys", "-t", &exact_pane(name), key]).map(|_| ())
}

#[cfg(test)]
mod prompt_tests {
    use super::prompt;

    const PERMISSION: &str = "Bash command\n  git push origin main\n  Push to remote\n Do you want to proceed?\n ❯ 1. Yes\n   2. Yes, and don't ask again for git push commands\n   3. No, and tell Claude what to do differently (esc)\n\n";

    #[test]
    fn reads_claude_permission_prompt() {
        let p = prompt(PERMISSION).unwrap();
        assert_eq!(p.choices.len(), 3);
        assert_eq!(
            p.choices[2].label,
            "No, and tell Claude what to do differently (esc)"
        );
        assert_eq!(
            p.context,
            [
                "git push origin main",
                "Push to remote",
                "Do you want to proceed?"
            ]
        );
    }

    #[test]
    fn same_labels_for_another_command_differ() {
        let other = PERMISSION.replace("git push origin main", "rm -rf build");
        assert_ne!(
            prompt(PERMISSION).unwrap().fingerprint,
            prompt(&other).unwrap().fingerprint
        );
    }

    #[test]
    fn boxed_prompt() {
        let p = prompt("│ Allow? │\n│ ❯ 1. Yes │\n│   2. No  │\n╰──────╯").unwrap();
        assert_eq!(p.choices[1].label, "No");
    }

    #[test]
    fn not_a_prompt() {
        // Plain numbered text, no cursor.
        assert!(prompt("Remaining:\n1. Pull main\n2. Move sessions\n").is_none());
        // An old prompt with output after it is no longer waiting.
        assert!(
            prompt("❯ 1. Yes\n  2. No\nRan 3 commands\nDone.\nAll good.\nNext step\n").is_none()
        );
    }
}
