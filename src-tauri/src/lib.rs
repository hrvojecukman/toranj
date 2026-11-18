mod claude_hooks;
mod git;
mod history;
mod icons;
mod projects;
mod pty;
use toranj_spool as spool;
mod status;
mod system;
mod notifications;
mod titles;
mod tmux;
mod workspaces;

use tauri::ipc::{Channel, InvokeResponseBody};
use tauri::{Manager, State};

#[tauri::command]
async fn list_projects() -> Vec<projects::Project> {
    projects::scan()
}

#[tauri::command]
async fn snapshot() -> Result<status::Snapshot, String> {
    // ps, tmux, claude and git are blocking calls; keep them off the async runtime.
    tauri::async_runtime::spawn_blocking(|| status::snapshot(&tmux::list()))
        .await
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn create_tab(project: String, cwd: String, command: String) -> Result<String, String> {
    tmux::create(&project, &cwd, &command)
}

#[tauri::command]
async fn resources() -> Result<system::Resources, String> {
    tauri::async_runtime::spawn_blocking(system::read)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn stop_process(pid: u32) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || system::stop(pid))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn shutdown_simulators() -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(system::shutdown_simulators)
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn git_details(path: String) -> Result<git::Details, String> {
    tauri::async_runtime::spawn_blocking(move || git::details(&path))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
fn refresh_git(path: String) {
    git::invalidate(&path);
}

#[tauri::command]
async fn move_external(session_id: String, force: Option<bool>) -> Result<String, String> {
    let force = force.unwrap_or(false);
    tauri::async_runtime::spawn_blocking(move || status::move_external(&session_id, force))
        .await
        .map_err(|e| e.to_string())?
}

/// Appends a UI error to the log file, so problems in the webview can be
/// diagnosed without devtools. Size-capped.
#[tauri::command]
fn log_ui(msg: String) {
    log_line(&msg);
}

/// Appends a line to the app's diagnostic log (see `log_ui`).
pub(crate) fn log_line(msg: &str) {
    use std::io::Write;
    let dir = if cfg!(target_os = "macos") {
        dirs::home_dir()
            .unwrap_or_default()
            .join("Library/Logs/Toranj")
    } else {
        dirs::home_dir()
            .unwrap_or_default()
            .join(".local/state/toranj")
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("ui.log");
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > 1_000_000) {
        let _ = std::fs::rename(&path, dir.join("ui.log.1"));
    }
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    if let Ok(mut f) = opts.open(&path) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        // No control characters: the log is read with cat.
        let line: String = msg.chars().take(2000).filter(|c| !c.is_control()).collect();
        let _ = writeln!(f, "{ts} {line}");
    }
}

#[tauri::command]
async fn peek_tab(name: String) -> Result<tmux::Peek, String> {
    tauri::async_runtime::spawn_blocking(move || tmux::peek(&name))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn answer_tab(name: String, key: String, fingerprint: String) -> Result<(), String> {
    if !(key.len() == 1 && key.chars().all(|c| ('1'..='9').contains(&c))) {
        return Err("key not allowed".into());
    }
    tauri::async_runtime::spawn_blocking(move || tmux::answer(&name, &key, &fingerprint))
        .await
        .map_err(|e| e.to_string())?
}

#[tauri::command]
async fn kill_tab(name: String, stop_session: Option<bool>) -> Result<(), String> {
    tauri::async_runtime::spawn_blocking(move || {
        // Stop the background session first: if that fails the tab stays, so
        // the user still sees it and nothing silently keeps running.
        if stop_session.unwrap_or(false) {
            let attached = tmux::list()
                .into_iter()
                .find(|s| s.name == name)
                .map(|s| s.attached)
                .unwrap_or_default();
            if !attached.is_empty() {
                status::stop_background(&attached)?;
            }
        }
        tmux::kill(&name)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
fn attach(
    bridge: State<pty::Bridge>,
    name: String,
    cols: u16,
    rows: u16,
    on_data: Channel<InvokeResponseBody>,
) -> Result<u32, String> {
    tmux::clear_bell(&name);
    bridge.attach(&name, cols, rows, on_data)
}

#[tauri::command]
fn write(bridge: State<pty::Bridge>, id: u32, data: String) -> Result<(), String> {
    bridge.write(id, data.as_bytes())
}

#[tauri::command]
fn resize(bridge: State<pty::Bridge>, id: u32, cols: u16, rows: u16) -> Result<(), String> {
    bridge.resize(id, cols, rows)
}

#[tauri::command]
fn detach(bridge: State<pty::Bridge>, id: u32) {
    bridge.detach(id)
}

/// A light tap on a Force Touch trackpad (e.g. a divider snapping into place).
#[tauri::command]
fn haptic(app: tauri::AppHandle) {
    #[cfg(target_os = "macos")]
    let _ = app.run_on_main_thread(|| {
        use objc2_app_kit::{NSHapticFeedbackManager, NSHapticFeedbackPattern, NSHapticFeedbackPerformanceTime, NSHapticFeedbackPerformer};
        NSHapticFeedbackManager::defaultPerformer()
            .performFeedbackPattern_performanceTime(NSHapticFeedbackPattern::Alignment, NSHapticFeedbackPerformanceTime::Now);
    });
    #[cfg(not(target_os = "macos"))]
    let _ = app;
}

/// Plays a macOS system sound (notifications stay silent while the app is in front).
#[tauri::command]
fn play_sound(name: String) {
    const SOUNDS: [&str; 14] = [
        "Basso", "Blow", "Bottle", "Frog", "Funk", "Glass", "Hero", "Morse", "Ping", "Pop",
        "Purr", "Sosumi", "Submarine", "Tink",
    ];
    if !SOUNDS.contains(&name.as_str()) {
        return;
    }
    #[cfg(target_os = "macos")]
    if let Ok(mut child) = std::process::Command::new("/usr/bin/afplay")
        .arg(format!("/System/Library/Sounds/{name}.aiff"))
        .spawn()
    {
        std::thread::spawn(move || child.wait());
    }
}

#[tauri::command]
async fn claude_statusline_status() -> Result<claude_hooks::StatusLineStatus, String> {
    claude_hooks::statusline_status()
}

#[tauri::command]
async fn install_claude_statusline() -> Result<(), String> {
    claude_hooks::install_statusline()
}

#[tauri::command]
async fn uninstall_claude_statusline() -> Result<(), String> {
    claude_hooks::uninstall_statusline()
}

/// Project names for Claude's status line (read by toranj-hook).
#[tauri::command]
fn set_status_labels(labels: serde_json::Value) -> Result<(), String> {
    let text = serde_json::to_string(&labels).map_err(|e| e.to_string())?;
    if text.len() > 256 * 1024 {
        return Err("too many labels".into());
    }
    let path = toranj_spool::labels_path();
    let dir = path.parent().ok_or("invalid path")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let tmp = dir.join(format!(".labels.{}.json", std::process::id()));
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut f = opts.open(&tmp).map_err(|e| e.to_string())?;
        f.write_all(text.as_bytes()).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

#[tauri::command]
fn open_in_editor(path: String) -> Result<(), String> {
    if !std::path::Path::new(&path).is_dir() {
        return Err("folder does not exist".into());
    }
    #[cfg(target_os = "macos")]
    let mut cmd = {
        let mut c = std::process::Command::new("/usr/bin/open");
        c.args(["-a", "Visual Studio Code", &path]);
        c
    };
    #[cfg(not(target_os = "macos"))]
    let mut cmd = {
        let mut c = std::process::Command::new("code");
        c.arg(&path);
        c
    };
    cmd.spawn().map(|_| ()).map_err(|e| e.to_string())
}

#[tauri::command]
fn claude_hooks_status() -> Result<claude_hooks::HookStatus, String> {
    claude_hooks::status()
}

#[tauri::command]
fn install_claude_hooks() -> Result<(), String> {
    claude_hooks::install()
}

#[tauri::command]
fn uninstall_claude_hooks() -> Result<(), String> {
    claude_hooks::uninstall()
}

/// Tells the UI to refresh as soon as a hook writes, instead of waiting for
/// the next poll.
fn watch_spool(app: tauri::AppHandle) {
    use notify::{RecursiveMode, Watcher};
    use tauri::Emitter;
    let Some(dir) = spool::ensure_dir() else {
        return; // not a private directory we own: don't watch or read it
    };
    spool::prune(7);
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let Ok(mut watcher) = notify::recommended_watcher(tx) else {
            return;
        };
        if watcher.watch(&dir, RecursiveMode::NonRecursive).is_err() {
            return;
        }
        while rx.recv().is_ok() {
            // Coalesce bursts (parallel tool calls) into one refresh.
            std::thread::sleep(std::time::Duration::from_millis(150));
            while rx.try_recv().is_ok() {}
            let _ = app.emit("toranj://spool", ());
        }
    });
}

/// The app menu. The default macOS menu closes the whole window on ⌘W; here
/// ⌘W closes the selected terminal instead (the UI asks first). The Edit menu
/// stays, because copy and paste in the webview go through it.
fn install_menu(app: &tauri::App) -> tauri::Result<()> {
    use tauri::Emitter;
    use tauri::menu::{MenuBuilder, MenuItemBuilder, SubmenuBuilder};
    let close_tab = MenuItemBuilder::with_id("close-tab", "Close terminal")
        .accelerator("CmdOrCtrl+W")
        .build(app)?;
    let app_menu = SubmenuBuilder::new(app, "Toranj")
        .about(None)
        .separator()
        .services()
        .separator()
        .hide()
        .hide_others()
        .show_all()
        .separator()
        .quit()
        .build()?;
    let edit = SubmenuBuilder::new(app, "Edit")
        .undo()
        .redo()
        .separator()
        .cut()
        .copy()
        .paste()
        .select_all()
        .build()?;
    let window = SubmenuBuilder::new(app, "Window")
        .item(&close_tab)
        .separator()
        .minimize()
        .maximize()
        .fullscreen()
        .build()?;
    let menu = MenuBuilder::new(app)
        .items(&[&app_menu, &edit, &window])
        .build()?;
    app.set_menu(menu)?;
    app.on_menu_event(|app, e| {
        if e.id() == "close-tab" {
            let _ = app.emit("toranj://close-tab", ());
        }
    });
    Ok(())
}

#[tauri::command]
fn tmux_available() -> bool {
    tmux::bin().exists()
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .manage(pty::Bridge::default())
        .setup(|app| {
            watch_spool(app.handle().clone());
            notifications::init(app.handle());
            install_menu(app)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_projects,
            snapshot,
            create_tab,
            kill_tab,
            peek_tab,
            answer_tab,
            log_ui,
            move_external,
            refresh_git,
            git_details,
            resources,
            stop_process,
            shutdown_simulators,
            attach,
            write,
            resize,
            detach,
            open_in_editor,
            play_sound,
            haptic,
            claude_statusline_status,
            install_claude_statusline,
            uninstall_claude_statusline,
            set_status_labels,
            icons::project_icon,
            history::recent_sessions,
            workspaces::workspace_files,
            notifications::notify_native,
            claude_hooks_status,
            install_claude_hooks,
            uninstall_claude_hooks,
            tmux_available
        ])
        .on_window_event(|window, event| {
            if let tauri::WindowEvent::Destroyed = event {
                // Only the attach clients die; tmux sessions (and agents) keep running.
                window.app_handle().state::<pty::Bridge>().detach_all();
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
