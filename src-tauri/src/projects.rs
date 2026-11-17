//! Finds git repositories in the usual project folders.

use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Serialize, Clone, Debug)]
pub struct Project {
    pub name: String,
    pub path: String,
}

const ROOTS: &[&str] = &[
    "Developer",
    "Projects",
    "code",
    "src",
    "dev",
    "Documents/Projects",
];
const SKIP: &[&str] = &[
    "node_modules",
    "build",
    "dist",
    "target",
    "Pods",
    ".dart_tool",
    "vendor",
    "Library",
];

fn walk(dir: &Path, depth: usize, out: &mut Vec<PathBuf>) {
    if dir.join(".git").exists() {
        out.push(dir.to_path_buf());
        return;
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || SKIP.contains(&name.as_str()) || name.contains("-wt-") {
            continue;
        }
        if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            walk(&p, depth - 1, out);
        }
    }
}

pub fn scan() -> Vec<Project> {
    let home = dirs::home_dir().unwrap_or_default();
    let mut found = Vec::new();
    for r in ROOTS {
        walk(&home.join(r), 4, &mut found);
    }
    found.sort();
    found.dedup();
    found
        .into_iter()
        .map(|p| {
            let name = match (p.parent().and_then(|x| x.file_name()), p.file_name()) {
                (Some(parent), Some(leaf)) => {
                    let (parent, leaf) = (parent.to_string_lossy(), leaf.to_string_lossy());
                    if leaf.to_lowercase().contains(&parent.to_lowercase())
                        || ROOTS.iter().any(|r| r.ends_with(&*parent))
                    {
                        leaf.into_owned()
                    } else {
                        format!("{parent}/{leaf}")
                    }
                }
                _ => p.to_string_lossy().into_owned(),
            };
            Project {
                name,
                path: p.to_string_lossy().into_owned(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore]
    fn live_projects() {
        println!(
            "PROJECTS_JSON={}",
            serde_json::to_string(&super::scan()).unwrap()
        );
    }
}
