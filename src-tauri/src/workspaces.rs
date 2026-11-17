//! VS Code workspace files (`*.code-workspace`) near the projects, so the
//! sidebar can group the folders a workspace lists.

use serde::Serialize;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct WorkspaceFile {
    /// The workspace file itself (its identity).
    pub file: String,
    /// Its name: the file name without `.code-workspace`.
    pub name: String,
    /// The folders it lists, as absolute paths (also resolved through
    /// symlinks when that gives a different path).
    pub folders: Vec<String>,
}

/// JSON with comments and trailing commas (what VS Code writes) to JSON.
fn strip_jsonc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            // Whole UTF-8 characters: paths can have non-ASCII names.
            let ch = s[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
            if c == b'\\' && i < b.len() {
                let next = s[i..].chars().next().unwrap_or(' ');
                out.push(next);
                i += next.len_utf8();
            } else if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
            out.push('"');
            i += 1;
        } else if c == b'/' && b.get(i + 1) == Some(&b'/') {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
        } else if c == b'/' && b.get(i + 1) == Some(&b'*') {
            i += 2;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                i += 1;
            }
            i += 2;
        } else if c == b',' {
            // Drop a comma that only closes an object or array (comments
            // between them included).
            let rest = skip_blank(&s[i + 1..]);
            if !(rest.starts_with('}') || rest.starts_with(']')) {
                out.push(',');
            }
            i += 1;
        } else {
            // Copy the whole UTF-8 character.
            let ch = s[i..].chars().next().unwrap_or(' ');
            out.push(ch);
            i += ch.len_utf8();
        }
    }
    out
}

/// `s` without leading whitespace and comments.
fn skip_blank(mut s: &str) -> &str {
    loop {
        s = s.trim_start();
        if let Some(r) = s.strip_prefix("//") {
            s = r.find('\n').map_or("", |n| &r[n..]);
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.find("*/").map_or("", |n| &r[n + 2..]);
        } else {
            return s;
        }
    }
}

/// `a/b/../c` to `a/c` without touching the disk.
fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

fn parse(file: &Path) -> Option<WorkspaceFile> {
    let text = std::fs::read_to_string(file).ok()?;
    if text.len() > 1_000_000 {
        return None;
    }
    let v: serde_json::Value = serde_json::from_str(&strip_jsonc(&text)).ok()?;
    let dir = file.parent()?;
    let mut folders = Vec::new();
    for f in v.get("folders")?.as_array()? {
        let Some(p) = f.get("path").and_then(|p| p.as_str()) else { continue };
        let abs = normalize(&dir.join(p));
        let s = abs.to_string_lossy().trim_end_matches('/').to_string();
        if let Ok(real) = std::fs::canonicalize(&abs) {
            let r = real.to_string_lossy().to_string();
            if r != s {
                folders.push(r);
            }
        }
        folders.push(s);
    }
    if folders.is_empty() {
        return None;
    }
    let name = file.file_name()?.to_str()?.trim_end_matches(".code-workspace").to_string();
    Some(WorkspaceFile { file: file.to_string_lossy().to_string(), name, folders })
}

/// Workspace files in each project's folder, its parent and grandparent.
#[tauri::command]
pub async fn workspace_files(paths: Vec<String>) -> Vec<WorkspaceFile> {
    tauri::async_runtime::spawn_blocking(move || {
        let mut found: BTreeMap<PathBuf, WorkspaceFile> = BTreeMap::new();
        for p in paths.iter().take(200) {
            let mut dir = Some(Path::new(p));
            for _ in 0..3 {
                let Some(d) = dir else { break };
                // Not the home folder or above: too broad to mean one product.
                if d.components().count() <= 3 {
                    break;
                }
                if let Ok(rd) = std::fs::read_dir(d) {
                    for e in rd.flatten().take(500) {
                        let f = e.path();
                        if f.extension().is_some_and(|x| x == "code-workspace") && !found.contains_key(&f)
                            && let Some(w) = parse(&f) {
                                found.insert(f, w);
                            }
                    }
                }
                dir = d.parent();
            }
        }
        found.into_values().collect()
    })
    .await
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_jsonc() {
        let s = "{\n // folders\n \"folders\": [ {\"path\": \"a//b\"}, /* x */ {\"path\": \"čćž \\\"q\\\"\"}, ],\n}";
        let v: serde_json::Value = serde_json::from_str(&strip_jsonc(s)).unwrap();
        assert_eq!(v["folders"][0]["path"], "a//b");
        let t = "{\"folders\": [{\"path\": \"a\"}, // last\n /* x */ ], /* y */ }";
        let w: serde_json::Value = serde_json::from_str(&strip_jsonc(t)).unwrap();
        assert_eq!(w["folders"][0]["path"], "a");
        assert_eq!(v["folders"][1]["path"], "čćž \"q\"");
    }

    #[test]
    fn resolves_folders_next_to_the_file() {
        let dir = std::env::temp_dir().join(format!("toranj-ws-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shop/api")).unwrap();
        std::fs::create_dir_all(dir.join("shop/app")).unwrap();
        let f = dir.join("shop/shop.code-workspace");
        std::fs::write(&f, r#"{"folders":[{"path":"api"},{"path":"./app/"},{"path":"../shop/api"}]}"#).unwrap();
        let w = parse(&f).unwrap();
        assert_eq!(w.name, "shop");
        let api = dir.join("shop/api").to_string_lossy().to_string();
        assert!(w.folders.contains(&api));
        assert!(w.folders.contains(&dir.join("shop/app").to_string_lossy().to_string()));
        std::fs::remove_dir_all(&dir).ok();
    }
}
