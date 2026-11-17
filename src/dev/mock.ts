// Dev-only: render the UI in a plain browser (http://localhost:1420/?mock) with
// data dumped by scripts/dump-mock.sh, so it can be screenshotted headlessly.
import { mockIPC, mockWindows } from "@tauri-apps/api/mocks";

export async function installMock() {
  const params = new URLSearchParams(location.search);
  // ?demo: fictional, committed sample data (for README screenshots).
  const data = await (await fetch(params.has("demo") ? "/src/dev/demo-data.json" : "/src/dev/mock-data.json")).json();
  const enc = new TextEncoder();
  mockWindows("main"); // setBadge reads the current window
  // ?wait: the 2nd and 3rd tabs wait for an answer (waiting dock). ?ov: overview.
  if (params.has("wait")) {
    data.snapshot.tabs[1].state = "waiting";
    data.snapshot.tabs[1].detail = "Bash: git push origin feat/contracts";
    data.snapshot.tabs[2].state = "waiting";
    data.snapshot.tabs[2].detail = "";
  }
  // ?bg: a background session in the first tab's project (shown under it).
  if (params.has("bg")) {
    const t = data.snapshot.tabs[0];
    data.snapshot.external.unshift({ name: "nightly-audit", cwd: t.project || t.cwd, state: "running", detail: "", title: "Nightly dependency audit", session_id: "bg-1", background: true, unlisted: false, pid: 1 });
  }
  // ?done: three tabs finish (one with an error) a few seconds in.
  if (params.has("done")) {
    for (const i of [3, 4, 5]) data.snapshot.tabs[i].state = "running";
    setTimeout(() => {
      data.snapshot.tabs[3].state = "idle";
      data.snapshot.tabs[4].state = "idle";
      data.snapshot.tabs[5].state = "error";
      data.snapshot.tabs[5].detail = "API Error: overloaded";
    }, 2500);
  }
  mockIPC((cmd, args: any) => {
    switch (cmd) {
      case "peek_tab": return args.name === data.snapshot.tabs[1].name
        ? { lines: ["Bash command", "  git push origin feat/contracts", "Do you want to proceed?"], prompt: { context: ["git push origin feat/contracts", "Push the contracts branch", "Do you want to proceed?"], fingerprint: "f1", choices: [{ key: "1", label: "Yes" }, { key: "2", label: "Yes, and don't ask again for git push commands" }, { key: "3", label: "No, and tell Claude what to do differently (esc)" }] } }
        : { lines: ["Which database should I use for staging?"], prompt: null };
      case "answer_tab": return null;
      case "claude_statusline_status": return { installed: false, other: false, settings_path: "/Users/you/.claude/settings.json" };
      // ?ws: the first two other projects share a VS Code workspace.
      case "workspace_files": {
        if (!params.has("ws")) return [];
        const keys = [...new Set(data.snapshot.tabs.map((t: any) => t.project || t.cwd))].sort() as string[];
        return [{ file: "/demo/Studio.code-workspace", name: "Studio", folders: keys.slice(1, 3) }];
      }
      // A sample icon for projects whose name has an "a" in it; the rest show their initial.
      case "project_icon": return /a[^/]*$/.test(args.path) ? `data:image/svg+xml,${encodeURIComponent('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 16 16"><rect width="16" height="16" rx="3" fill="#2b6cb0"/><circle cx="8" cy="8" r="4" fill="#f6c343"/></svg>')}` : null;
      case "tmux_available": return true;
      case "list_projects": return data.projects;
      case "snapshot": return data.snapshot;
      case "resources": return data.resources ?? null;
      case "git_details": return params.get("git") === "clean" ? GIT_CLEAN : GIT_MESSY;
      case "claude_hooks_status":
        return { installed: params.has("hooks-on"), hook_path: "/Applications/Toranj.app/Contents/MacOS/toranj-hook", hook_exists: true, settings_path: "/Users/you/.claude/settings.json", preview: "{ \"hooks\": { … } }" };
      case "attach": {
        const screen: string = data.screens[args.name] ?? "";
        const bytes = enc.encode(screen.replace(/\n/g, "\r\n")).buffer;
        setTimeout(() => (window as any).__TAURI_INTERNALS__.runCallback(args.onData.id, { index: 0, message: bytes }), 50);
        return 1;
      }
      default: return null;
    }
  });
  const tab = params.get("tab");
  if (tab) setTimeout(() => [...document.querySelectorAll<HTMLButtonElement>("#tabs .sess")].find((b) => b.textContent?.startsWith(tab))?.click(), 500);
  if (params.has("hooks-dialog")) setTimeout(() => document.getElementById("hooks-btn")?.click(), 600);
  if (params.has("res")) setTimeout(() => {
    document.getElementById("res-sum")?.click();
    const g = params.get("res");
    if (g) document.querySelector<HTMLDetailsElement>(`#res-body details[data-g="${CSS.escape(g)}"] summary`)?.click();
    if (params.has("res-end")) setTimeout(() => { const b = document.getElementById("res-body")!; b.scrollTop = b.scrollHeight; }, 100);
  }, 1200);
  if (params.has("unlisted")) setTimeout(() => [...document.querySelectorAll<HTMLButtonElement>("#external .sess")].find((b) => b.textContent?.includes("manual"))?.click(), 900);
  if (params.has("git")) setTimeout(() => {
    document.getElementById("h-git")?.click();
    if (params.has("git-end")) setTimeout(() => { const b = document.getElementById("git-body")!; b.scrollTop = b.scrollHeight; }, 300);
  }, 1200);
  if (params.has("pal")) setTimeout(() => {
    document.getElementById("pal-btn")?.click();
    const q = document.getElementById("pal-q") as HTMLInputElement;
    q.value = params.get("pal") ?? ""; q.dispatchEvent(new Event("input"));
  }, 900);
  if (params.has("light") || params.has("settings")) setTimeout(() => {
    document.getElementById("set-btn")?.click();
    if (params.has("light")) document.querySelector<HTMLButtonElement>('#set-theme [data-theme="light"]')?.click();
    if (!params.has("settings")) (document.getElementById("set-dlg") as HTMLDialogElement).close();
    if (params.get("settings") === "pick") document.querySelector<HTMLButtonElement>("#set-done-sound .pick-btn")?.click();
  }, 800);
  if (params.has("ov")) setTimeout(() => {
    document.getElementById("ov-row")?.click();
    const f = params.get("ov");
    if (f) setTimeout(() => document.querySelector<HTMLButtonElement>(`#overview [data-f="${f}"]`)?.click(), 100);
  }, 900);
  if (params.has("wsgrid")) setTimeout(() => document.querySelector<HTMLButtonElement>("#tabs .wgrid")?.click(), 1400);
  // ?pdrag: hold the 2nd project over the 3rd (screenshot of the "Group" state).
  if (params.has("pdrag")) setTimeout(() => {
    const heads = [...document.querySelectorAll<HTMLElement>("#tabs .proj[data-proj]")];
    const [a, b] = [heads[1], heads[2]];
    if (!a || !b) return;
    const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
    a.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, buttons: 1, clientX: ra.left + 40, clientY: ra.top + 8 }));
    window.dispatchEvent(new PointerEvent("pointermove", { buttons: 1, clientX: ra.left + 60, clientY: ra.top + 30 }));
    window.dispatchEvent(new PointerEvent("pointermove", { buttons: 1, clientX: rb.left + 60, clientY: rb.top + rb.height / 2 }));
  }, 1300);
  // ?reorder: drag the 2nd project toward the top edge of the 1st (line above it).
  if (params.has("reorder")) setTimeout(() => {
    const heads = [...document.querySelectorAll<HTMLElement>("#tabs .proj[data-proj]")];
    const [a, b] = [heads[0], heads[1]];
    if (!a || !b) return;
    const ra = a.getBoundingClientRect(), rb = b.getBoundingClientRect();
    b.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true, button: 0, buttons: 1, clientX: rb.left + 40, clientY: rb.top + 8 }));
    window.dispatchEvent(new PointerEvent("pointermove", { buttons: 1, clientX: rb.left + 60, clientY: rb.top - 20 }));
    window.dispatchEvent(new PointerEvent("pointermove", { buttons: 1, clientX: ra.left + 60, clientY: ra.top + 2 }));
  }, 1300);
  if (params.has("dialog")) setTimeout(() => document.getElementById("new-btn")?.click(), 400);
}

// ?git / ?git=clean: sample data for the Git panel.
const NOW = Math.floor(Date.now() / 1000);
const GIT_CLEAN = {
  root: "/Users/you/Developer/toranj", branch: "main", upstream: "origin/main", ahead: 0, behind: 0, base: "origin/main", base_local: "main", has_remote: true, skip_reason: "", push_known: true, branches_truncated: false,
  status_skipped: false, unpushed: 0, changed: 0, files: [], last_fetch: NOW - 1200, stashes: [],
  commits: [
    { hash: "a8c2f60", time: NOW - 900, author: "you", subject: "Merge pull request #18 from you/hc/fix/agent-count", pushed: true },
    { hash: "4d1e2b9", time: NOW - 1500, author: "you", subject: "fix(resources): count agent sessions, not processes", pushed: true },
  ],
  worktrees: [{ path: "/Users/you/Developer/toranj", branch: "main", head: "a8c2f60", current: true, locked: false, prunable: false, changed: 0, unpushed: 0, merged: true }],
  branches: [{ name: "main", upstream: "origin/main", gone: false, ahead: 0, behind: 0, unpushed: 0, merged: true, time: NOW - 900, worktree: "/Users/you/Developer/toranj" }],
};
const GIT_MESSY = {
  root: "/Users/you/Developer/toranj", branch: "main", upstream: "origin/main", ahead: 2, behind: 6, base: "origin/main", base_local: "main", has_remote: true, skip_reason: "", push_known: true, branches_truncated: false,
  status_skipped: false, unpushed: 2, changed: 4, last_fetch: NOW - 3 * 86400,
  files: [
    { code: ".M", path: "src/main.ts" }, { code: "M.", path: "src-tauri/src/git.rs" },
    { code: "UU", path: "src/styles.css" }, { code: "??", path: "notes/todo-git-panel.md" },
  ],
  commits: [
    { hash: "e01f3c2", time: NOW - 300, author: "you", subject: "wip: git panel", pushed: false },
    { hash: "9b77a10", time: NOW - 2400, author: "you", subject: "feat: git details backend", pushed: false },
    { hash: "a8c2f60", time: NOW - 86400, author: "you", subject: "Merge pull request #18 from you/hc/fix/agent-count", pushed: true },
  ],
  worktrees: [
    { path: "/Users/you/Developer/toranj", branch: "main", head: "e01f3c2", current: true, locked: false, prunable: false, changed: 4, unpushed: 2, merged: false },
    { path: "/Users/you/Developer/toranj/.claude/worktrees/resource-details", branch: "hc/feat/git-panel", head: "77aa001", current: false, locked: false, prunable: false, changed: 2, unpushed: 3, merged: false },
    { path: "/Users/you/Developer/toranj/.claude/worktrees/old-fix", branch: "hc/fix/old", head: "1234567", current: false, locked: false, prunable: false, changed: 0, unpushed: 0, merged: true },
    { path: "/Users/you/tmp/gone-wt", branch: "hc/exp", head: "abcdef0", current: false, locked: false, prunable: true, changed: null, unpushed: null, merged: false },
  ],
  branches: [
    { name: "main", upstream: "origin/main", gone: false, ahead: 2, behind: 6, unpushed: 2, merged: true, time: NOW - 300, worktree: "/Users/you/Developer/toranj" },
    { name: "hc/feat/git-panel", upstream: "", gone: false, ahead: 0, behind: 0, unpushed: 3, merged: false, time: NOW - 600, worktree: "/Users/you/Developer/toranj/.claude/worktrees/resource-details" },
    { name: "hc/fix/old", upstream: "origin/hc/fix/old", gone: true, ahead: 0, behind: 0, unpushed: 0, merged: true, time: NOW - 5 * 86400, worktree: "/Users/you/Developer/toranj/.claude/worktrees/old-fix" },
    { name: "hc/spike", upstream: "", gone: false, ahead: 0, behind: 0, unpushed: 5, merged: false, time: NOW - 9 * 86400, worktree: "" },
    { name: "hc/fix/a", upstream: "origin/hc/fix/a", gone: true, ahead: 0, behind: 0, unpushed: 0, merged: true, time: NOW - 2 * 86400, worktree: "" },
    { name: "hc/fix/b", upstream: "origin/hc/fix/b", gone: false, ahead: 0, behind: 0, unpushed: 0, merged: true, time: NOW - 3 * 86400, worktree: "" },
  ],
  stashes: [{ name: "stash@{0}", time: NOW - 4 * 86400, subject: "On main: before rebase" }],
};
