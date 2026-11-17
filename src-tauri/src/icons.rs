//! A project's own icon (app icon, favicon), so the sidebar can show it.

use std::path::{Path, PathBuf};

/// Where projects keep a square icon, best first: app icons, then favicons.
const CANDIDATES: &[&str] = &[
    "web/icons/Icon-192.png",
    "public/icon-192.png",
    "public/icon-512.png",
    "public/apple-touch-icon.png",
    "src-tauri/icons/128x128.png",
    "app/icon.png",
    "src/app/icon.png",
    "app/apple-icon.png",
    "src/app/apple-icon.png",
    "assets/icon.png",
    "assets/images/icon.png",
    "assets/icon/icon.png",
    "android/app/src/main/res/mipmap-xxxhdpi/ic_launcher.png",
    "android/app/src/main/res/mipmap-xxhdpi/ic_launcher.png",
    "ios/Runner/Assets.xcassets/AppIcon.appiconset/Icon-App-1024x1024@1x.png",
    "public/favicon.svg",
    "public/favicon.png",
    "web/favicon.png",
    "static/favicon.png",
    "favicon.svg",
    "favicon.png",
    "icon.png",
    "public/favicon.ico",
    "app/favicon.ico",
    "src/app/favicon.ico",
    "favicon.ico",
];

/// Folders whose PNGs are all app icons (e.g. `assets/icon/app-icon-1024.png`).
const ICON_DIRS: &[&str] = &["assets/icon", "assets/icons"];

const MAX_BYTES: u64 = 1_500_000;

/// The file is inside `dir` for real: a symlink in a cloned repo must not
/// turn some other file (a key, say) into an "icon" shown in the webview.
fn inside(p: &Path, dir: &Path) -> bool {
    match (std::fs::canonicalize(p), std::fs::canonicalize(dir)) {
        (Ok(p), Ok(d)) => p.starts_with(d),
        _ => false,
    }
}

fn find_in(dir: &Path) -> Option<PathBuf> {
    for c in CANDIDATES {
        let p = dir.join(c);
        if p.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_BYTES) && inside(&p, dir) {
            return Some(p);
        }
    }
    for d in ICON_DIRS {
        let Ok(rd) = std::fs::read_dir(dir.join(d)) else { continue };
        let mut pngs: Vec<PathBuf> = rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("png")))
            .filter(|p| p.metadata().is_ok_and(|m| m.is_file() && m.len() <= MAX_BYTES) && inside(p, dir))
            .collect();
        pngs.sort();
        if let Some(p) = pngs.into_iter().next() {
            return Some(p);
        }
    }
    None
}

/// The project's folder first, then its direct subfolders (a website or app
/// inside a monorepo).
fn find(root: &Path) -> Option<PathBuf> {
    if let Some(p) = find_in(root) {
        return Some(p);
    }
    let mut subs: Vec<PathBuf> = std::fs::read_dir(root)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| {
            let n = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            !n.starts_with('.') && !matches!(n, "node_modules" | "target" | "build" | "dist" | "out" | "docs")
        })
        .collect();
    subs.sort();
    subs.iter().take(40).find_map(|d| find_in(d)).or_else(|| find_sibling(root))
}

/// A backend often has no icon while its app next to it does:
/// `Shop/shop-backend` takes the icon of `Shop/shop`.
/// Only folders named after the project's first word count.
fn find_sibling(root: &Path) -> Option<PathBuf> {
    let leaf = root.file_name()?.to_str()?.to_ascii_lowercase();
    let word = leaf.split(|c: char| !c.is_ascii_alphanumeric()).next()?.to_string();
    if word.len() < 3 || word == leaf {
        return None;
    }
    let mut sibs: Vec<(bool, PathBuf)> = std::fs::read_dir(root.parent()?)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .filter(|p| p != root)
        .filter_map(|p| {
            let n = p.file_name()?.to_str()?.to_ascii_lowercase();
            // The word whole: "web-api" doesn't take the icon of "website".
            let whole = n.strip_prefix(&word).is_some_and(|r| !r.starts_with(|c: char| c.is_ascii_alphanumeric()));
            (!n.starts_with('.') && whole).then_some((n != word, p))
        })
        .collect();
    sibs.sort(); // the folder named exactly like the word first
    sibs.iter().take(10).find_map(|(_, d)| find_in(d))
}

fn mime(p: &Path) -> &'static str {
    match p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref() {
        Some("svg") => "image/svg+xml",
        Some("ico") => "image/x-icon",
        _ => "image/png",
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// The project's icon as a data URL, or None when it has none.
#[tauri::command]
pub async fn project_icon(path: String) -> Option<String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = Path::new(&path);
        if !root.is_dir() {
            return None;
        }
        let p = find(root)?;
        let data = std::fs::read(&p).ok()?;
        Some(format!("data:{};base64,{}", mime(&p), base64(&data)))
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_base64() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn prefers_app_icons_and_looks_one_level_down() {
        let dir = std::env::temp_dir().join(format!("toranj-icons-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("site/src/app")).unwrap();
        std::fs::write(dir.join("site/src/app/icon.png"), b"x").unwrap();
        assert_eq!(find(&dir), Some(dir.join("site/src/app/icon.png")));
        std::fs::create_dir_all(dir.join("public")).unwrap();
        std::fs::write(dir.join("public/favicon.ico"), b"x").unwrap();
        std::fs::write(dir.join("public/icon-192.png"), b"x").unwrap();
        assert_eq!(find(&dir), Some(dir.join("public/icon-192.png")));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn borrows_the_icon_of_a_sibling_app() {
        let dir = std::env::temp_dir().join(format!("toranj-icons-sib-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shop-backend/src")).unwrap();
        std::fs::create_dir_all(dir.join("shop/web/icons")).unwrap();
        std::fs::create_dir_all(dir.join("other/web/icons")).unwrap();
        std::fs::write(dir.join("shop/web/icons/Icon-192.png"), b"x").unwrap();
        std::fs::write(dir.join("other/web/icons/Icon-192.png"), b"x").unwrap();
        std::fs::create_dir_all(dir.join("shopify/web/icons")).unwrap();
        std::fs::write(dir.join("shopify/web/icons/Icon-192.png"), b"x").unwrap();
        assert_eq!(find(&dir.join("shop-backend")), Some(dir.join("shop/web/icons/Icon-192.png")));
        // A single-word project doesn't borrow.
        std::fs::create_dir_all(dir.join("lone")).unwrap();
        assert_eq!(find(&dir.join("lone")), None);
        std::fs::remove_dir_all(&dir).ok();
    }
}
