//! Installs and removes Toranj's entries in `~/.claude/settings.json`.
//!
//! - Only entries whose command is a `toranj-hook` binary are ever touched;
//!   the user's own hooks and settings are kept as they are (key order too).
//! - The hook binary is copied to an owner-only directory first, so the path
//!   recorded in settings never points into an app bundle that may move or a
//!   build directory that any build can overwrite.
//! - Writes keep the file's permissions, follow a symlinked settings.json to
//!   its real file, never clobber a change Claude Code made in between, and
//!   leave a backup (the last three are kept).

use serde::Serialize;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// Events Toranj listens to, and whether the hook may run in the background.
/// Tool events fire constantly, so they never block Claude Code. PreToolUse
/// is left out on purpose: it comes before PermissionRequest and adds nothing.
pub const EVENTS: &[(&str, bool)] = &[
    ("SessionStart", false),
    ("UserPromptSubmit", false),
    ("PostToolUse", true),
    ("PostToolUseFailure", true),
    ("PermissionRequest", false),
    ("PermissionDenied", true),
    ("Elicitation", false),
    ("Notification", false),
    ("Stop", false),
    ("StopFailure", false),
    ("SessionEnd", false),
];

const HOOK_NAME: &str = "toranj-hook";
const KEEP_BACKUPS: usize = 3;

#[derive(Serialize)]
pub struct HookStatus {
    pub installed: bool,
    /// Installed and the recorded binary is gone: every hook call would fail.
    pub broken: bool,
    pub hook_path: String,
    /// The binary that ships with the app and gets copied on install.
    pub hook_exists: bool,
    pub settings_path: String,
    /// Exactly what would be added, for the confirmation dialog.
    pub preview: String,
}

fn settings_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".claude/settings.json")
}

fn exe_name() -> &'static str {
    if cfg!(windows) {
        "toranj-hook.exe"
    } else {
        HOOK_NAME
    }
}

/// The hook binary that ships next to the app executable.
fn bundled_hook() -> PathBuf {
    let exe = std::env::current_exe().unwrap_or_default();
    exe.parent()
        .map(|d| d.join(exe_name()))
        .unwrap_or_else(|| PathBuf::from(exe_name()))
}

/// Where the hook is installed: `$XDG_DATA_HOME/toranj/bin` (default
/// `~/.local/share/toranj/bin`), owner-only.
pub fn installed_hook() -> PathBuf {
    let base = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".local/share"));
    base.join("toranj/bin").join(exe_name())
}

fn copy_hook() -> Result<PathBuf, String> {
    let src = bundled_hook();
    if !src.exists() {
        return Err(format!("{} does not exist", src.display()));
    }
    let dst = installed_hook();
    let dir = dst.parent().ok_or("invalid path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    // Copy to a temp name and rename, so a running hook is never half-written.
    let tmp = dir.join(format!(".{HOOK_NAME}.{}", std::process::id()));
    std::fs::copy(&src, &tmp).map_err(|e| format!("hook was not copied: {e}"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700))
            .map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &dst).map_err(|e| e.to_string())?;
    Ok(dst)
}

fn is_ours(hook: &Value) -> bool {
    hook.get("command")
        .and_then(|c| c.as_str())
        .is_some_and(|c| Path::new(c).file_stem().is_some_and(|s| s == HOOK_NAME))
}

fn our_paths(settings: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) {
        for groups in hooks.values().filter_map(|g| g.as_array()) {
            for g in groups {
                for h in g
                    .get("hooks")
                    .and_then(|l| l.as_array())
                    .into_iter()
                    .flatten()
                {
                    if is_ours(h)
                        && let Some(c) = h.get("command").and_then(|c| c.as_str()) {
                            out.push(c.to_string());
                        }
                }
            }
        }
    }
    out
}

/// Real file behind `~/.claude/settings.json` (follows a dotfiles symlink).
fn real_path() -> PathBuf {
    let p = settings_path();
    std::fs::canonicalize(&p).unwrap_or(p)
}

fn read_text(path: &Path) -> Result<Option<String>, String> {
    match std::fs::read_to_string(path) {
        Ok(t) => Ok(Some(t)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.to_string()),
    }
}

fn parse(text: Option<&str>) -> Result<Value, String> {
    match text {
        None => Ok(json!({})),
        Some(t) if t.trim().is_empty() => Ok(json!({})),
        Some(t) => serde_json::from_str(t)
            .map_err(|e| format!("~/.claude/settings.json is not valid JSON: {e}")),
    }
}

/// Backups go to `dir` (always `~/.claude`), never next to a symlink target
/// that may live in a dotfiles repo.
fn backup(path: &Path, dir: &Path) -> Result<(), String> {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("settings.json");
    let prefix = format!("{name}.toranj-backup-");
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    std::fs::copy(path, dir.join(format!("{prefix}{stamp}")))
        .map_err(|e| format!("backup was not created: {e}"))?;
    // copy() keeps the source permissions, so backups stay as private as the original.
    {
        let mut old: Vec<PathBuf> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
            })
            .collect();
        old.sort();
        let excess = old.len().saturating_sub(KEEP_BACKUPS);
        for p in old.into_iter().take(excess) {
            let _ = std::fs::remove_file(p);
        }
    }
    Ok(())
}

/// Reads settings, applies `change`, and writes the result atomically.
/// If the file changed while we worked (Claude Code saving `/config`), the
/// whole edit is redone on the new content.
fn edit(change: impl Fn(&mut Value) -> Result<bool, String>) -> Result<(), String> {
    let backups = settings_path()
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    edit_at(&real_path(), &backups, change)
}

fn edit_at(
    path: &Path,
    backups: &Path,
    change: impl Fn(&mut Value) -> Result<bool, String>,
) -> Result<(), String> {
    let path = path.to_path_buf();
    for _ in 0..3 {
        let before = read_text(&path)?;
        let mut v = parse(before.as_deref())?;
        if !v.is_object() {
            return Err("~/.claude/settings.json is not a JSON object".into());
        }
        if !change(&mut v)? {
            return Ok(());
        }
        let text = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())? + "\n";

        let dir = path.parent().ok_or("invalid path")?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let tmp = dir.join(format!(
            ".settings.json.toranj-{}-{}",
            std::process::id(),
            rand_suffix()
        ));
        write_private(&tmp, &text, &path)?;

        if read_text(&path)? != before {
            let _ = std::fs::remove_file(&tmp);
            continue; // someone else wrote meanwhile: redo on fresh content
        }
        if before.is_some()
            && let Err(e) = backup(&path, backups) {
                let _ = std::fs::remove_file(&tmp);
                return Err(e);
            }
        return std::fs::rename(&tmp, &path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp);
            e.to_string()
        });
    }
    Err("settings.json keeps changing; try again in a moment".into())
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

/// Creates `tmp` exclusively with the original file's mode (0600 for a new file).
fn write_private(tmp: &Path, text: &str, original: &Path) -> Result<(), String> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mode = std::fs::metadata(original)
            .map(|m| m.permissions().mode() & 0o777)
            .unwrap_or(0o600);
        opts.mode(mode);
    }
    #[cfg(not(unix))]
    let _ = original;
    let mut f = opts.open(tmp).map_err(|e| e.to_string())?;
    f.write_all(text.as_bytes())
        .and_then(|_| f.sync_all())
        .map_err(|e| {
            let _ = std::fs::remove_file(tmp);
            e.to_string()
        })
}

fn entry(command: &Path, background: bool) -> Value {
    let mut hook = json!({ "type": "command", "command": command.to_string_lossy(), "args": [] });
    if background {
        hook["async"] = json!(true);
    }
    json!({ "hooks": [hook] })
}

/// Removes our hook objects; drops matcher groups and events left empty.
fn strip(settings: &mut Value) -> bool {
    let before = settings.clone();
    let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return false;
    };
    for groups in hooks.values_mut() {
        if let Some(arr) = groups.as_array_mut() {
            for g in arr.iter_mut() {
                if let Some(list) = g.get_mut("hooks").and_then(|l| l.as_array_mut()) {
                    list.retain(|h| !is_ours(h));
                }
            }
            arr.retain(|g| {
                g.get("hooks")
                    .and_then(|l| l.as_array())
                    .is_none_or(|l| !l.is_empty())
            });
        }
    }
    hooks.retain(|_, groups| groups.as_array().is_none_or(|a| !a.is_empty()));
    if hooks.is_empty() {
        settings.as_object_mut().map(|o| o.remove("hooks"));
    }
    *settings != before
}

fn add(settings: &mut Value, command: &Path) -> Result<(), String> {
    let hooks = settings
        .as_object_mut()
        .ok_or("~/.claude/settings.json is not a JSON object")?
        .entry("hooks")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("\"hooks\" in settings.json is not an object")?;
    for (event, bg) in EVENTS {
        hooks
            .entry(event.to_string())
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or(format!("hooks.{event} is not a list"))?
            .push(entry(command, *bg));
    }
    Ok(())
}

fn installed(settings: &Value) -> bool {
    let Some(hooks) = settings.get("hooks").and_then(|h| h.as_object()) else {
        return false;
    };
    EVENTS.iter().all(|(event, _)| {
        hooks
            .get(*event)
            .and_then(|g| g.as_array())
            .is_some_and(|groups| {
                groups.iter().any(|g| {
                    g.get("hooks")
                        .and_then(|l| l.as_array())
                        .is_some_and(|l| l.iter().any(is_ours))
                })
            })
    })
}

pub fn status() -> Result<HookStatus, String> {
    let settings = parse(read_text(&real_path())?.as_deref())?;
    let paths = our_paths(&settings);
    let preview: serde_json::Map<String, Value> = EVENTS
        .iter()
        .map(|(e, bg)| (e.to_string(), json!([entry(&installed_hook(), *bg)])))
        .collect();
    Ok(HookStatus {
        installed: installed(&settings),
        broken: !paths.is_empty() && paths.iter().any(|p| !Path::new(p).exists()),
        hook_path: installed_hook().to_string_lossy().into_owned(),
        hook_exists: bundled_hook().exists(),
        settings_path: settings_path().to_string_lossy().into_owned(),
        preview: serde_json::to_string_pretty(&json!({ "hooks": preview })).unwrap_or_default(),
    })
}

pub fn install() -> Result<(), String> {
    let hook = copy_hook()?;
    edit(|s| {
        strip(s); // replace older entries, e.g. from a moved app
        add(s, &hook)?;
        Ok(true)
    })
}

pub fn uninstall() -> Result<(), String> {
    edit(|s| Ok(strip(s)))?;
    // The status line runs the same binary.
    if !statusline_status().is_ok_and(|s| s.installed) {
        let _ = std::fs::remove_file(installed_hook());
    }
    Ok(())
}

// ---------- status line ----------
// Claude Code shows a status line under its input box; Toranj's prints the
// project's name there (see toranj-hook `statusline`).

#[derive(Serialize)]
pub struct StatusLineStatus {
    pub installed: bool,
    /// Another status line is set up: Toranj leaves it alone.
    pub other: bool,
    pub settings_path: String,
}

/// `'<path to toranj-hook>' statusline` (the path may hold spaces).
fn statusline_is_ours(v: &Value) -> bool {
    v.get("command").and_then(|c| c.as_str()).is_some_and(|c| {
        let c = c.trim();
        c.ends_with(" statusline") && c.contains(&format!("/{HOOK_NAME}"))
    })
}

/// Single-quoted for the shell Claude runs the status line with.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

pub fn statusline_status() -> Result<StatusLineStatus, String> {
    let settings = parse(read_text(&real_path())?.as_deref())?;
    let sl = settings.get("statusLine");
    Ok(StatusLineStatus {
        installed: sl.is_some_and(statusline_is_ours),
        other: sl.is_some_and(|v| !statusline_is_ours(v)),
        settings_path: settings_path().to_string_lossy().into_owned(),
    })
}

pub fn install_statusline() -> Result<(), String> {
    let hook = copy_hook()?;
    edit(|s| {
        if s.get("statusLine").is_some_and(|v| !statusline_is_ours(v)) {
            return Err("Claude Code already has a status line; Toranj leaves it as it is.".into());
        }
        let cmd = format!("{} statusline", sh_quote(&hook.to_string_lossy()));
        s.as_object_mut()
            .ok_or("settings is not an object")?
            .insert("statusLine".into(), json!({ "type": "command", "command": cmd, "padding": 0 }));
        Ok(true)
    })
}

pub fn uninstall_statusline() -> Result<(), String> {
    edit(|s| {
        if !s.get("statusLine").is_some_and(statusline_is_ours) {
            return Ok(false);
        }
        s.as_object_mut().map(|o| o.remove("statusLine"));
        Ok(true)
    })?;
    if !status().is_ok_and(|s| s.installed) {
        let _ = std::fs::remove_file(installed_hook());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ours() -> PathBuf {
        PathBuf::from("/home/u/.local/share/toranj/bin/toranj-hook")
    }

    #[test]
    fn statusline_command_is_quoted_and_recognised() {
        let cmd = format!("{} statusline", sh_quote("/Users/A B/it's/toranj-hook"));
        assert_eq!(cmd, "'/Users/A B/it'\\''s/toranj-hook' statusline");
        assert!(statusline_is_ours(&json!({ "command": cmd })));
        assert!(!statusline_is_ours(&json!({ "command": "~/bin/my-status.sh" })));
    }

    #[test]
    fn install_then_strip_keeps_user_hooks() {
        let user_hook = json!({ "matcher": "", "hooks": [{ "type": "command", "command": "terminal-notifier -message hi" }] });
        let mut s = json!({ "theme": "dark", "hooks": { "Notification": [user_hook.clone()] } });
        add(&mut s, &ours()).unwrap();
        assert!(installed(&s));
        assert!(strip(&mut s));
        assert_eq!(
            s,
            json!({ "theme": "dark", "hooks": { "Notification": [user_hook] } })
        );
        assert!(!installed(&s));
        assert!(!strip(&mut s));
    }

    #[test]
    fn reinstall_does_not_duplicate() {
        let mut s = json!({});
        add(&mut s, &ours()).unwrap();
        strip(&mut s);
        add(&mut s, &ours()).unwrap();
        assert_eq!(s["hooks"]["Stop"].as_array().unwrap().len(), 1);
        assert_eq!(our_paths(&s).len(), EVENTS.len());
    }

    #[test]
    fn strip_removes_empty_hooks_key() {
        let mut s = json!({ "a": 1, "hooks": { "Stop": [entry(&ours(), false)] } });
        strip(&mut s);
        assert_eq!(s, json!({ "a": 1 }));
    }

    #[test]
    fn edit_keeps_mode_follows_symlink_and_backs_up() {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("toranj-hooks-test-{}", std::process::id()));
        let real = home.join("dotfiles/settings.json");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::create_dir_all(home.join(".claude")).unwrap();
        std::fs::write(&real, "{\"env\":{\"KEY\":\"secret\"}}").unwrap();
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o600)).unwrap();
        std::os::unix::fs::symlink(&real, home.join(".claude/settings.json")).unwrap();
        let target = std::fs::canonicalize(home.join(".claude/settings.json")).unwrap();

        edit_at(&target, &home.join(".claude"), |s| {
            add(s, &ours())?;
            Ok(true)
        })
        .unwrap();

        let link = std::fs::symlink_metadata(home.join(".claude/settings.json")).unwrap();
        assert!(link.file_type().is_symlink(), "symlink must survive");
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&real).unwrap()).unwrap();
        assert_eq!(v["env"]["KEY"], "secret");
        assert!(installed(&v));
        let count = |d: &Path| {
            std::fs::read_dir(d)
                .unwrap()
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().contains("toranj-backup"))
                .count()
        };
        assert_eq!(count(&home.join(".claude")), 1, "backup goes to ~/.claude");
        assert_eq!(
            count(real.parent().unwrap()),
            0,
            "never into the dotfiles repo"
        );
        let _ = std::fs::remove_dir_all(&home);
    }
}
