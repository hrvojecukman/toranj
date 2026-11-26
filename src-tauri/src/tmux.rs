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
# Selecting with the mouse copies, like iTerm's "copy on select", and leaves
# copy mode so typing goes to the program. The copy reaches the clipboard as
# OSC 52 through Toranj (set-clipboard on), which keeps it only after a real
# drag: a click that tmux takes for a tiny selection or a double-click
# doesn't replace what you copied.
set -g copy-command ""
bind -T copy-mode MouseDragEnd1Pane send -X copy-selection-and-cancel
bind -T copy-mode-vi MouseDragEnd1Pane send -X copy-selection-and-cancel
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

fn socket() -> String {
    // Tests run against a throwaway server, never the user's.
    #[cfg(test)]
    if let Ok(s) = std::env::var("TORANJ_TEST_SOCKET") {
        return s;
    }
    SOCKET.to_string()
}

fn tmux() -> Command {
    let mut c = Command::new(bin());
    c.arg("-L").arg(socket()).arg("-f").arg(conf_path());
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

/// A program that turned on mouse reporting and exited without turning it
/// off (Claude does when it quits abruptly) leaves its pane forwarding every
/// drag to the shell, which ignores it: nothing can be selected. For panes
/// back at a shell prompt, write the "mouse off" sequences to the pane's tty
/// so tmux sees them as output and selects with the mouse again.
pub fn reset_stale_mouse() {
    let tok = nonce();
    let sep = format!("\u{1f}{tok}\u{1f}");
    let fmt = ["#{session_name}", "#{pane_tty}", "#{pane_current_command}", "#{mouse_any_flag}", "#{alternate_on}"].join(&sep);
    let Ok(out) = run(&["list-panes", "-a", "-F", &fmt]) else { return };
    for l in out.lines() {
        let f: Vec<&str> = l.split(&sep).collect();
        if f.len() != 5 || !crate::spool::valid_tab(f[0]) || f[3] != "1" || f[4] != "0" {
            continue;
        }
        let shell = f[2].trim_start_matches('-');
        if !matches!(shell, "zsh" | "bash" | "fish" | "sh") {
            continue;
        }
        let tty = f[1];
        if !tty.strip_prefix("/dev/ttys").is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())) {
            continue;
        }
        use std::os::unix::fs::OpenOptionsExt;
        // Never become this tty's controlling process; never wait on it.
        let opened = std::fs::OpenOptions::new().write(true).custom_flags(libc::O_NOCTTY | libc::O_NONBLOCK).open(tty);
        if let Ok(mut t) = opened {
            use std::io::Write;
            let _ = t.write_all(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1005l\x1b[?1006l\x1b[?1015l");
        }
    }
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

/// tmux reads an argument ending in ';' as a command separator and drops
/// the ';'. Escaped, it stays part of the text.
fn no_sep(s: &str) -> String {
    match s.strip_suffix(';') {
        Some(rest) => format!("{rest}\\;"),
        None => s.to_string(),
    }
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
    run(&["set-option", "-t", &exact_pane(&name), "@toranj_project", &no_sep(project)])?;
    if !command.trim().is_empty() {
        run(&["send-keys", "-t", &exact_pane(&name), "-l", "--", &no_sep(command)])?;
        run(&["send-keys", "-t", &exact_pane(&name), "Enter"])?;
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

/// Exact tmux target for a session (only for commands that take a session,
/// like kill-session and attach-session; options and keys take a pane, so
/// they need `exact_pane`, or tmux answers "no such session"). A bare name falls back to prefix and
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
mod create_tests {
    /// A new tab gets its project option and its command typed in.
    #[test]
    fn create_types_the_command() {
        if !super::bin().exists() {
            return; // no tmux here (e.g. CI)
        }
        let sock = format!("toranj_test_{}", std::process::id());
        unsafe { std::env::set_var("TORANJ_TEST_SOCKET", &sock) };
        // The throwaway server goes away even if an assertion fails.
        struct Kill;
        impl Drop for Kill {
            fn drop(&mut self) {
                let _ = super::run(&["kill-server"]);
            }
        }
        let _kill = Kill;
        let dir = std::env::temp_dir();
        let d = dir.to_string_lossy();
        let name = super::create(&d, &d, "echo toranj-ok").expect("tab is created");
        let project = super::run(&["show-options", "-v", "-t", &super::exact_pane(&name), "@toranj_project"]).unwrap_or_default();
        let mut screen = String::new();
        for _ in 0..30 {
            screen = super::run(&["capture-pane", "-p", "-t", &super::exact_pane(&name)]).unwrap_or_default();
            if screen.contains("toranj-ok") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert_eq!(project.trim_end_matches('\n'), d, "project option set");
        assert!(screen.contains("toranj-ok"), "command was typed: {screen}");
    }
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

/// The pane's screen and recent history with colors (SGR escapes), and its
/// width, for the phone's read-only view.
pub fn capture_color(name: &str, history: u32) -> Result<(String, u32), String> {
    let start = format!("-{}", history.min(2000));
    let text = run(&["capture-pane", "-p", "-e", "-J", "-S", &start, "-t", &exact_pane(name)])
        .map_err(|_| "tab no longer exists".to_string())?;
    let width = run(&["display-message", "-p", "-t", &exact_pane(name), "#{pane_width}"])
        .ok()
        .and_then(|w| w.trim().parse().ok())
        .unwrap_or(80);
    Ok((text, width))
}

/// Types `text` into the tab and presses Enter. It goes in as one bracketed
/// paste (so newlines stay part of the message instead of sending it early),
/// then Enter after a short pause, so the program doesn't take the Enter as
/// part of the paste.
///
/// Files come first: each path goes in as a paste of its own, the way
/// dropping a file on a terminal does, so Claude Code attaches an image
/// instead of only reading its path. Text may be empty when there are files.
pub fn send_with_files(name: &str, text: &str, files: &[String]) -> Result<(), String> {
    let text_ok = text.len() <= 8000 && !text.chars().any(|c| c.is_control() && c != '\n' && c != '\t');
    if !text_ok || (text.trim().is_empty() && files.is_empty()) {
        return Err("text not allowed".into());
    }
    let t = exact_pane(name);
    // The tab must exist before anything is loaded.
    run(&["display-message", "-p", "-t", &t, ""]).map_err(|_| "tab no longer exists".to_string())?;
    for f in files {
        // The path alone, as a drop pastes it; the space separating it from
        // what follows is typed after.
        paste(&t, f)?;
        run(&["send-keys", "-t", &t, "-l", " "])?;
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    if !text.trim().is_empty() {
        paste(&t, text)?;
    }
    std::thread::sleep(std::time::Duration::from_millis(if files.is_empty() { 150 } else { 400 }));
    run(&["send-keys", "-t", &t, "Enter"]).map(|_| ())
}

/// One bracketed paste of `text` into pane `t`, without Enter.
fn paste(t: &str, text: &str) -> Result<(), String> {
    use std::io::Write;
    let buf = format!("toranj-phone-{}", nonce());
    let mut child = tmux()
        .args(["load-buffer", "-b", &buf, "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .map_err(|e| format!("tmux did not start: {e}"))?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    if !child.wait().map_err(|e| e.to_string())?.success() {
        return Err("could not hand the text to tmux".into());
    }
    if run(&["paste-buffer", "-p", "-d", "-b", &buf, "-t", t]).is_err() {
        let _ = run(&["delete-buffer", "-b", &buf]);
        return Err("tab no longer exists".into());
    }
    Ok(())
}

/// The newest tmux paste buffer (what a program copied with OSC 52, or a
/// mouse selection) and when it was made, unix seconds. The phone's own
/// paste buffers don't count.
pub fn newest_buffer() -> Option<(u64, String)> {
    let sep = nonce();
    let list = run(&["list-buffers", "-F", &format!("#{{buffer_created}}{sep}#{{buffer_name}}")]).ok()?;
    let (at, name) = list
        .lines()
        .filter_map(|l| {
            let (at, name) = l.split_once(sep.as_str())?;
            Some((at.parse::<u64>().ok()?, name.to_string()))
        })
        .filter(|(_, n)| !n.starts_with("toranj-phone-"))
        .max_by_key(|(at, _)| *at)?;
    let text = run(&["show-buffer", "-b", &name]).ok()?;
    Some((at, text))
}

/// Presses one of a few named keys the phone keyboard lacks.
pub fn send_key(name: &str, key: &str) -> Result<(), String> {
    const KEYS: [&str; 7] = ["Escape", "Enter", "BTab", "Tab", "C-c", "Up", "Down"];
    if !KEYS.contains(&key) {
        return Err("key not allowed".into());
    }
    run(&["send-keys", "-t", &exact_pane(name), key]).map(|_| ())
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
    // Claude counts down on some prompts ("…automatically block this request
    // in 1:48"): the ticking clock must not make it a different question.
    let steady: Vec<String> = context.iter().map(|l| steady_line(l)).collect();
    steady.hash(&mut h);
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

/// A prompt line with countdown clocks (`in 1:48`, `in 12s`) blanked out.
fn steady_line(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut rest = line;
    while let Some(i) = rest.find("in ") {
        out.push_str(&rest[..i + 3]);
        rest = &rest[i + 3..];
        let clock: usize = rest
            .char_indices()
            .take_while(|(_, c)| c.is_ascii_digit() || *c == ':' || *c == 's')
            .map(|(i, c)| i + c.len_utf8())
            .last()
            .unwrap_or(0);
        let c = &rest[..clock];
        // Only clocks: "1:48" or "12s", never a plain number.
        let is_clock = c.starts_with(|c: char| c.is_ascii_digit()) && (c.contains(':') || c.ends_with('s'));
        if is_clock {
            out.push('#');
            rest = &rest[clock..];
        }
    }
    out.push_str(rest);
    out
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
    run(&["send-keys", "-t", &exact_pane(name), key])?;
    answered().insert(name.to_string(), (fingerprint.to_string(), std::time::Instant::now()));
    Ok(())
}

/// Questions answered from Toranj (dock, phone), by tab. Claude's own status
/// keeps saying "waiting" until the approved step finishes, which can take
/// minutes; meanwhile the tab isn't waiting for anyone.
fn answered() -> std::sync::MutexGuard<'static, std::collections::HashMap<String, (String, std::time::Instant)>> {
    static A: std::sync::OnceLock<std::sync::Mutex<std::collections::HashMap<String, (String, std::time::Instant)>>> = std::sync::OnceLock::new();
    A.get_or_init(Default::default).lock().unwrap_or_else(|e| e.into_inner())
}

/// For a tab whose agent reports "waiting": true while the question answered
/// from Toranj is still the latest one (it is gone, or still on screen right
/// after the key press). A new question, or 20 minutes, end it.
pub fn still_answered(name: &str) -> bool {
    let Some((fp, at)) = answered().get(name).cloned() else { return false };
    let current = screen(name).ok().and_then(|s| prompt(&s));
    let same = match current {
        None => true,
        Some(p) => p.fingerprint == fp,
    };
    if !same || at.elapsed() > std::time::Duration::from_secs(20 * 60) {
        answered().remove(name);
        return false;
    }
    true
}

/// The agent moved on (running, idle, done): nothing to remember.
pub fn forget_answered(name: &str) {
    answered().remove(name);
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
    fn countdown_keeps_the_fingerprint() {
        let a = PERMISSION.replace("Push to remote", "Claude Code will automatically block this request in 1:48");
        let b = PERMISSION.replace("Push to remote", "Claude Code will automatically block this request in 1:47");
        assert_eq!(prompt(&a).unwrap().fingerprint, prompt(&b).unwrap().fingerprint);
        // Other numbers still count.
        let c = PERMISSION.replace("git push origin main", "git push origin v2");
        assert_ne!(prompt(PERMISSION).unwrap().fingerprint, prompt(&c).unwrap().fingerprint);
        assert_eq!(super::steady_line("block in 1:48 or in 12s, keep in place"), "block in # or in #, keep in place");
        assert_eq!(super::steady_line("rm the 3 files in 2 dirs"), "rm the 3 files in 2 dirs");
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
