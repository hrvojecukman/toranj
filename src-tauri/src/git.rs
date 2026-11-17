//! Git state per project: branch, uncommitted changes, ahead/behind.
//! Results are cached briefly so the 2 s status poll never runs git on every tick.

use serde::Serialize;
use std::collections::HashMap;
use std::process::Command;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct GitInfo {
    pub branch: String,
    /// Files with staged, unstaged or untracked changes.
    pub changed: u32,
    pub ahead: u32,
    pub behind: u32,
}

const TTL: Duration = Duration::from_secs(8);

/// When each project was last looked at, and what was found.
type Cache = HashMap<String, (Instant, Option<GitInfo>)>;
static CACHE: Mutex<Option<Cache>> = Mutex::new(None);

/// Homebrew git first: on a Mac without Command Line Tools, /usr/bin/git is a
/// shim that opens the install dialog every time it runs.
fn git_bin() -> &'static str {
    static BIN: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();
    BIN.get_or_init(|| {
        for p in ["/opt/homebrew/bin/git", "/usr/local/bin/git"] {
            if std::path::Path::new(p).exists() {
                return p;
            }
        }
        let clt = Command::new("/usr/bin/xcode-select")
            .arg("-p")
            .output()
            .is_ok_and(|o| o.status.success());
        if clt || !cfg!(target_os = "macos") {
            "/usr/bin/git"
        } else {
            ""
        }
    })
}

/// Parses `git status --porcelain=v2 --branch`.
pub fn parse(out: &str) -> GitInfo {
    let mut info = GitInfo::default();
    for line in out.lines() {
        if let Some(head) = line.strip_prefix("# branch.head ") {
            info.branch = if head == "(detached)" {
                "detached".into()
            } else {
                head.into()
            };
        } else if let Some(ab) = line.strip_prefix("# branch.ab ") {
            for part in ab.split_whitespace() {
                if let Some(n) = part.strip_prefix('+') {
                    info.ahead = n.parse().unwrap_or(0);
                } else if let Some(n) = part.strip_prefix('-') {
                    info.behind = n.parse().unwrap_or(0);
                }
            }
        } else if !line.starts_with('#') && !line.is_empty() {
            info.changed += 1;
        }
    }
    info
}

const TIMEOUT: Duration = Duration::from_secs(3);

/// Runs git with a hard timeout; a hung git (network drive, huge tree) is killed.
fn git(path: &str, args: &[&str]) -> Option<String> {
    let bin = git_bin();
    if bin.is_empty() {
        return None;
    }
    let mut child = Command::new(bin)
        .env("GIT_TERMINAL_PROMPT", "0")
        // Don't take index.lock: an agent's own git commands must never fail
        // because Toranj happened to refresh. Stat-dirty files get rehashed
        // instead, which costs a little on very large repos.
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args(["-c", "core.fsmonitor=false", "-C", path])
        .args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    let reader = std::thread::spawn(move || {
        // Bytes, not read_to_string: one non-UTF-8 file name must not turn
        // the whole output into "" (which would read as a clean tree).
        let mut buf = Vec::new();
        std::io::Read::read_to_end(&mut stdout, &mut buf).ok();
        String::from_utf8_lossy(&buf).into_owned()
    });
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let out = reader.join().ok()?;
                return status.success().then_some(out);
            }
            Ok(None) if start.elapsed() < TIMEOUT => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// A repo can define clean/smudge filter programs in its own `.git/config`
/// and switch them on via `.gitattributes`; `git status` then runs them. So
/// a repo with repo-local filters only gets its branch shown, read without
/// touching the work tree. Filters from the user's global or system config
/// (e.g. git-lfs) are the user's own and are fine.
fn has_local_filters(path: &str) -> bool {
    match git(
        path,
        &["config", "--show-scope", "--get-regexp", "^filter\\."],
    ) {
        Some(out) => out.lines().any(|l| {
            l.starts_with("local") || l.starts_with("worktree") || l.starts_with("command")
        }),
        None => false, // exit 1: no filter.* keys at all
    }
}

fn read(path: &str) -> Option<GitInfo> {
    if has_local_filters(path) {
        let branch = git(path, &["symbolic-ref", "--short", "-q", "HEAD"])
            .map(|b| b.trim().to_string())
            .filter(|b| !b.is_empty())
            .unwrap_or_else(|| "detached".into());
        return Some(GitInfo {
            branch,
            ..Default::default()
        });
    }
    git(
        path,
        &[
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
        ],
    )
    .map(|out| parse(&out))
}

pub fn info(path: &str) -> Option<GitInfo> {
    {
        let cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, v)) = cache.as_ref().and_then(|c| c.get(path))
            && at.elapsed() < TTL {
                return v.clone();
            }
    }
    let v = read(path);
    CACHE
        .lock()
        .unwrap()
        .get_or_insert_with(HashMap::new)
        .insert(path.to_string(), (Instant::now(), v.clone()));
    v
}

/// Drops cache entries for projects that no longer have tabs.
pub fn retain(keep: &std::collections::HashSet<String>) {
    if let Some(c) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        c.retain(|k, _| keep.contains(k));
    }
}

/// Forget cached state, e.g. after an agent finished and likely changed files.
pub fn invalidate(path: &str) {
    if let Some(c) = CACHE.lock().unwrap_or_else(|e| e.into_inner()).as_mut() {
        c.remove(path);
    }
}

// ---------- details panel ----------
// Everything below runs only when the user opens the Git panel, never on the
// status poll. Only `status` can run repo-defined programs (filters), so it is
// guarded the same way as above; log, rev-list, for-each-ref, worktree and
// stash listings only read refs and objects.

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct FileChange {
    /// Porcelain XY code ("M.", ".M", "A.", "??", "UU", …).
    pub code: String,
    pub path: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub time: u64,
    pub author: String,
    pub subject: String,
    /// Reachable from some remote-tracking ref.
    pub pushed: bool,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Worktree {
    pub path: String,
    /// Empty when detached.
    pub branch: String,
    pub head: String,
    pub current: bool,
    pub locked: bool,
    /// Folder is gone; `git worktree prune` would drop it.
    pub prunable: bool,
    /// Changed files, or None when status was not read (missing folder,
    /// repo-local filters, timeout).
    pub changed: Option<u32>,
    /// Commits at its HEAD that are on no remote; None if unknown.
    pub unpushed: Option<u32>,
    /// HEAD is contained in the base branch.
    pub merged: bool,
    #[serde(skip)]
    pub oid: String,
}

#[derive(Serialize, Clone, Debug, Default, PartialEq)]
pub struct Branch {
    pub name: String,
    pub upstream: String,
    /// The upstream branch was deleted on the remote.
    pub gone: bool,
    pub ahead: u32,
    pub behind: u32,
    /// Commits not on any remote; None if it could not be counted.
    pub unpushed: Option<u32>,
    /// Fully contained in the base branch.
    pub merged: bool,
    pub time: u64,
    /// Path of the worktree that has it checked out, if any.
    pub worktree: String,
}

#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Stash {
    pub name: String,
    pub time: u64,
    pub subject: String,
}

#[derive(Serialize, Clone, Debug, Default)]
pub struct Details {
    pub root: String,
    pub branch: String,
    pub upstream: String,
    pub ahead: u32,
    pub behind: u32,
    /// Branch others are merged into, e.g. "origin/main"; empty if unknown.
    pub base: String,
    pub has_remote: bool,
    /// Base without the "origin/" prefix, e.g. "main".
    pub base_local: String,
    /// Status was not read (repo-local filters or timeout).
    pub status_skipped: bool,
    /// Why: "filters" or "failed".
    pub skip_reason: String,
    /// Commits on HEAD that are on no remote (works when detached); None if unknown.
    pub unpushed: Option<u32>,
    /// The per-commit `pushed` flags are reliable.
    pub push_known: bool,
    /// More local branches exist than are listed.
    pub branches_truncated: bool,
    pub changed: u32,
    pub files: Vec<FileChange>,
    pub commits: Vec<Commit>,
    pub worktrees: Vec<Worktree>,
    pub branches: Vec<Branch>,
    pub stashes: Vec<Stash>,
    pub last_fetch: Option<u64>,
}

const MAX_FILES: usize = 300;
const MAX_COMMITS: usize = 20;
const MAX_BRANCHES: usize = 60;

/// Changed paths from `git status --porcelain=v2`.
pub fn parse_files(out: &str) -> Vec<FileChange> {
    let mut v = Vec::new();
    for line in out.lines() {
        let (code, path) = match line.as_bytes().first() {
            Some(b'1') => match line.splitn(9, ' ').collect::<Vec<_>>()[..] {
                [_, xy, .., p] => (xy, p),
                _ => continue,
            },
            Some(b'2') => match line.splitn(10, ' ').collect::<Vec<_>>()[..] {
                // "<path>\t<origPath>": show the new name.
                [_, xy, .., p] => (xy, p.split('\t').next().unwrap_or(p)),
                _ => continue,
            },
            Some(b'u') => match line.splitn(11, ' ').collect::<Vec<_>>()[..] {
                [_, xy, .., p] => (xy, p),
                _ => continue,
            },
            Some(b'?') => ("??", line.get(2..).unwrap_or("")),
            _ => continue,
        };
        v.push(FileChange {
            code: code.to_string(),
            path: path.to_string(),
        });
    }
    v
}

/// Parses `git worktree list --porcelain`.
pub fn parse_worktrees(out: &str) -> Vec<Worktree> {
    let mut v: Vec<Worktree> = Vec::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("worktree ") {
            v.push(Worktree {
                path: p.to_string(),
                ..Default::default()
            });
            continue;
        }
        let Some(w) = v.last_mut() else { continue };
        if let Some(h) = line.strip_prefix("HEAD ") {
            w.head = h.chars().take(7).collect();
            w.oid = h.to_string();
        } else if let Some(b) = line.strip_prefix("branch ") {
            w.branch = b.strip_prefix("refs/heads/").unwrap_or(b).to_string();
        } else if line == "locked" || line.starts_with("locked ") {
            w.locked = true;
        } else if line == "prunable" || line.starts_with("prunable ") {
            w.prunable = true;
        }
    }
    // A bare main repo has no work tree to report.
    v.retain(|w| !w.head.is_empty() || !w.branch.is_empty());
    v
}

/// Parses `for-each-ref` lines of
/// `name \t upstream \t track \t committerdate \t worktreepath`.
pub fn parse_branches(out: &str) -> Vec<Branch> {
    let mut v = Vec::new();
    for line in out.lines() {
        let f: Vec<&str> = line.splitn(5, '\t').collect();
        if f.len() < 4 || f[0].is_empty() {
            continue;
        }
        let track = f[2];
        let mut b = Branch {
            name: f[0].to_string(),
            upstream: f[1].to_string(),
            gone: track.contains("gone"),
            time: f[3].parse().unwrap_or(0),
            worktree: f.get(4).unwrap_or(&"").to_string(),
            ..Default::default()
        };
        for part in track.trim_matches(|c| c == '[' || c == ']').split(", ") {
            if let Some(n) = part.strip_prefix("ahead ") {
                b.ahead = n.parse().unwrap_or(0);
            } else if let Some(n) = part.strip_prefix("behind ") {
                b.behind = n.parse().unwrap_or(0);
            }
        }
        v.push(b);
    }
    v
}

fn same_path(a: &str, b: &str) -> bool {
    let c = |p: &str| std::fs::canonicalize(p).unwrap_or_else(|_| p.into());
    c(a) == c(b)
}

/// Status of one work tree. Err("filters") when it must not be read
/// (repo-local filters), Err("failed") on error or timeout.
fn status_of(path: &str) -> Result<String, &'static str> {
    if has_local_filters(path) {
        return Err("filters");
    }
    git(
        path,
        &[
            "-c",
            "core.quotePath=false",
            "status",
            "--porcelain=v2",
            "--branch",
            "--untracked-files=normal",
        ],
    )
    .ok_or("failed")
}

/// Commits reachable from `rev` that no remote-tracking ref has.
fn count_unpushed(root: &str, rev: &str) -> Option<u32> {
    git(root, &["rev-list", "--count", rev, "--not", "--remotes"])?
        .trim()
        .parse()
        .ok()
}

pub fn details(path: &str) -> Result<Details, String> {
    if git_bin().is_empty() {
        return Err("git is not installed".into());
    }
    let root = git(path, &["rev-parse", "--show-toplevel"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or("this folder is not a git repository")?;
    let mut d = Details {
        root: root.clone(),
        ..Default::default()
    };

    match status_of(&root) {
        Ok(out) => {
            let info = parse(&out);
            d.changed = info.changed;
            d.files = parse_files(&out).into_iter().take(MAX_FILES).collect();
        }
        Err(why) => {
            d.status_skipped = true;
            d.skip_reason = why.into();
        }
    }
    // Branch and tracking come from refs, so they show even without status.
    if let Some(head) = git(&root, &["symbolic-ref", "--short", "-q", "HEAD"]) {
        d.branch = head.trim().to_string();
    }
    if d.branch.is_empty() {
        d.branch = "detached".into();
    }

    d.has_remote = git(&root, &["remote"]).is_some_and(|r| !r.trim().is_empty());
    d.base = git(&root, &["symbolic-ref", "-q", "--short", "refs/remotes/origin/HEAD"])
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .or_else(|| {
            ["origin/main", "origin/master", "main", "master"]
                .into_iter()
                .find(|r| {
                    git(&root, &["rev-parse", "--verify", "-q", &format!("{r}^{{commit}}")])
                        .is_some()
                })
                .map(String::from)
        })
        .unwrap_or_default();
    d.base_local = d.base.strip_prefix("origin/").unwrap_or(&d.base).to_string();

    // --path-format needs git 2.31; older git prints a path relative to root.
    let common = git(&root, &["rev-parse", "--path-format=absolute", "--git-common-dir"])
        .or_else(|| git(&root, &["rev-parse", "--git-common-dir"]))
        .map(|s| std::path::Path::new(&root).join(s.trim()));
    if let Some(dir) = common {
        d.last_fetch = std::fs::metadata(dir.join("FETCH_HEAD"))
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|t| t.as_secs());
    }

    // Commits on HEAD, each marked pushed or not.
    let unpushed: Option<std::collections::HashSet<String>> = if d.has_remote {
        git(&root, &["rev-list", "-n", "500", "HEAD", "--not", "--remotes"])
            .map(|o| o.lines().map(String::from).collect())
    } else {
        None
    };
    d.push_known = unpushed.is_some();
    if d.has_remote {
        d.unpushed = count_unpushed(&root, "HEAD");
    }
    let log = git(
        &root,
        &[
            "log",
            "-n",
            &MAX_COMMITS.to_string(),
            "--no-show-signature",
            "--format=%H%x09%ct%x09%an%x09%s",
            "HEAD",
            "--",
        ],
    )
    .unwrap_or_default();
    d.commits = log
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(4, '\t').collect();
            (f.len() == 4).then(|| Commit {
                hash: f[0].chars().take(7).collect(),
                time: f[1].parse().unwrap_or(0),
                author: f[2].to_string(),
                subject: f[3].to_string(),
                pushed: unpushed.as_ref().is_some_and(|u| !u.contains(f[0])),
            })
        })
        .collect();

    // Local branches with tracking, merge and push state. All refs are read
    // (cheap); the list keeps the newest plus any checked out somewhere.
    let refs = git(
        &root,
        &[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:lstrip=2)%09%(upstream:short)%09%(upstream:track)%09%(committerdate:unix)%09%(worktreepath)",
            "refs/heads",
        ],
    )
    .unwrap_or_default();
    let all = parse_branches(&refs);
    let total = all.len();
    d.branches = all
        .into_iter()
        .enumerate()
        .filter(|(i, b)| *i < MAX_BRANCHES || !b.worktree.is_empty() || b.name == d.branch)
        .map(|(_, b)| b)
        .collect();
    d.branches_truncated = d.branches.len() < total;
    // Unknown merge state reads as "not merged": never suggests deleting.
    let merged: std::collections::HashSet<String> = if d.base.is_empty() {
        Default::default()
    } else {
        git(
            &root,
            &[
                "for-each-ref",
                &format!("--merged={}", d.base),
                "--format=%(refname:lstrip=2)",
                "refs/heads",
            ],
        )
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
    };
    std::thread::scope(|s| {
        for b in d.branches.iter_mut() {
            b.merged = merged.contains(&b.name);
            if d.has_remote {
                let root = &root;
                s.spawn(move || b.unpushed = count_unpushed(root, &format!("refs/heads/{}", b.name)));
            } else {
                b.unpushed = None;
            }
        }
    });
    if let Some(cur) = d.branches.iter().find(|b| b.name == d.branch) {
        d.upstream = cur.upstream.clone();
        d.ahead = cur.ahead;
        d.behind = cur.behind;
    }

    // Worktrees, each with its own status and push state read in parallel.
    d.worktrees = parse_worktrees(&git(&root, &["worktree", "list", "--porcelain"]).unwrap_or_default());
    let (has_remote, base, changed, skipped) = (d.has_remote, d.base.clone(), d.changed, d.status_skipped);
    std::thread::scope(|s| {
        for w in d.worktrees.iter_mut() {
            w.current = same_path(&w.path, &root);
            if !w.current && (w.prunable || !std::path::Path::new(&w.path).is_dir()) {
                w.prunable = true;
                continue;
            }
            let (root, base) = (&root, &base);
            s.spawn(move || {
                w.changed = if w.current {
                    (!skipped).then_some(changed)
                } else {
                    status_of(&w.path).ok().map(|o| parse(&o).changed)
                };
                if !w.oid.is_empty() {
                    if has_remote {
                        w.unpushed = count_unpushed(root, &w.oid);
                    }
                    w.merged = !base.is_empty()
                        && git(root, &["merge-base", "--is-ancestor", &w.oid, base]).is_some();
                }
            });
        }
    });

    d.stashes = git(&root, &["stash", "list", "--format=%gd%x09%ct%x09%gs"])
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.splitn(3, '\t').collect();
            (f.len() == 3).then(|| Stash {
                name: f[0].to_string(),
                time: f[1].parse().unwrap_or(0),
                subject: f[2].to_string(),
            })
        })
        .collect();
    Ok(d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_counts_and_ab() {
        let out = "# branch.oid abc\n# branch.head hc/feat/x\n# branch.upstream origin/hc/feat/x\n# branch.ab +2 -1\n1 .M N... 100644 100644 100644 a b src/a.rs\n? new.txt\n";
        assert_eq!(
            parse(out),
            GitInfo {
                branch: "hc/feat/x".into(),
                changed: 2,
                ahead: 2,
                behind: 1
            }
        );
    }

    #[test]
    fn detached_and_clean() {
        let out = "# branch.oid abc\n# branch.head (detached)\n";
        assert_eq!(
            parse(out),
            GitInfo {
                branch: "detached".into(),
                ..Default::default()
            }
        );
    }

    #[test]
    fn hostile_repo_filter_never_runs() {
        use std::process::Command;
        let dir = std::env::temp_dir().join(format!("toranj-git-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |cmd: &str| {
            assert!(
                Command::new("sh")
                    .arg("-c")
                    .arg(cmd)
                    .current_dir(&dir)
                    .status()
                    .unwrap()
                    .success()
            )
        };
        sh("git init -q -b main . && git config user.email t@t && git config user.name t");
        sh("echo 'a.txt filter=evil' > .gitattributes && echo hi > a.txt");
        sh("git config filter.evil.clean 'sh -c \"touch PWNED; cat\"'");
        sh("git add -A && rm -f PWNED && git commit -qm init && rm -f PWNED && touch a.txt");

        let path = dir.to_string_lossy().into_owned();
        let info = read(&path).expect("branch is still shown");
        assert_eq!(info.branch, "main");
        assert!(
            !dir.join("PWNED").exists(),
            "repo-local filter must not run"
        );

        sh("git config --unset filter.evil.clean && echo change >> a.txt");
        assert_eq!(
            read(&path).unwrap().changed,
            1,
            "normal repos get full status"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_changed_files() {
        let out = "# branch.head main\n1 .M N... 100644 100644 100644 a b src/a b.rs\n2 R. N... 100644 100644 100644 a b R100 new.rs\told.rs\nu UU N... 100644 100644 100644 100644 a b c conflict.rs\n? notes.txt\n";
        let f = parse_files(out);
        let got: Vec<(&str, &str)> = f.iter().map(|c| (c.code.as_str(), c.path.as_str())).collect();
        assert_eq!(
            got,
            vec![(".M", "src/a b.rs"), ("R.", "new.rs"), ("UU", "conflict.rs"), ("??", "notes.txt")]
        );
    }

    #[test]
    fn parses_worktrees_and_branches() {
        let wt = "worktree /r\nHEAD 1234567890\nbranch refs/heads/main\n\nworktree /r/wt\nHEAD abcdef0123\ndetached\nlocked\n\nworktree /gone\nHEAD ffffffff\nbranch refs/heads/x\nprunable gitdir file points to non-existent location\n";
        let w = parse_worktrees(wt);
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].branch.as_str(), w[0].head.as_str()), ("main", "1234567"));
        assert!(w[1].branch.is_empty() && w[1].locked);
        assert!(w[2].prunable);

        let refs = "main\torigin/main\t[behind 6]\t100\t/r\nold\torigin/old\t[gone]\t50\t\nfeat\torigin/feat\t[ahead 2, behind 1]\t70\t\nlocal\t\t\t60\t\n";
        let b = parse_branches(refs);
        assert_eq!((b[0].behind, b[0].worktree.as_str()), (6, "/r"));
        assert!(b[1].gone);
        assert_eq!((b[2].ahead, b[2].behind), (2, 1));
        assert!(b[3].upstream.is_empty() && !b[3].gone);
    }
}
#[cfg(test)]
mod live {
    // GIT_LIVE=<repo> cargo test live -- --nocapture prints the panel data.
    // Skipped without the variable.
    #[test]
    fn details_of_a_real_repo() {
        let Ok(p) = std::env::var("GIT_LIVE") else { return };
        let t = std::time::Instant::now();
        let d = super::details(&p).unwrap();
        println!("{}\n{:?}", serde_json::to_string_pretty(&d).unwrap(), t.elapsed());
    }
}
