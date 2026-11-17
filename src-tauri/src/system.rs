//! Memory overview: total use, swap, and the usual heavy hitters of an
//! agent-heavy dev machine grouped into categories.

use serde::Serialize;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use sysinfo::{
    MemoryRefreshKind, Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System, UpdateKind,
};

/// One process in the details panel.
#[derive(Serialize, Clone, Debug)]
pub struct Proc {
    pub pid: u32,
    /// Short human name, e.g. "claude", "tsserver", "Gradle daemon".
    pub label: String,
    /// Working directory with the home folder shortened to ~, when known.
    pub cwd: String,
    pub bytes: u64,
    /// Processes summed into this row: an app with its helpers counts as one.
    pub count: u32,
    /// Toranj may stop it: the user's own process in a group where stopping
    /// a single process is safe (agents, language servers, daemons, …).
    pub killable: bool,
}

#[derive(Serialize, Clone, Debug)]
pub struct Group {
    pub name: String,
    pub bytes: u64,
    pub count: u32,
    /// Largest processes of the group first, at most [`TOP`].
    pub procs: Vec<Proc>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Resources {
    pub total: u64,
    pub used: u64,
    pub swap_total: u64,
    pub swap_used: u64,
    /// Largest groups first; only groups that exist.
    pub groups: Vec<Group>,
    /// Large processes outside every group, largest first.
    pub others: Vec<Proc>,
}

const TOP: usize = 8;
/// Ungrouped processes smaller than this are not worth listing.
const OTHER_MIN: u64 = 300 << 20;

/// Groups where stopping one process is safe and useful: the owner app keeps
/// working (an editor restarts its language server, Gradle starts a new
/// daemon) or the process is a standalone tool the user started.
fn group_killable(g: &str) -> bool {
    matches!(
        g,
        "AI agents" | "Language servers" | "Gradle/Kotlin" | "Android emulator"
    )
}

const AGENTS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "aider",
    "opencode",
    "cursor-agent",
    "amp",
];

fn base(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Label for a Claude helper process (`claude daemon`, `bg-pty-host`, …),
/// or None for a conversation.
fn helper_label(args: &[String]) -> Option<&'static str> {
    // `claude <sub>` or `node /path/claude <sub>`.
    let i = args.iter().take(2).position(|a| base(a) == "claude")?;
    Some(match args.get(i + 1).map(String::as_str)? {
        "daemon" => "Claude daemon (background sessions)",
        "bg-pty-host" => "Claude background terminal",
        "bg-spare" => "Claude bg-spare (spare background process)",
        "attach" => "claude attach (view of a background session)",
        "agents" => "claude agents",
        _ => return None,
    })
}

/// Short name shown for a process in the details panel.
fn label(group: Option<&str>, name: &str, args: &[String]) -> String {
    let c = args.join(" ").to_ascii_lowercase();
    match group {
        Some("Gradle/Kotlin") if c.contains("kotlin") => return "Kotlin daemon".into(),
        Some("Gradle/Kotlin") => return "Gradle daemon".into(),
        // Claude's background machinery runs as `claude <subcommand>`; name
        // it, so the list doesn't read as many separate agents.
        Some("AI agents") if helper_label(args).is_some() => {
            return helper_label(args).unwrap_or_default().into();
        }
        Some("AI agents") if !AGENTS.contains(&name.to_ascii_lowercase().as_str()) => {
            if name != "node" {
                return "claude".into();
            }
            if let Some(a) = args
                .iter()
                .skip(1)
                .take(2)
                .find(|a| AGENTS.contains(&base(a)))
            {
                return base(a).into();
            }
        }
        Some("Language servers") => {
            for (needle, l) in [
                ("tsserver", "tsserver (TypeScript)"),
                ("analysis_server", "Dart analyzer"),
                ("rust-analyzer", "rust-analyzer"),
                ("sourcekit-lsp", "sourcekit-lsp (Swift)"),
            ] {
                if c.contains(needle) || name == needle {
                    return l.into();
                }
            }
            if let Some(a) = args.iter().find(|a| a.contains("language-server")) {
                return base(a).trim_end_matches(".js").into();
            }
        }
        _ => {}
    }
    name.to_string()
}

/// Name of the outermost app bundle an executable lives in ("Spotify" for
/// /Applications/Spotify.app/Contents/Frameworks/Spotify Helper.app/…).
fn app_bundle(exe: &str) -> Option<&str> {
    let i = exe.find(".app/")?;
    Some(base(&exe[..i]))
}

/// System processes the user must not stop from here.
fn is_system(exe: &str) -> bool {
    exe.is_empty()
        || [
            "/System/",
            "/usr/libexec/",
            "/usr/sbin/",
            "/sbin/",
            "/Library/Apple/",
        ]
        .iter()
        .any(|p| exe.starts_with(p))
}

fn short_home(p: &str) -> String {
    match std::env::var("HOME") {
        Ok(h) if !h.is_empty() && p.starts_with(&h) => format!("~{}", &p[h.len()..]),
        _ => p.to_string(),
    }
}

/// Group for a process, from its name, executable path and arguments.
/// First match wins; language servers come before their editor so tsserver
/// or the Dart analyzer launched by VS Code count as language servers.
fn classify(name: &str, exe: &str, args: &[String]) -> Option<&'static str> {
    let n = name.to_ascii_lowercase();
    let e = exe.to_ascii_lowercase();
    let c = args.join(" ").to_ascii_lowercase();
    let any = |needle: &str| e.contains(needle) || c.contains(needle);

    if any("analysis_server")
        || any("language-server")
        || any("tsserver")
        || n == "rust-analyzer"
        || any("sourcekit-lsp")
    {
        return Some("Language servers");
    }
    if any("visual studio code.app") || any("/cursor.app/") || n.starts_with("code helper") {
        return Some("VS Code");
    }
    if c.contains("org.gradle.launcher.daemon")
        || c.contains("gradledaemon")
        || c.contains("kotlincompiledaemon")
        || c.contains("kotlin-daemon")
    {
        return Some("Gradle/Kotlin");
    }
    if n.starts_with("qemu-system") || e.contains("/emulator/") {
        return Some("Android emulator");
    }
    if n == "launchd_sim" || e.contains("coresimulator") || n == "simulator" {
        return Some("iOS simulator");
    }
    if e.contains("/orbstack.app/")
        || e.contains("/docker.app/")
        || matches!(
            n.as_str(),
            "dockerd" | "containerd" | "com.docker.backend" | "com.docker.vmnetd"
        )
        || n.contains("com.apple.virtualization")
    {
        return Some("Docker/VM");
    }
    // Agents installed with npm run as `node /path/to/claude …`; the native
    // Claude Code binary is named after its version (…/claude/versions/2.1.3).
    // Desktop apps such as Claude.app are not agents.
    if e.contains("/claude/versions/") {
        return Some("AI agents");
    }
    if (AGENTS.contains(&n.as_str()) && !e.contains(".app/"))
        || (n == "node"
            && args
                .iter()
                .skip(1)
                .take(2)
                .any(|a| AGENTS.contains(&base(a))))
    {
        return Some("AI agents");
    }
    if e.contains("google chrome")
        || e.contains("brave browser")
        || e.contains("firefox")
        || n == "safari"
    {
        return Some("Browsers");
    }
    None
}

static SYS: Mutex<Option<System>> = Mutex::new(None);

/// A process [`read`] offered to stop: its start time and executable, so
/// [`stop`] never signals a reused PID, and when it was last offered.
struct Offer {
    start: u64,
    exe: String,
    at: Instant,
}

/// How long an offer stays valid. A confirm dialog may stay open while later
/// reads drop the process from the list (it got smaller or was outranked).
const OFFER_TTL: Duration = Duration::from_secs(120);

/// Processes recent [`read`]s offered to stop; [`stop`] acts only on these.
static OFFERED: Mutex<Option<HashMap<u32, Offer>>> = Mutex::new(None);

/// Memory a process really costs. On macOS that is the "physical footprint"
/// (what Activity Monitor and `top` show): it includes compressed and swapped
/// pages, which RSS hides on a machine under memory pressure.
#[cfg(target_os = "macos")]
fn footprint(pid: u32, rss: u64) -> u64 {
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    let ok = unsafe {
        libc::proc_pid_rusage(
            pid as libc::c_int,
            libc::RUSAGE_INFO_V2,
            &mut info as *mut _ as *mut libc::rusage_info_t,
        )
    } == 0;
    if ok { info.ri_phys_footprint } else { rss }
}

#[cfg(not(target_os = "macos"))]
fn footprint(_pid: u32, rss: u64) -> u64 {
    rss
}

pub fn read() -> Result<Resources, String> {
    let mut guard = SYS.lock().unwrap_or_else(|e| e.into_inner());
    if guard.is_none() {
        *guard = Some(
            System::new_with_specifics(
                RefreshKind::nothing().with_memory(MemoryRefreshKind::everything()),
            )
            .map_err(|e| e.to_string())?,
        );
    }
    let sys = guard.as_mut().unwrap();
    sys.refresh_memory();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing()
            .with_memory()
            .with_cmd(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::OnlyIfNotSet)
            .with_cwd(UpdateKind::OnlyIfNotSet)
            .with_user(UpdateKind::OnlyIfNotSet),
    );

    let me = std::process::id();
    #[cfg(unix)]
    let uid = unsafe { libc::getuid() };
    let mut groups: Vec<Group> = Vec::new();
    let mut loose: HashMap<String, Proc> = HashMap::new();
    // The size of the process each summed row shows (its biggest one).
    let mut rep: HashMap<String, u64> = HashMap::new();
    let mut ids: HashMap<u32, (u64, String)> = HashMap::new();
    for p in sys.processes().values() {
        // Threads show up as processes on Linux; count each process once.
        if p.thread_kind().is_some() {
            continue;
        }
        let name = p.name().to_string_lossy();
        let args: Vec<String> = p
            .cmd()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let exe = p
            .exe()
            .map(|e| e.to_string_lossy().into_owned())
            .unwrap_or_default();
        let g = classify(&name, &exe, &args);
        let bytes = footprint(p.pid().as_u32(), p.memory());
        #[cfg(unix)]
        let own = p.user_id().is_some_and(|u| **u == uid);
        #[cfg(not(unix))]
        let own = false;
        let pid = p.pid().as_u32();
        let cwd = p
            .cwd()
            .map(|c| c.to_string_lossy().into_owned())
            .filter(|c| c != "/")
            .map(|c| short_home(&c))
            .unwrap_or_default();
        let Some(g) = g else {
            // Apps are summed with their helpers and only suggested to quit;
            // stopping one helper just breaks a window.
            let app = app_bundle(&exe);
            let key = app.unwrap_or(&name).to_string();
            let killable = own && pid != me && app.is_none() && !is_system(&exe);
            let e = loose.entry(key.clone()).or_insert(Proc {
                pid,
                label: key,
                cwd: String::new(),
                bytes: 0,
                count: 0,
                killable,
            });
            let biggest = rep.entry(e.label.clone()).or_insert(0);
            if bytes >= *biggest {
                // Show the biggest process's PID and folder for the row.
                *biggest = bytes;
                e.pid = pid;
                e.cwd = cwd;
                e.killable = killable;
            }
            e.bytes += bytes;
            e.count += 1;
            ids.insert(pid, (p.start_time(), exe.clone()));
            continue;
        };
        let proc = Proc {
            pid,
            label: label(Some(g), &name, &args),
            cwd,
            bytes,
            count: 1,
            killable: own && pid != me && group_killable(g),
        };
        ids.insert(pid, (p.start_time(), exe.clone()));
        match groups.iter_mut().find(|x| x.name == g) {
            Some(x) => {
                x.bytes += bytes;
                x.count += 1;
                x.procs.push(proc);
            }
            None => groups.push(Group {
                name: g.into(),
                bytes,
                count: 1,
                procs: vec![proc],
            }),
        }
    }
    let mut others: Vec<Proc> = loose
        .into_values()
        .filter(|p| p.bytes >= OTHER_MIN)
        .map(|mut p| {
            // A summed row stands for several processes; stop none of them.
            p.killable &= p.count == 1;
            p
        })
        .collect();
    groups.sort_by_key(|x| std::cmp::Reverse(x.bytes));
    for g in &mut groups {
        g.procs.sort_by_key(|x| std::cmp::Reverse(x.bytes));
        g.procs.truncate(TOP);
    }
    others.sort_by_key(|x| std::cmp::Reverse(x.bytes));
    others.truncate(TOP);
    // Only what is shown as stoppable can be stopped.
    let now = Instant::now();
    let mut offered = OFFERED.lock().unwrap_or_else(|e| e.into_inner());
    let offered = offered.get_or_insert_with(HashMap::new);
    offered.retain(|_, o| now.duration_since(o.at) < OFFER_TTL);
    for p in groups
        .iter()
        .flat_map(|g| g.procs.iter())
        .chain(others.iter())
        .filter(|p| p.killable)
    {
        if let Some((start, exe)) = ids.remove(&p.pid) {
            offered.insert(
                p.pid,
                Offer {
                    start,
                    exe,
                    at: now,
                },
            );
        }
    }

    Ok(Resources {
        total: sys.total_memory(),
        used: sys.used_memory(),
        swap_total: sys.total_swap(),
        swap_used: sys.used_swap(),
        groups,
        others,
    })
}

/// Politely stop (SIGTERM) a process a recent [`read`] offered as stoppable.
pub fn stop(pid: u32) -> Result<(), String> {
    let (start, exe) = OFFERED
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .and_then(|m| m.get(&pid))
        .filter(|o| o.at.elapsed() < OFFER_TTL)
        .map(|o| (o.start, o.exe.clone()))
        .ok_or("Toranj cannot close that process.")?;
    let mut guard = SYS.lock().unwrap_or_else(|e| e.into_inner());
    let sys = guard.as_mut().ok_or("Process data is not loaded.")?;
    let p = Pid::from_u32(pid);
    sys.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[p]),
        true,
        ProcessRefreshKind::nothing().with_exe(UpdateKind::Always),
    );
    match sys.process(p) {
        Some(proc)
            if proc.start_time() == start
                && proc
                    .exe()
                    .map(|e| e.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    == exe => {}
        _ => return Err("The process has already exited.".into()),
    }
    #[cfg(unix)]
    {
        if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
            return Err(std::io::Error::last_os_error().to_string());
        }
        Ok(())
    }
    #[cfg(not(unix))]
    Err("Not supported on this system.".into())
}

/// Shut down every booted iOS simulator.
pub fn shutdown_simulators() -> Result<(), String> {
    let out = std::process::Command::new("/usr/bin/xcrun")
        .args(["simctl", "shutdown", "all"])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_processes() {
        let a = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let vsc =
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Plugin).app/x";
        assert_eq!(
            classify("Code Helper (Plugin)", vsc, &a(&[vsc])),
            Some("VS Code")
        );
        assert_eq!(
            classify(
                "node",
                "/usr/bin/node",
                &a(&["node", "/x/node_modules/typescript/lib/tsserver.js"])
            ),
            Some("Language servers")
        );
        let gradle = a(&[
            "java",
            "--add-opens=java.base/java.util=ALL-UNNAMED",
            "-Xmx8G",
            "-Dfile.encoding=UTF-8",
            "-cp",
            "/x/gradle-launcher.jar",
            "org.gradle.launcher.daemon.bootstrap.GradleDaemon",
            "9.3.1",
        ]);
        assert_eq!(
            classify("java", "/jdk/bin/java", &gradle),
            Some("Gradle/Kotlin")
        );
        assert_eq!(
            classify("claude", "/x/claude", &a(&["claude"])),
            Some("AI agents")
        );
        assert_eq!(
            classify(
                "node",
                "/usr/bin/node",
                &a(&["node", "/usr/local/bin/codex"])
            ),
            Some("AI agents")
        );
        assert_eq!(
            classify("vim", "/usr/bin/vim", &a(&["vim", "docker-compose.yml"])),
            None
        );
        assert_eq!(classify("zsh", "/bin/zsh", &a(&["-zsh"])), None);
        let native = "/Users/x/.local/share/claude/versions/2.1.323";
        assert_eq!(
            classify("2.1.323", native, &a(&[native])),
            Some("AI agents")
        );
        assert_eq!(label(Some("AI agents"), "2.1.323", &a(&[native])), "claude");
        let desktop = "/Applications/Claude.app/Contents/MacOS/Claude";
        assert_eq!(classify("Claude", desktop, &a(&[desktop])), None);
        let ag = Some("AI agents");
        assert_eq!(
            label(ag, "claude", &a(&["/x/claude", "daemon", "run"])),
            "Claude daemon (background sessions)"
        );
        assert_eq!(
            label(ag, "claude", &a(&["claude", "bg-pty-host", "--x"])),
            "Claude background terminal"
        );
        assert_eq!(
            label(ag, "claude", &a(&["claude", "--resume", "abc"])),
            "claude"
        );
        assert_eq!(
            app_bundle("/Applications/Spotify.app/Contents/Frameworks/Spotify Helper.app/x"),
            Some("Spotify")
        );
    }

    #[test]
    #[ignore]
    fn live_resources() {
        let r = read().unwrap();
        println!("RESOURCES_JSON={}", serde_json::to_string(&r).unwrap());
    }
}
