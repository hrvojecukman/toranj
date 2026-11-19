import { invoke, Channel } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { Terminal } from "@xterm/xterm";
import { FitAddon } from "@xterm/addon-fit";
import { WebglAddon } from "@xterm/addon-webgl";
import { WebLinksAddon } from "@xterm/addon-web-links";
import { Unicode11Addon } from "@xterm/addon-unicode11";
import { ClipboardAddon } from "@xterm/addon-clipboard";
import { notifyTab, onNotificationClick, playSound, setBadge, SOUNDS, type Kind } from "./notify";

type State = "running" | "waiting" | "idle" | "done" | "error";
interface Tab { name: string; project: string; cwd: string; agent: string; state: State; detail: string; title: string; since: number; background: boolean }
interface External { name: string; cwd: string; state: State; detail: string; title: string; session_id: string; background: boolean; unlisted: boolean; pid: number }
interface Git { branch: string; changed: number; ahead: number; behind: number }
interface Snapshot { tabs: Tab[]; external: External[]; git: Record<string, Git> }
/** A project folder, or (`count` set) a folder holding several of them. */
interface Project { name: string; path: string; count?: number; folders?: string[] }

const $ = <T extends HTMLElement = HTMLElement>(id: string) => document.getElementById(id) as T;
const LABEL: Record<State, string> = { running: "running", waiting: "waiting for you", idle: "idle", done: "done", error: "error" };
/** Sidebar status: only what needs a look. Idle is the dot alone. */
const SHORT: Record<State, string> = { running: "", waiting: "waiting", idle: "", done: "done", error: "error" };

// ---------- terminals ----------
// Every visible tab gets its own xterm and its own `tmux attach` client.
// Hidden tabs have neither: they cost nothing but their tmux session.

type Layout = "single" | "grid" | "cols" | "rows";
/** Pane sizes per layout shape: row heights, and each row's pane widths. */
interface TrackSizes { r: number[]; c: number[][] }
interface ViewPrefs { layout: Layout; multi: Exclude<Layout, "single">; order: string[]; sizes?: Record<string, TrackSizes> }

const termOptions = {
  fontFamily: '"SF Mono", Menlo, monospace',
  fontSize: 13,
  lineHeight: 1.15,
  cursorBlink: true,
  allowProposedApi: true,
  macOptionIsMeta: true,
  // tmux owns the mouse; Option+drag still makes a native xterm selection.
  macOptionClickForcesSelection: true,
  // Inside tmux an Option+click would send arrow keys (history, menus).
  altClickMovesCursor: false,
  scrollback: 5000,
  // OSC 8 links (Claude prints them): ⌘-click opens them in the browser.
  linkHandler: { activate: (e: MouseEvent, uri: string) => openLink(e, uri), allowNonHttpProtocols: false },
};

/** ⌘-click on a link in a terminal opens it in the default browser. */
function openLink(e: MouseEvent, uri: string) {
  if (!e.metaKey) return;
  invoke("open_url", { url: uri }).catch((err) => console.error(err));
}

// ---------- settings ----------
// Stored in this app's localStorage: a per-machine preference.
type Theme = "dark" | "light" | "system";
interface Settings { fontSize: number; theme: Theme; sound: boolean; doneSound: string; waitSound: string; quickSecs: number; quickKey: string }
/** Quick open: how long a new dock card listens for its key (0 = off), and the keys offered. */
const QUICK_SECS = [0, 5, 10, 20];
const QUICK_KEYS: Record<string, string> = { Space: "Space", Return: "Enter", "→ (right arrow)": "ArrowRight", "Tab": "Tab" };
const FONT_MIN = 10, FONT_MAX = 22;
const DARK_TERM = { background: "#1d232b", foreground: "#d5dbe2", cursor: "#d5dbe2", selectionBackground: "#3b4b66" };
// xterm's default ANSI colors are made for dark backgrounds: white and bright
// yellow vanish on white, so the light theme brings its own palette.
const LIGHT_TERM = {
  background: "#ffffff", foreground: "#24292f", cursor: "#24292f", cursorAccent: "#ffffff", selectionBackground: "#b6d3ff",
  black: "#24292f", red: "#cf222e", green: "#116329", yellow: "#7d4e00", blue: "#0969da", magenta: "#8250df", cyan: "#1b7c83", white: "#6e7781",
  brightBlack: "#57606a", brightRed: "#a40e26", brightGreen: "#1a7f37", brightYellow: "#9a6700", brightBlue: "#218bff", brightMagenta: "#a475f9", brightCyan: "#3192aa", brightWhite: "#8c959f",
};

/** The sound for a notification, "" when sounds are off. Errors keep Basso. */
function soundFor(kind: Kind): string {
  if (!settings.sound) return "";
  return kind === "done" ? settings.doneSound : kind === "waiting" ? settings.waitSound : "Basso";
}

function loadSettings(): Settings {
  const d: Settings = { fontSize: 13, theme: "dark", sound: true, doneSound: "Glass", waitSound: "Ping", quickSecs: 10, quickKey: "Space" };
  try {
    const s = JSON.parse(localStorage.getItem("toranj.settings") ?? "{}");
    const fs = typeof s.fontSize === "number" ? s.fontSize : NaN;
    return {
      fontSize: Number.isFinite(fs) ? Math.min(FONT_MAX, Math.max(FONT_MIN, Math.round(fs))) : d.fontSize,
      theme: ["dark", "light", "system"].includes(s.theme) ? s.theme : d.theme,
      sound: typeof s.sound === "boolean" ? s.sound : d.sound,
      doneSound: SOUNDS.includes(s.doneSound) ? s.doneSound : d.doneSound,
      waitSound: SOUNDS.includes(s.waitSound) ? s.waitSound : d.waitSound,
      quickSecs: QUICK_SECS.includes(s.quickSecs) ? s.quickSecs : d.quickSecs,
      quickKey: Object.values(QUICK_KEYS).includes(s.quickKey) ? s.quickKey : d.quickKey,
    };
  } catch { return d; }
}
const settings = loadSettings();
const lightQuery = matchMedia("(prefers-color-scheme: light)");
const isLight = () => settings.theme === "light" || (settings.theme === "system" && lightQuery.matches);
const termTheme = () => (isLight() ? LIGHT_TERM : DARK_TERM);
document.documentElement.dataset.theme = isLight() ? "light" : "dark";

/** Control sequence for a macOS editing shortcut, or null for other keys. */
function macEditKey(e: KeyboardEvent): string | null {
  if (e.ctrlKey || e.shiftKey) return null;
  if (e.metaKey && !e.altKey) {
    switch (e.key) {
      case "ArrowLeft": return "\x01"; // start of line (Ctrl+A)
      case "ArrowRight": return "\x05"; // end of line (Ctrl+E)
      case "Backspace": return "\x15"; // delete to line start (Ctrl+U)
    }
  }
  if (e.altKey && !e.metaKey) {
    switch (e.key) {
      case "ArrowLeft": return "\x1bb"; // word left (Meta+B)
      case "ArrowRight": return "\x1bf"; // word right (Meta+F)
    }
  }
  return null;
}

class Pane {
  readonly el = document.createElement("div");
  private readonly host = document.createElement("div");
  readonly term = new Terminal({ ...termOptions, fontSize: settings.fontSize, theme: termTheme() });
  private readonly fit = new FitAddon();
  private readonly ro: ResizeObserver;
  attachId: number | null = null;
  private copiedTimer = 0;

  /** A short "Copied" in the pane's corner after a selection reaches the clipboard. */
  copied() {
    let tag = this.el.querySelector<HTMLElement>(".tp-copied");
    if (!tag) {
      tag = document.createElement("div");
      tag.className = "tp-copied";
      tag.textContent = "Copied";
      this.el.append(tag);
    }
    tag.classList.remove("go");
    void tag.offsetWidth; // restart the animation
    tag.classList.add("go");
    clearTimeout(this.copiedTimer);
    this.copiedTimer = window.setTimeout(() => tag!.classList.remove("go"), 1300);
  }
  private disposed = false;

  constructor(readonly name: string, parent: HTMLElement) {
    this.el.className = "tp";
    this.el.dataset.name = name;
    this.el.innerHTML = `<div class="tp-bar" title="Drag to rearrange"><i class="st"></i><span class="tp-n"></span><span class="tp-t"></span><button class="tp-x" title="Close this terminal" aria-label="Close this terminal">×</button></div>`;
    this.host.className = "tp-host";
    const foot = document.createElement("div");
    foot.className = "tp-foot";
    foot.innerHTML = `<i class="pi" aria-hidden="true"></i><b></b><span></span>`;
    this.el.append(this.host, foot);
    parent.append(this.el);

    const t = this.term;
    t.loadAddon(this.fit);
    t.loadAddon(new WebLinksAddon(openLink));
    // Programs may copy (tmux uses OSC 52 for mouse selections) but never
    // read the clipboard: an OSC 52 query would type it into the terminal.
    t.loadAddon(new ClipboardAddon(undefined, {
      readText: () => "",
      writeText: (_sel, text) => navigator.clipboard.writeText(text).then(() => this.copied()),
    }));
    const uni = new Unicode11Addon();
    t.loadAddon(uni);
    t.unicode.activeVersion = "11";
    t.open(this.host);
    try {
      const gl = new WebglAddon();
      gl.onContextLoss(() => gl.dispose()); // falls back to the DOM renderer
      t.loadAddon(gl);
    } catch { /* DOM renderer */ }
    t.attachCustomKeyEventHandler((e) => {
      // Shift+Enter and Cmd+Enter insert a new line instead of sending, like
      // the usual VS Code setup: they send ESC + CR (Meta+Enter), which
      // Claude Code and most line editors treat as "new line".
      // While an input method is composing, Enter commits the candidate.
      if (e.isComposing || e.keyCode === 229) return true;
      if (e.key === "Enter" && (e.shiftKey || e.metaKey) && !e.ctrlKey && !e.altKey) {
        if (e.type === "keydown" && this.attachId !== null) invoke("write", { id: this.attachId, data: "\x1b\r" });
        e.preventDefault();
        return false;
      }
      // macOS text-editing keys, mapped as Terminal and iTerm do: xterm
      // sends nothing for Cmd+arrow, but the shell (zsh, bash) and Claude
      // Code understand these control sequences.
      const seq = macEditKey(e);
      if (seq !== null) {
        if (e.type === "keydown" && this.attachId !== null) invoke("write", { id: this.attachId, data: seq });
        e.preventDefault();
        return false;
      }
      // Cmd+C copies an xterm selection (Option+drag); a mouse selection
      // made through tmux is already on the clipboard.
      if (e.metaKey && !e.shiftKey && !e.altKey && e.key.toLowerCase() === "c") {
        if (e.type === "keydown" && t.hasSelection()) navigator.clipboard.writeText(t.getSelection()).catch(() => {});
        return false;
      }
      // App shortcuts never reach the terminal.
      return !(e.metaKey && e.type === "keydown" && /^[tuwgk0-9=+,-]$/i.test(e.key));
    });
    t.onData((data) => { if (this.attachId !== null) invoke("write", { id: this.attachId, data }); });
    t.onResize(({ cols, rows }) => { if (this.attachId !== null) invoke("resize", { id: this.attachId, cols, rows }); });
    this.ro = new ResizeObserver(() => this.refit());
    this.ro.observe(this.host);
    // Typing or clicking in a pane makes it the current tab.
    this.el.addEventListener("focusin", () => focusTab(this.name));
    this.el.addEventListener("mousedown", () => focusTab(this.name));
    (this.el.firstChild as HTMLElement).addEventListener("pointerdown", (e) => startDrag(e, this.name));
    const x = this.el.querySelector(".tp-x") as HTMLButtonElement;
    x.addEventListener("pointerdown", (e) => e.stopPropagation()); // not a drag
    x.addEventListener("click", (e) => { e.stopPropagation(); askClose(this.name); });
    (this.el.firstChild as HTMLElement).addEventListener("contextmenu", (e) => tabMenu(e, this.name));
  }

  refit() { try { this.fit.fit(); } catch {} }

  async attach() {
    this.refit();
    const ch = new Channel<ArrayBuffer>();
    ch.onmessage = (buf) => { if (!this.disposed) this.term.write(new Uint8Array(buf)); };
    try {
      const id = await invoke<number>("attach", { name: this.name, cols: this.term.cols, rows: this.term.rows, onData: ch });
      if (this.disposed) { invoke("detach", { id }); return; }
      this.attachId = id;
      // The pane may have been resized while attaching.
      invoke("resize", { id, cols: this.term.cols, rows: this.term.rows });
    } catch (e) {
      if (!this.disposed) this.term.write(`\r\n\x1b[31mCould not connect to the terminal: ${e}\x1b[0m\r\n`);
    }
  }

  dispose() {
    this.disposed = true;
    if (this.attachId !== null) invoke("detach", { id: this.attachId });
    this.attachId = null;
    this.ro.disconnect();
    this.term.dispose();
    this.el.remove();
  }
}

const panes = new Map<string, Pane>();
let current: string | null = null;
/** Tabs being closed: hidden right away, before the next snapshot drops them. */
const closing = new Set<string>();

const groupKey = (t: Tab) => t.project || t.cwd;
/** What a grid shows together: the workspace of the tab's project, else the project. */
const viewKey = (t: Tab) => { const w = wsNow.get(groupKey(t)); return w ? `ws:${w.id}` : groupKey(t); };

function loadPrefs(): Record<string, ViewPrefs> {
  try { return JSON.parse(localStorage.getItem("toranj.views") ?? "{}"); } catch { return {}; }
}
const viewPrefs = loadPrefs();
function prefsFor(key: string): ViewPrefs {
  // A group (workspace) is there to be seen together: its default is the grid.
  return viewPrefs[key] ?? { layout: key.startsWith("ws:") ? "grid" : "single", multi: "grid", order: [] };
}
function savePrefs(key: string, p: ViewPrefs) {
  viewPrefs[key] = p;
  try { localStorage.setItem("toranj.views", JSON.stringify(viewPrefs)); } catch { /* per-viewer convenience only */ }
}

/** Tabs of the current tab's project, in the user's saved order. */
function groupTabs(cur: Tab): Tab[] {
  const key = viewKey(cur);
  const order = prefsFor(key).order;
  const rank = (n: string) => { const i = order.indexOf(n); return i < 0 ? order.length : i; };
  // In a group, projects keep their sidebar order (same as ⌘1–9).
  const prank = (t: Tab) => { const i = org.order.indexOf(`p:${groupKey(t)}`); return i < 0 ? org.order.length : i; };
  const pname = (t: Tab) => leafLabel(groupKey(t));
  return snap.tabs.filter((t) => viewKey(t) === key && !closing.has(t.name))
    .sort((a, b) => (prank(a) - prank(b)) || pname(a).localeCompare(pname(b), undefined, { sensitivity: "base" }) || (rank(a.name) - rank(b.name)));
}

/** Columns × rows for `n` panes. */
function gridShape(layout: Layout, n: number) {
  if (layout === "cols") return { cols: n, rows: 1 };
  if (layout === "rows") return { cols: 1, rows: n };
  const cols = Math.ceil(Math.sqrt(n));
  return { cols, rows: Math.ceil(n / cols) };
}

/** Shows the current tab alone, or all tabs of its project side by side. */
function layoutPanes() {
  const grid = $("grid");
  const cur = snap.tabs.find((t) => t.name === current);
  const layout = cur ? prefsFor(viewKey(cur)).layout : "single";
  const shown = !cur ? [] : layout === "single" ? [cur] : groupTabs(cur);
  const names = shown.map((t) => t.name);

  for (const [name, p] of panes) if (!names.includes(name)) { p.dispose(); panes.delete(name); }

  const { cols } = gridShape(layout, Math.max(1, names.length));
  // Rows of panes; each row sizes its own panes (no shared columns), so a
  // short last row just shares its width.
  const rowNames: string[][] = [];
  for (let i = 0; i < names.length; i += cols) rowNames.push(names.slice(i, i + cols));
  const vkey = cur ? viewKey(cur) : "";
  const shape = `${layout}:${rowNames.map((r) => r.length).join("-")}`;
  const saved = cur ? prefsFor(vkey).sizes?.[shape] : undefined;
  const ok = (v: unknown, n: number): v is number[] => Array.isArray(v) && v.length === n && v.every((x) => typeof x === "number" && x > 0);
  const sizes: TrackSizes = {
    r: ok(saved?.r, rowNames.length) ? [...saved!.r] : rowNames.map(() => 1),
    c: rowNames.map((row, i) => (Array.isArray(saved?.c) && ok(saved!.c[i], row.length) ? [...saved!.c[i]] : row.map(() => 1))),
  };
  grid.classList.toggle("multi", names.length > 1);

  const fresh: Pane[] = [];
  for (const name of names) {
    let p = panes.get(name);
    if (!p) { p = new Pane(name, grid); panes.set(name, p); fresh.push(p); paneDone.delete(name); }
  }
  buildRows(grid, rowNames, sizes, vkey, shape);
  for (const p of fresh) {
    doneMarks.delete(p.name); // a visible tab counts as seen
    p.attach();
  }
  // Re-measure every terminal once the new layout has settled.
  requestAnimationFrame(() => { for (const p of panes.values()) p.refit(); });
  paintPanes();
}

/** Lays the panes out in rows, with dividers between panes and between rows:
 * drag a divider to resize, double-click it to even out. */
function buildRows(grid: HTMLElement, rowNames: string[][], sizes: TrackSizes, vkey: string, shape: string) {
  document.body.classList.remove("resizing-c", "resizing-r"); // a drag cut short by a re-layout
  // Keep the panes, rebuild the rows and dividers around them.
  for (const el of [...grid.children]) if (el.classList.contains("grow") || el.classList.contains("gut")) {
    for (const tp of [...el.querySelectorAll<HTMLElement>(".tp")]) grid.append(tp);
    el.remove();
  }
  const save = () => {
    const p = prefsFor(vkey);
    savePrefs(vkey, { ...p, sizes: { ...(p.sizes ?? {}), [shape]: { r: [...sizes.r], c: sizes.c.map((c) => [...c]) } } });
  };
  const rowEls: HTMLElement[] = [];
  rowNames.forEach((row, r) => {
    const rowEl = document.createElement("div");
    rowEl.className = "grow";
    rowEl.style.flex = `${sizes.r[r]} 1 0`;
    row.forEach((name, c) => {
      const p = panes.get(name)!;
      p.el.style.flex = `${sizes.c[r][c]} 1 0`;
      rowEl.append(p.el);
      if (c < row.length - 1) rowEl.append(divider("v", sizes.c[r], c, () => row.map((n) => panes.get(n)!.el), rowEl));
    });
    rowEls.push(rowEl);
    grid.append(rowEl);
    if (r < rowNames.length - 1) grid.append(divider("h", sizes.r, r, () => rowEls, grid));
  });

  function divider(dir: "v" | "h", t: number[], i: number, items: () => HTMLElement[], box: HTMLElement): HTMLElement {
    const g = document.createElement("div");
    g.className = `gut ${dir}`;
    g.title = "Drag to resize · double-click to even out";
    const apply = () => items().forEach((el, k) => { if (t[k] !== undefined) el.style.flex = `${t[k]} 1 0`; });
    g.onpointerdown = (e) => {
      if (e.button !== 0) return;
      e.preventDefault();
      e.stopPropagation();
      try { g.setPointerCapture(e.pointerId); } catch { /* synthetic events */ }
      const rect = box.getBoundingClientRect();
      const span = dir === "v" ? rect.width : rect.height;
      const total = t.reduce((a, b) => a + b, 0);
      const before = t.slice(0, i).reduce((a, b) => a + b, 0);
      const pair = t[i] + t[i + 1];
      const min = Math.min(pair / 2, total * (120 / span)); // panes keep ~120 px
      document.body.classList.add(dir === "v" ? "resizing-c" : "resizing-r");
      let raf = 0;
      let snapped = false;
      const snapWithin = total * (14 / span); // ~14 px either side of an even split
      g.onpointermove = (ev) => {
        const pos = ((dir === "v" ? ev.clientX - rect.left : ev.clientY - rect.top) / span) * total;
        let left = Math.max(min, Math.min(pair - min, pos - before));
        // An even split between the two holds for a moment, with a tap on
        // the trackpad, so 50 % is easy to hit.
        const snap = Math.abs(left - pair / 2) < snapWithin;
        if (snap) left = pair / 2;
        if (snap && !snapped) invoke("haptic").catch(() => {});
        snapped = snap;
        g.classList.toggle("snap", snap);
        t[i] = left;
        t[i + 1] = pair - left;
        if (!raf) raf = requestAnimationFrame(() => { raf = 0; apply(); });
      };
      const done = () => {
        g.classList.remove("snap");
        g.onpointermove = null;
        g.onpointerup = null;
        g.onpointercancel = null;
        document.body.classList.remove("resizing-c", "resizing-r");
        apply();
        save();
        requestAnimationFrame(() => { for (const p of panes.values()) p.refit(); });
      };
      g.onpointerup = done;
      g.onpointercancel = done;
    };
    g.ondblclick = () => {
      t.fill(1);
      apply();
      save();
      requestAnimationFrame(() => { for (const p of panes.values()) p.refit(); });
    };
    return g;
  }
}

/** Panes on screen that finished since you last clicked or typed in them. */
const paneDone = new Map<string, State>();

function paintPanes() {
  const multi = panes.size > 1;
  for (const p of panes.values()) {
    const t = snap.tabs.find((x) => x.name === p.name);
    p.el.classList.toggle("on", p.name === current);
    if (!t) continue;
    const st = doneMarks.get(t.name) ?? t.state;
    // Side by side, a pane's border says what it needs: waiting, done, error.
    const flag = !multi ? "" : t.state === "waiting" ? "waiting" : paneDone.get(t.name) ?? "";
    for (const k of ["waiting", "done", "error"]) p.el.classList.toggle(`is-${k}`, flag === k);
    const dot = p.el.querySelector(".st") as HTMLElement;
    if (dot.className !== `st ${st}`) dot.className = `st ${st}`;
    const num = sidebarNumber(t.name);
    setText(p.el, ".tp-n", (t.title || t.agent) + (num ? `  ⌘${num}` : ""));
    setText(p.el, ".tp-t", st === "running" ? ago(t.since) : LABEL[st]);
    const proj = groupKey(t);
    paintIcon(p.el.querySelector(".tp-foot .pi") as HTMLElement, proj);
    setText(p.el, ".tp-foot b", fullLabel(proj));
    setText(p.el, ".tp-foot span", t.title ? `· ${t.title}` : "");
    (p.el.querySelector(".tp-foot b") as HTMLElement).style.color = projectColor(proj);
  }
}

function sidebarNumber(name: string) {
  const i = [...document.querySelectorAll<HTMLElement>("#tabs .sess:not(.ext)")].findIndex((b) => b.dataset.key === `t:${name}`);
  return i >= 0 && i < 9 ? i + 1 : 0;
}

/** Makes `name` the current tab without changing what is shown. */
function focusTab(name: string) {
  paneDone.delete(name);
  if (current === name) { paintPanes(); return; }
  recordMove({ tab: name, ov: false });
  rememberView(name);
  current = name;
  doneMarks.delete(name);
  const tab = snap.tabs.find((t) => t.name === name);
  if (tab?.project) invoke("refresh_git", { path: tab.project });
  render();
}

let shownProject = "";
/** Shows the project's icon and name over the terminals for a moment. */
function flashProject(proj: string) {
  const f = $("pflash");
  paintIcon(f.querySelector(".pi") as HTMLElement, proj);
  (f.querySelector("b") as HTMLElement).textContent = projectLabel(proj);
  f.hidden = false;
  f.classList.remove("go");
  void f.offsetWidth; // restart the animation
  f.classList.add("go");
}

/** What was on screen, so a restart comes back to it ("" = the overview). */
function rememberView(tab: string) {
  try { localStorage.setItem("toranj.view", tab); } catch { /* convenience only */ }
}

// ---------- back / forward ----------
// Every jump (a dock card, ⌘U, the sidebar, a click on another pane, the
// overview) is remembered, so ⌘[ goes back to where you came from and ⌘]
// forward again, like a browser. The mouse's back/forward buttons too.
interface Place { tab: string | null; ov: boolean }
const backStack: Place[] = [];
const fwdStack: Place[] = [];
let navigating = false;
const here = (): Place => ({ tab: current, ov: ovShown() });
const samePlace = (a: Place, b: Place) => a.tab === b.tab && a.ov === b.ov;
const placeOk = (p: Place) => p.ov ? snap.tabs.length > 0 : !!p.tab && snap.tabs.some((t) => t.name === p.tab);

/** Called just before moving to `to`. */
function recordMove(to: Place) {
  if (navigating) return;
  const from = here();
  if (samePlace(from, to) || (!from.tab && !from.ov)) return;
  if (!backStack.length || !samePlace(backStack[backStack.length - 1], from)) backStack.push(from);
  if (backStack.length > 50) backStack.shift();
  fwdStack.length = 0;
}

function goPlace(p: Place) {
  navigating = true;
  try {
    if (p.ov) toggleOverview(true);
    else if (p.tab) open(p.tab);
  } finally { navigating = false; }
}

function goBack() {
  while (backStack.length) {
    const p = backStack.pop()!;
    if (placeOk(p) && !samePlace(p, here())) { fwdStack.push(here()); goPlace(p); return; }
  }
}

function goForward() {
  while (fwdStack.length) {
    const p = fwdStack.pop()!;
    if (placeOk(p) && !samePlace(p, here())) { backStack.push(here()); goPlace(p); return; }
  }
}

window.addEventListener("mouseup", (e) => {
  if (e.button === 3) { e.preventDefault(); goBack(); }
  else if (e.button === 4) { e.preventDefault(); goForward(); }
});

function open(name: string) {
  recordMove({ tab: name, ov: false });
  overview = false;
  rememberView(name);
  const tab = snap.tabs.find((t) => t.name === name);
  if (tab) {
    const p = groupKey(tab);
    if (shownProject && p !== shownProject) flashProject(p);
    shownProject = p;
  }
  if (tab?.project) invoke("refresh_git", { path: tab.project }); // e.g. a branch switch in the terminal
  current = name;
  doneMarks.delete(name); // looking at a finished tab marks it as seen
  layoutPanes();
  render();
  panes.get(name)?.term.focus();
}

function setLayout(layout: Layout) {
  const cur = snap.tabs.find((t) => t.name === current);
  if (!cur) return;
  const key = viewKey(cur);
  const p = prefsFor(key);
  savePrefs(key, { ...p, layout, multi: layout === "single" ? p.multi : layout });
  open(cur.name);
}

function toggleLayout() {
  const cur = snap.tabs.find((t) => t.name === current);
  if (!cur) return;
  const p = prefsFor(viewKey(cur));
  setLayout(p.layout === "single" ? p.multi : "single");
}

// Drag a pane by its title bar onto another pane to swap them.
function startDrag(e: PointerEvent, name: string) {
  if (e.button !== 0) return;
  focusTab(name); // preventDefault below also stops the mousedown that would do this
  panes.get(name)?.term.focus();
  if (panes.size < 2) return;
  e.preventDefault();
  const from = panes.get(name)!.el;
  let target: string | null = null;
  const over = (ev: PointerEvent) => {
    const el = (document.elementFromPoint(ev.clientX, ev.clientY) as HTMLElement | null)?.closest<HTMLElement>(".tp");
    target = el && el !== from ? el.dataset.name ?? null : null;
    for (const p of panes.values()) p.el.classList.toggle("drop", p.name === target);
  };
  from.classList.add("dragging");
  document.body.classList.add("dragging");
  const up = () => {
    window.removeEventListener("pointermove", over);
    window.removeEventListener("pointerup", up);
    from.classList.remove("dragging");
    document.body.classList.remove("dragging");
    for (const p of panes.values()) p.el.classList.remove("drop");
    const cur = snap.tabs.find((t) => t.name === name);
    if (!target || !cur) return;
    const key = viewKey(cur);
    const other = snap.tabs.find((t) => t.name === target);
    const ws = wsNow.get(groupKey(cur));
    if (other && ws && groupKey(other) !== groupKey(cur)) {
      // Panes of two projects in a group: the projects trade places (the
      // sidebar order, which the grid follows).
      const list = wsProjects(ws.id).map((p) => p.key);
      const pa = list.indexOf(`p:${groupKey(cur)}`), pb = list.indexOf(`p:${groupKey(other)}`);
      if (pa < 0 || pb < 0) return;
      [list[pa], list[pb]] = [list[pb], list[pa]];
      org.order = [...list, ...org.order.filter((k) => !list.includes(k))];
      saveOrg();
      layoutPanes();
      panes.get(name)?.term.focus();
      return;
    }
    const order = groupTabs(cur).map((t) => t.name);
    const a = order.indexOf(name), b = order.indexOf(target);
    if (a < 0 || b < 0) return;
    [order[a], order[b]] = [order[b], order[a]];
    savePrefs(key, { ...prefsFor(key), order });
    layoutPanes();
    panes.get(name)?.term.focus();
  };
  window.addEventListener("pointermove", over);
  window.addEventListener("pointerup", up);
}

// ---------- state + sidebar ----------
let snap: Snapshot = { tabs: [], external: [], git: {} };
const prevState = new Map<string, State>();

function ago(unix: number) {
  const s = Math.max(0, Date.now() / 1000 - unix);
  if (s < 60) return `${Math.floor(s)} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h`;
  return `${Math.floor(s / 86400)} d`;
}

const GENERIC_DIRS = new Set(["my_apps", "projects", "developer", "documents", "code", "src", "dev", "work", "repos"]);
// ---------- workspaces and names ----------
// Projects of one product (e.g. a backend and its app) group under a
// workspace: by hand, from a VS Code *.code-workspace file, or by sharing a
// parent folder that isn't a generic one. Names and groups are per machine.
interface Org {
  names: Record<string, string>;   // project path -> name shown
  ws: Record<string, string>;      // project path -> workspace id, "-" = none
  wsNames: Record<string, string>; // workspace id -> name shown
  collapsed: string[];             // workspace ids
  order: string[];                 // sidebar order: "w:<id>" and "p:<path>" keys
}
function loadOrg(): Org {
  const o: Org = { names: {}, ws: {}, wsNames: {}, collapsed: [], order: [] };
  try {
    const s = JSON.parse(localStorage.getItem("toranj.org") ?? "{}");
    const strings = (x: unknown) => Object.fromEntries(Object.entries(x && typeof x === "object" ? x : {}).filter(([, v]) => typeof v === "string")) as Record<string, string>;
    o.names = strings(s.names);
    o.ws = strings(s.ws);
    o.wsNames = strings(s.wsNames);
    o.collapsed = Array.isArray(s.collapsed) ? s.collapsed.filter((x: unknown) => typeof x === "string") : [];
    o.order = Array.isArray(s.order) ? s.order.filter((x: unknown) => typeof x === "string") : [];
  } catch { /* defaults */ }
  return o;
}
const org = loadOrg();
function saveOrg() {
  try { localStorage.setItem("toranj.org", JSON.stringify(org)); } catch { /* applied, not kept */ }
  ovSig = "";
  render();
}

interface WsFile { file: string; name: string; folders: string[] }
let wsFiles: WsFile[] = [];
let wsAsked = "";
function loadWsFiles(keys: string[]) {
  const sig = [...keys].sort().join("\n");
  if (sig === wsAsked) return;
  wsAsked = sig;
  invoke<WsFile[]>("workspace_files", { paths: keys })
    .then((f) => { if (JSON.stringify(f) !== JSON.stringify(wsFiles)) { wsFiles = f; ovSig = ""; render(); } })
    .catch(() => {});
}

interface Ws { id: string; label: string }
/** The workspace of each project key. Automatic ones need two projects. */
function workspacesFor(keys: string[]): Map<string, Ws> {
  const raw = new Map<string, string>();
  const auto = new Map<string, string>();
  for (const k of keys) {
    const set = org.ws[k];
    if (set === "-") continue;
    if (set) { raw.set(k, set); continue; }
    // A terminal opened on the folder of a group's projects joins that group.
    const inCodeWs = wsFiles.some((w) => w.folders.some((d) => k === d || k.startsWith(d + "/")));
    if (!inCodeWs && k.split("/").filter(Boolean).length >= 3 && keys.some((o) => o !== k && o.replace(/\/[^/]+\/?$/, "") === k) && !GENERIC_DIRS.has((k.split("/").pop() ?? "").toLowerCase())) {
      raw.set(k, `dir:${k}`);
      auto.set(`dir:${k}`, k.split("/").pop() ?? k);
      continue;
    }
    const f = wsFiles.find((w) => w.folders.some((d) => k === d || k.startsWith(d + "/")));
    if (f) { raw.set(k, `code:${f.file}`); auto.set(`code:${f.file}`, f.name); continue; }
    const parts = k.split("/").filter(Boolean);
    const parent = parts[parts.length - 2] ?? "";
    // Not ~ itself or a generic folder like My_Apps.
    if (parts.length > 3 && !GENERIC_DIRS.has(parent.toLowerCase())) {
      const id = `dir:/${parts.slice(0, -1).join("/")}`;
      raw.set(k, id);
      auto.set(id, parent);
    }
  }
  const count = new Map<string, number>();
  for (const id of raw.values()) count.set(id, (count.get(id) ?? 0) + 1);
  const out = new Map<string, Ws>();
  for (const [k, id] of raw) {
    if (!id.startsWith("m:") && (count.get(id) ?? 0) < 2) continue;
    const fileName = id.startsWith("code:") ? wsFiles.find((w) => `code:${w.file}` === id)?.name : undefined;
    out.set(k, { id, label: org.wsNames[id] || auto.get(id) || fileName || id.replace(/^\w+:/, "").split("/").filter(Boolean).pop() || id });
  }
  return out;
}

/** A project's name inside its workspace: the folder alone, unless renamed. */
function leafLabel(p: string) {
  if (org.names[p]) return org.names[p];
  // The group's own folder: a terminal with all its projects.
  if (wsNow.get(p)?.id === `dir:${p}`) return "All projects";
  return p.split("/").filter(Boolean).pop() || p;
}

/** Asks for a line of text; resolves null on cancel. */
function promptText(title: string, value: string, note = ""): Promise<string | null> {
  const dlg = $<HTMLDialogElement>("ask-dlg");
  if (dlg.open || document.querySelector("dialog[open]")) return Promise.resolve(null);
  $("ask-title").textContent = title;
  $("ask-note").textContent = note;
  $("ask-note").hidden = !note;
  const input = $<HTMLInputElement>("ask-input");
  input.value = value;
  dlg.returnValue = "";
  dlg.showModal();
  input.select();
  return new Promise((resolve) => {
    dlg.addEventListener("close", () => {
      resolve(dlg.returnValue === "yes" ? input.value.trim() : null);
      panes.get(current ?? "")?.term.focus();
    }, { once: true });
  });
}

// Enter saves (the form's first submit button is Save); Cancel closes.
$("ask-cancel").onclick = () => $<HTMLDialogElement>("ask-dlg").close("no");

function showMenu(e: MouseEvent, items: [string, () => void, string?][]) {
  e.preventDefault();
  e.stopPropagation();
  const menu = $("ctx");
  menu.replaceChildren(...items.map(([label, run, cls]) => {
    const b = document.createElement("button");
    b.type = "button";
    b.role = "menuitem";
    b.textContent = label;
    if (cls) b.className = cls;
    b.onclick = () => { hideMenu(); run(); };
    return b;
  }));
  menu.hidden = false;
  const r = menu.getBoundingClientRect();
  menu.style.left = `${Math.min(e.clientX, innerWidth - r.width - 4)}px`;
  menu.style.top = `${Math.min(e.clientY, innerHeight - r.height - 4)}px`;
  (menu.firstChild as HTMLElement).focus();
}

let wsNow = new Map<string, Ws>(); // as last drawn in the sidebar

function projectMenu(e: MouseEvent, proj: string) {
  const mine = wsNow.get(proj);
  const others = new Map<string, string>();
  for (const w of wsNow.values()) if (w.id !== mine?.id) others.set(w.id, w.label);
  for (const id of Object.values(org.ws)) if (id.startsWith("m:") && id !== mine?.id && !others.has(id)) others.set(id, org.wsNames[id] || id.slice(2));
  const items: [string, () => void, string?][] = [
    ["New terminal here", () => openNew({ name: projectLabel(proj), path: proj })],
    ["Rename project…", async () => {
      const v = await promptText("Rename project", org.names[proj] || proj.split("/").filter(Boolean).pop() || proj, "Only the name shown in Toranj. Leave it empty for the folder's name.");
      if (v === null) return;
      if (v) org.names[proj] = v; else delete org.names[proj];
      saveOrg();
    }],
  ];
  for (const [id, label] of others) items.push([`Move to ${label}`, () => { org.ws[proj] = id; saveOrg(); }]);
  items.push(["Move to a new workspace…", async () => {
    const v = await promptText("New workspace", "", "Projects in a workspace sit together in the sidebar.");
    if (!v) return;
    const id = `m:${v}`;
    org.ws[proj] = id;
    org.wsNames[id] = v;
    saveOrg();
  }]);
  if (mine) items.push([`Take out of ${mine.label}`, () => { org.ws[proj] = "-"; saveOrg(); }]);
  if (org.ws[proj] !== undefined) items.push(["Group automatically", () => { delete org.ws[proj]; saveOrg(); }]);
  const mineTabs = snap.tabs.filter((t) => groupKey(t) === proj && !closing.has(t.name));
  if (mineTabs.length === 1) items.push(["Close terminal…", () => askClose(mineTabs[0].name), "danger"]);
  else if (mineTabs.length > 1) items.push([`Close all ${mineTabs.length} terminals…`, () => askCloseAll(mineTabs.map((t) => t.name), projectLabel(proj)), "danger"]);
  showMenu(e, items);
}

function workspaceMenu(e: MouseEvent, ws: Ws, projs: string[]) {
  const shut = org.collapsed.includes(ws.id);
  showMenu(e, [
    [shut ? "Expand" : "Collapse", () => toggleWs(ws.id)],
    ["Rename workspace…", () => renameWorkspace(ws)],
    ["Ungroup", () => { for (const p of projs) org.ws[p] = "-"; saveOrg(); }],
    ["Close all terminals in group…", () => askCloseAll(snap.tabs.filter((t) => projs.includes(groupKey(t))).map((t) => t.name), ws.label), "danger"],
  ]);
}

/** "Workspace › project" when the project is in one. */
function fullLabel(proj: string) {
  const w = wsNow.get(proj);
  return w ? `${w.label} › ${leafLabel(proj)}` : projectLabel(proj);
}

let labelsSent = "";
/** Names for Claude's status line (toranj-hook reads them). */
function sendLabels() {
  const projects: Record<string, { label: string; color: string }> = {};
  const tabs: Record<string, { label: string; color: string }> = {};
  for (const t of snap.tabs) {
    const p = groupKey(t);
    const l = { label: fullLabel(p), color: projectColor(p) };
    projects[p] = l;
    tabs[t.name] = l;
  }
  const body = JSON.stringify({ tabs, projects });
  if (body === labelsSent) return;
  labelsSent = body;
  invoke("set_status_labels", { labels: { tabs, projects } }).catch(() => { labelsSent = ""; });
}

/** Opens a workspace's terminals side by side (its grid layout). */
function showWorkspaceGrid(tabs: Tab[]) {
  const first = tabs.find((t) => t.name === current) ?? tabs[0];
  if (!first) return;
  const key = viewKey(first);
  const p = prefsFor(key);
  if (p.layout === "single") savePrefs(key, { ...p, layout: p.multi });
  open(first.name);
}

async function renameWorkspace(ws: Ws) {
  const v = await promptText("Rename workspace", ws.label, "Leave it empty for the automatic name.");
  if (v === null) return;
  if (v) org.wsNames[ws.id] = v; else delete org.wsNames[ws.id];
  saveOrg();
}

// ---------- drag a project to group it ----------
// Hold a project over another one for a moment: it swells and says "Group";
// let go and both form a workspace (like making a folder on macOS). Over a
// workspace's header it joins that one; dropped on empty space it leaves its
// workspace.
interface SidebarDrag {
  src: string;            // "p:<project>", "w:<workspace id>" or "t:<tab>"
  over: string;           // header key to group with ("p:…"/"w:…"), when grouping
  ready: boolean;         // held long enough over `over`
  out: boolean;           // over empty space: leave the workspace
  line: { key: string; side: "before" | "after" } | null; // where a reorder lands
  move: { level: "unit" | "ws" | "tab"; target: string; side: "before" | "after" } | null;
}
let pdrag: SidebarDrag | null = null;
const HOLD_MS = 450;
let justDragged = 0;

/** Drag state classes for a sidebar row, kept across re-renders. */
function dragClass(key: string): string {
  if (!pdrag) return "";
  if (key === pdrag.src) return " drag-src";
  if (pdrag.line?.key === key) return pdrag.line.side === "before" ? " drop-before" : " drop-after";
  if (key === pdrag.over) return pdrag.ready ? " merge-ready" : " merge-hover";
  return "";
}

/** Top-level sidebar units (workspaces and lone projects) in order, each with its rows. */
function sidebarUnits(): { key: string; rows: HTMLElement[] }[] {
  const out: { key: string; rows: HTMLElement[] }[] = [];
  for (const el of [...$("tabs").children] as HTMLElement[]) {
    const k = el.dataset.key ?? "";
    const top = (k.startsWith("w:") || k.startsWith("p:")) && !el.classList.contains("in-ws");
    if (top) out.push({ key: k, rows: [el] });
    else out[out.length - 1]?.rows.push(el);
  }
  return out;
}

/** In-workspace projects of workspace `id`, each with its rows. */
function wsProjects(id: string): { key: string; rows: HTMLElement[] }[] {
  const unit = sidebarUnits().find((u) => u.key === `w:${id}`);
  const out: { key: string; rows: HTMLElement[] }[] = [];
  for (const el of unit?.rows.slice(1) ?? []) {
    const k = el.dataset.key ?? "";
    if (k.startsWith("p:")) out.push({ key: k, rows: [el] });
    else out[out.length - 1]?.rows.push(el);
  }
  return out;
}

/** Puts `src` before or after `target` in `list`. */
function placed(list: string[], src: string, target: string, side: "before" | "after"): string[] {
  const rest = list.filter((k) => k !== src);
  const i = rest.indexOf(target);
  if (i < 0) return list;
  rest.splice(side === "before" ? i : i + 1, 0, src);
  return rest;
}

// Drag in the sidebar: the top or bottom of a row reorders (a line shows
// where it lands); holding a project over the middle of another one groups
// them, like making a folder on macOS; dropped on empty space a project
// leaves its workspace. Terminals reorder within their project.
function startSidebarDrag(e: PointerEvent, src: string) {
  // Ctrl+click is a right-click on macOS (no pointerup follows).
  if (e.button !== 0 || e.ctrlKey || pdrag || (e.target as HTMLElement).closest("button:not(.sess)")) return;
  const kind = src.slice(0, 1);
  const id = src.slice(2);
  const x0 = e.clientX, y0 = e.clientY;
  let ghost: HTMLElement | null = null;
  let timer = 0;
  const paint = () => {
    for (const el of $("tabs").querySelectorAll<HTMLElement>(".proj, .sess")) {
      el.classList.remove("drag-src", "merge-hover", "merge-ready", "drop-before", "drop-after");
      const c = dragClass(el.dataset.key ?? "").trim();
      if (c) el.classList.add(c);
    }
    if (ghost && pdrag) {
      const tag = ghost.querySelector("em") as HTMLElement;
      const ws = pdrag.over.startsWith("w:") ? [...wsNow.values()].find((w) => `w:${w.id}` === pdrag!.over) : wsNow.get(pdrag.over.slice(2));
      tag.textContent = pdrag.ready ? (ws ? `Move to ${ws.label}` : "Make a group")
        : pdrag.out ? `Take out of ${wsNow.get(id)?.label ?? ""}` : "";
    }
  };
  const label = () => {
    if (kind === "p") return leafLabel(id);
    if (kind === "w") return [...wsNow.values()].find((w) => w.id === id)?.label ?? "Group";
    const t = snap.tabs.find((x) => x.name === id);
    return t ? t.title || t.agent : id;
  };
  const move = (ev: PointerEvent) => {
    if (ev.buttons === 0) { finish(false); return; } // the button came up unseen
    if (!pdrag) {
      if (Math.hypot(ev.clientX - x0, ev.clientY - y0) < 6) return;
      pdrag = { src, over: "", ready: false, out: false, line: null, move: null };
      ghost = document.createElement("div");
      ghost.className = "pdrag-ghost";
      ghost.innerHTML = `<i class="pi" aria-hidden="true"></i><b></b><em></em>`;
      const iconOf = kind === "p" ? id : kind === "t" ? groupKey(snap.tabs.find((x) => x.name === id) ?? ({ project: "", cwd: id } as Tab)) : "";
      if (iconOf) paintIcon(ghost.querySelector(".pi") as HTMLElement, iconOf);
      else (ghost.querySelector(".pi") as HTMLElement).remove();
      (ghost.querySelector("b") as HTMLElement).textContent = label();
      document.body.append(ghost);
      document.body.classList.add("dragging");
      hideMenu();
    }
    ghost!.style.transform = `translate(${ev.clientX + 16}px, ${ev.clientY + 14}px)`;
    const hit = document.elementFromPoint(ev.clientX, ev.clientY) as HTMLElement | null;
    const row = hit?.closest<HTMLElement>("#tabs > *") ?? null;
    const rowKey = row?.dataset.key ?? "";
    const r = row?.getBoundingClientRect();
    const frac = r ? (ev.clientY - r.top) / r.height : 0.5;
    let over = "";
    let line: SidebarDrag["line"] = null;
    let mv: SidebarDrag["move"] = null;

    if (kind === "t") {
      // Terminals: among the rows of the same project.
      const t = snap.tabs.find((x) => x.name === id);
      const tt = rowKey.startsWith("t:") ? snap.tabs.find((x) => x.name === rowKey.slice(2)) : undefined;
      if (t && tt && tt.name !== t.name && groupKey(tt) === groupKey(t)) {
        const side = frac < 0.5 ? "before" : "after";
        line = { key: rowKey, side };
        mv = { level: "tab", target: tt.name, side };
      }
    } else if (row) {
      const isHeader = rowKey.startsWith("p:") || rowKey.startsWith("w:");
      const mid = isHeader && frac > 0.28 && frac < 0.72;
      const myWs = kind === "p" ? wsNow.get(id) : undefined;
      // Grouping: a project held over the middle of another project or a workspace.
      if (kind === "p" && mid && rowKey !== src && !(rowKey.startsWith("w:") && myWs && rowKey === `w:${myWs.id}`)) {
        over = rowKey;
      } else if (kind === "p" && myWs && row.classList.contains("in-ws")) {
        // Reorder among the projects of its own workspace.
        const sibs = wsProjects(myWs.id);
        const sib = sibs.find((p) => p.rows.includes(row));
        if (sib && sib.key !== src) {
          const side = sib.rows[0] === row && frac < 0.5 ? "before" : "after";
          line = { key: side === "before" ? sib.key : sib.rows[sib.rows.length - 1].dataset.key ?? "", side };
          mv = { level: "ws", target: sib.key, side };
        }
      } else if (!myWs) {
        // Reorder workspaces and lone projects.
        const unit = sidebarUnits().find((u) => u.rows.includes(row));
        if (unit && unit.key !== src) {
          const side = unit.rows[0] === row && frac < 0.5 ? "before" : "after";
          line = { key: side === "before" ? unit.key : unit.rows[unit.rows.length - 1].dataset.key ?? "", side };
          mv = { level: "unit", target: unit.key, side };
        }
      }
    }
    // Out of its workspace: only over empty sidebar space, not over rows.
    const empty = !!hit && (hit === $("tabs") || hit === $("tabs").parentElement || hit.id === "external");
    pdrag.out = kind === "p" && !row && !!wsNow.get(id) && empty;
    pdrag.line = line;
    pdrag.move = mv;
    if (over !== pdrag.over) {
      clearTimeout(timer);
      pdrag.over = over;
      pdrag.ready = false;
      if (over) timer = window.setTimeout(() => { if (pdrag && pdrag.over === over) { pdrag.ready = true; paint(); } }, over.startsWith("w:") ? 150 : HOLD_MS);
    }
    paint();
  };
  const finish = async (drop: boolean) => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    window.removeEventListener("pointercancel", cancel);
    window.removeEventListener("keydown", esc, true);
    window.removeEventListener("blur", cancel);
    clearTimeout(timer);
    const d = pdrag;
    pdrag = null;
    ghost?.remove();
    document.body.classList.remove("dragging");
    paint();
    if (!d) return;
    justDragged = Date.now(); // the click that ends a drag opens nothing
    if (!drop) return;
    if (d.ready && d.over.startsWith("w:")) {
      org.ws[id] = d.over.slice(2);
      saveOrg();
    } else if (d.ready && d.over.startsWith("p:")) {
      const other = d.over.slice(2);
      const theirs = wsNow.get(other);
      if (theirs) { org.ws[id] = theirs.id; saveOrg(); return; }
      const suggestion = commonName(leafLabel(id), leafLabel(other));
      const wid = `m:${suggestion}-${Date.now().toString(36)}`;
      org.ws[id] = wid;
      org.ws[other] = wid;
      org.wsNames[wid] = suggestion;
      saveOrg();
      // Name it right away, like a new folder; Enter keeps the suggestion.
      const v = await promptText("Name this group", suggestion, "Its projects sit together in the sidebar; click the group to see all its terminals.");
      if (v) { org.wsNames[wid] = v; saveOrg(); }
    } else if (d.move) {
      const { level, target, side } = d.move;
      if (level === "tab") {
        const t = snap.tabs.find((x) => x.name === id);
        if (!t) return;
        const key = viewKey(t);
        savePrefs(key, { ...prefsFor(key), order: placed(groupTabs(t).map((x) => x.name), id, target, side) });
        layoutPanes();
        render();
        return;
      }
      const w = wsNow.get(id);
      if (level === "ws" && !w) return; // it left the group meanwhile
      const list = level === "unit" ? sidebarUnits().map((u) => u.key) : wsProjects(w!.id).map((p) => p.key);
      const next = placed(list, src, target, side);
      org.order = [...next, ...org.order.filter((k) => !next.includes(k))];
      saveOrg();
    } else if (d.out) {
      org.ws[id] = "-";
      saveOrg();
    }
  };
  const up = () => finish(true);
  const cancel = () => finish(false);
  const esc = (ev: KeyboardEvent) => { if (ev.key === "Escape" && pdrag) { ev.preventDefault(); ev.stopPropagation(); finish(false); } };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
  window.addEventListener("pointercancel", cancel);
  window.addEventListener("keydown", esc, true);
  window.addEventListener("blur", cancel);
}
// The click that ends a drag doesn't open or fold anything.
$("tabs").addEventListener("click", (e) => {
  if (Date.now() - justDragged < 350) { e.stopPropagation(); e.preventDefault(); }
}, true);

/** A name for two projects: their shared first word, else "Group". */
function commonName(a: string, b: string): string {
  const w = (s: string) => s.toLowerCase().split(/[^\p{L}\p{N}]+/u).filter(Boolean)[0] ?? "";
  const x = w(a);
  if (x && x === w(b)) return x.charAt(0).toUpperCase() + x.slice(1);
  return "Group";
}

function toggleWs(id: string) {
  org.collapsed = org.collapsed.includes(id) ? org.collapsed.filter((x) => x !== id) : [...org.collapsed, id];
  saveOrg();
}

// ---------- project icons ----------
// A project's own icon (see icons.rs), else its initial on a color of its own.
const icons = new Map<string, string | null>();
const iconAsked = new Set<string>();

/** A project's color: the average of its icon, else the hue of its initial. */
const iconColors = new Map<string, string>();
function projectHue(proj: string) {
  let h = 0;
  for (const c of proj) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return h % 360;
}
function projectColor(proj: string): string {
  const c = iconColors.get(proj);
  if (c) return c;
  // hsl(h 55% 60%) as hex, readable on dark and light terminals.
  const h = projectHue(proj) / 360, s = 0.55, l = 0.6;
  const f = (n: number) => {
    const k = (n + h * 12) % 12;
    const v = l - s * Math.min(l, 1 - l) * Math.max(-1, Math.min(k - 3, 9 - k, 1));
    return Math.round(v * 255).toString(16).padStart(2, "0");
  };
  return `#${f(0)}${f(8)}${f(4)}`;
}
function measureIcon(proj: string, url: string) {
  const img = new Image();
  img.onload = () => {
    try {
      const c = document.createElement("canvas");
      c.width = c.height = 16;
      const g = c.getContext("2d")!;
      g.drawImage(img, 0, 0, 16, 16);
      const d = g.getImageData(0, 0, 16, 16).data;
      // The most colorful opaque pixels say more than a plain average.
      const px: [number, number, number, number][] = [];
      for (let i = 0; i < d.length; i += 4) {
        if (d[i + 3] < 200) continue;
        const [r, gg, b] = [d[i], d[i + 1], d[i + 2]];
        px.push([r, gg, b, Math.max(r, gg, b) - Math.min(r, gg, b)]);
      }
      if (!px.length) return;
      px.sort((a, b) => b[3] - a[3]);
      const top = px.slice(0, Math.max(1, Math.ceil(px.length / 3)));
      const avg = [0, 1, 2].map((k) => Math.round(top.reduce((s, p) => s + p[k], 0) / top.length));
      // Too dark or grey to read as text: keep the initial's hue.
      if (Math.max(...avg) < 70 || top[0][3] < 40) return;
      iconColors.set(proj, `#${avg.map((v) => v.toString(16).padStart(2, "0")).join("")}`);
      render();
    } catch { /* tainted or broken image: keep the hue */ }
  };
  img.src = url;
}

function paintIcon(el: HTMLElement, proj: string) {
  if (!iconAsked.has(proj)) {
    iconAsked.add(proj);
    invoke<string | null>("project_icon", { path: proj })
      .then((url) => { icons.set(proj, url ?? null); if (url) measureIcon(proj, url); })
      .catch(() => { icons.set(proj, null); })
      .finally(() => {
        if (!icons.get(proj)) return;
        render();
        // Overview cards are painted in place: a rebuild would drop clicks.
        $("overview").querySelectorAll<HTMLElement>("[data-icon]").forEach((el) => {
          if (el.dataset.proj === proj) paintIcon(el, proj);
        });
      });
  }
  const url = icons.get(proj);
  const key = url ? `img:${proj}` : `letter:${proj}`;
  if (el.dataset.k === key) return;
  el.dataset.k = key;
  if (url) {
    el.classList.remove("letter");
    el.style.removeProperty("--h");
    el.innerHTML = `<img src="${url}" alt="">`;
  } else {
    const l = projectLabel(proj).split(" / ").pop() ?? proj;
    el.classList.add("letter");
    el.style.setProperty("--h", String(projectHue(proj)));
    el.textContent = (l.match(/[\p{L}\p{N}]/u)?.[0] ?? "?").toUpperCase();
  }
}

function projectLabel(p: string) {
  if (org.names[p]) return org.names[p];
  const parts = p.split("/").filter(Boolean);
  const leaf = parts[parts.length - 1] ?? p;
  const parent = parts[parts.length - 2] ?? "";
  if (!parent || GENERIC_DIRS.has(parent.toLowerCase()) || leaf.toLowerCase().includes(parent.toLowerCase())) return leaf;
  return `${parent} / ${leaf}`;
}

const doneMarks = new Map<string, State>();

/** One sidebar element: a stable key, how to create it, how to update it. */
interface Item { key: string; tag: "div" | "button"; fill: (el: HTMLElement) => void }

/**
 * Updates `box` to show `items`. Existing elements are updated in place and
 * only replaced when the list itself changes. WebKit drops a click when the
 * element under the mouse is replaced between press and release, and the
 * sidebar refreshes every 2 s, so rebuilding it made rows hard to click.
 */
function sync(box: HTMLElement, items: Item[]) {
  const kids = [...box.children] as HTMLElement[];
  const same = kids.length === items.length && kids.every((k, i) => k.dataset.key === items[i].key);
  if (!same) {
    box.replaceChildren(...items.map((it) => {
      const el = document.createElement(it.tag);
      el.dataset.key = it.key;
      return el;
    }));
  }
  items.forEach((it, i) => it.fill(box.children[i] as HTMLElement));
}

function setText(el: HTMLElement, sel: string, text: string) {
  const c = el.querySelector(sel) as HTMLElement | null;
  if (!c) return;
  if (c.textContent !== text) c.textContent = text;
}

function rowFill(el: HTMLElement, cls: string, st: State, name: string, right: string, title: string, onClick: () => void, key = "") {
  if (!el.firstChild) el.innerHTML = `<i class="st"></i><span class="n"></span><span class="t"></span><kbd class="k"></kbd>`;
  if (el.className !== cls) el.className = cls;
  const dot = el.querySelector(".st") as HTMLElement;
  if (dot.className !== `st ${st}`) dot.className = `st ${st}`;
  setText(el, ".n", name);
  setText(el, ".t", right);
  // Coloured only when the text is the state (not "background", "manual").
  const t = el.querySelector(".t") as HTMLElement;
  const tc = right && right === SHORT[st] ? `t ${st}` : "t";
  if (t.className !== tc) t.className = tc;
  setText(el, ".k", key);
  if (el.title !== title) el.title = title;
  el.onclick = onClick;
}

/** The project (of `keys`) a session outside Toranj belongs to: the deepest whose folder holds its cwd. */
function extHome(e: External, keys: string[]): string | undefined {
  if (!e.cwd) return undefined;
  return keys.filter((k) => e.cwd === k || e.cwd.startsWith(k + "/")).sort((a, b) => b.length - a.length)[0];
}

/** Name of a Claude session outside Toranj; under its project the project goes. */
function extName(e: External, nested: boolean) {
  const same = (x: External) => projectLabel(x.cwd) === projectLabel(e.cwd);
  const short = e.unlisted ? `PID ${e.pid}` : e.name.split("-").pop() || e.session_id.slice(0, 4);
  if (nested) return e.title || (e.background && e.name ? e.name : short);
  if (!e.cwd) return e.title || (e.unlisted ? `Claude · PID ${e.pid}` : e.name || "Claude");
  const l = projectLabel(e.cwd);
  if (e.title) return `${l} · ${e.title}`;
  if (snap.external.filter(same).length < 2) return l;
  // Same folder twice: what tells them apart goes first, so it survives truncation.
  return `${short} · ${l}`;
}

/** A sidebar row for a Claude session outside Toranj (click: move it in). */
function extItem(e: External, nested: boolean, inWs = false): Item {
  return {
    key: `e:${e.session_id}`,
    tag: "button",
    fill: (d) => {
      const busyMoving = moving.has(e.session_id);
      const kind = e.unlisted ? "manual" : e.background ? "background" : "outside";
      rowFill(d, "sess ext" + (nested ? " sub" : "") + (inWs ? " in-ws" : "") + dragClass(`e:${e.session_id}`), e.state, extName(e, nested),
        busyMoving ? "moving…" : nested ? (e.state === "running" || e.state === "waiting" ? SHORT[e.state] || kind : kind) : e.unlisted ? "manual" : e.background ? "background" : SHORT[e.state],
        [e.title, `${e.background ? "Background session" : "Claude session in another terminal"}, outside Toranj${e.detail ? " · " + e.detail : ""}`, e.cwd,
          e.unlisted ? "Click for instructions on moving it" : "Click to move it into Toranj"].filter(Boolean).join("\n"),
        () => { if (e.unlisted) explainUnlisted(e); else if (!moving.has(e.session_id)) askMove([e]); });
      (d as HTMLButtonElement).disabled = busyMoving;
    },
  };
}

function render() {
  renderDock();
  // Same rule as showOv below; needed before the sidebar is drawn.
  // (A closed current tab falls back to a pane still on screen, see below.)
  const live = (n: string | null) => snap.tabs.some((t) => t.name === n);
  const ovUp = snap.tabs.length > 0 && (overview || (!live(current) && ![...panes.keys()].some(live)));
  const groups = new Map<string, Tab[]>();
  for (const t of snap.tabs) {
    const k = t.project || t.cwd;
    groups.set(k, [...(groups.get(k) ?? []), t]);
  }
  // Claude sessions outside Toranj in a folder of a project shown here sit
  // under that project, indented; the rest stay under "Outside Toranj".
  const nested = new Map<string, External[]>();
  const outside: External[] = [];
  for (const e of snap.external) {
    const home = extHome(e, [...groups.keys()]);
    if (home) nested.set(home, [...(nested.get(home) ?? []), e]);
    else outside.push(e);
  }
  const items: Item[] = [];
  let n = 0;
  loadWsFiles([...groups.keys()]);
  const wsMap = workspacesFor([...groups.keys()]);
  wsNow = wsMap;
  // Units in the sidebar: a workspace with its projects, or a project alone.
  const units = new Map<string, { label: string; ws?: Ws; projs: [string, Tab[]][] }>();
  for (const [proj, tabs] of groups) {
    const w = wsMap.get(proj);
    const k = w ? `w:${w.id}` : `p:${proj}`;
    const u = units.get(k) ?? { label: w ? w.label : projectLabel(proj), ws: w, projs: [] };
    u.projs.push([proj, tabs]);
    units.set(k, u);
  }
  // Your order first (dragged), then the rest by name.
  const rank = (k: string) => { const i = org.order.indexOf(k); return i < 0 ? Infinity : i; };
  const byName = (a: string, b: string) => a.localeCompare(b, undefined, { sensitivity: "base" });
  const keyOf = (u: { ws?: Ws; projs: [string, Tab[]][] }) => u.ws ? `w:${u.ws.id}` : `p:${u.projs[0][0]}`;
  const ordered = [...units.values()].sort((a, b) => (rank(keyOf(a)) - rank(keyOf(b))) || byName(a.label, b.label));
  for (const u of ordered) {
    u.projs.sort((a, b) => (rank(`p:${a[0]}`) - rank(`p:${b[0]}`)) || byName(leafLabel(a[0]), leafLabel(b[0])));
    const ws = u.ws;
    const shut = !!ws && org.collapsed.includes(ws.id);
    if (ws) {
      const all = u.projs.flatMap(([, ts]) => ts);
      const projs = u.projs.map(([p]) => p);
      items.push({
        key: `w:${ws.id}`,
        tag: "div",
        fill: (h) => {
          if (!h.firstChild) h.innerHTML = `<button class="wchev" aria-label="Collapse or expand"></button><span class="pn"></span><span class="wsum"></span><button class="wgrid" aria-label="All terminals of this workspace in a grid">▦</button>`;
          const gb = h.querySelector(".wgrid") as HTMLButtonElement;
          gb.title = `All terminals of ${ws.label} in a grid`;
          gb.onclick = (e) => { e.stopPropagation(); showWorkspaceGrid(all); };
          const curTab = snap.tabs.find((t) => t.name === current);
          const showing = !ovUp && !!curTab && wsNow.get(groupKey(curTab))?.id === ws.id && prefsFor(`ws:${ws.id}`).layout !== "single";
          h.className = "proj ws" + (shut ? " shut" : "") + (showing ? " on" : "") + dragClass(`w:${ws.id}`);
          h.dataset.ws = ws.id;
          // The first click of a double-click toggled it: undo that, then rename.
          h.title = `${ws.label}: ${projs.map(leafLabel).join(", ")}\nClick to see all its terminals together · drag to reorder · right-click to rename`;
          setText(h, ".pn", ws.label);
          setText(h, ".wchev", shut ? "▸" : "▾");
          const sts = all.map((t) => doneMarks.get(t.name) ?? t.state);
          const sum = shut ? (sts.includes("waiting") ? "waiting" : sts.includes("error") ? "error" : sts.includes("done") ? "done" : `${all.length}`) : "";
          setText(h, ".wsum", sum);
          (h.querySelector(".wsum") as HTMLElement).className = `wsum ${sum}`;
          // The chevron folds the group; the rest of the header shows it.
          h.onclick = (e) => {
            if ((e.target as HTMLElement).closest(".branch, .padd")) return;
            if ((e.target as HTMLElement).closest(".wchev")) toggleWs(ws.id);
            else if (e.detail < 2) showWorkspaceGrid(all);
          };
          h.oncontextmenu = (e) => workspaceMenu(e, ws, projs);
          h.onpointerdown = (e) => startSidebarDrag(e, `w:${ws.id}`);
        },
      });
      if (shut) continue;
    }
    for (const [proj, tabs] of u.projs) {
    items.push({
      key: `p:${proj}`,
      tag: "div",
      fill: (h) => {
        if (!h.firstChild) {
          h.innerHTML = `<i class="pi" aria-hidden="true"></i><span class="pn"></span><span class="branch"></span><button class="padd" aria-label="New terminal in this project">+</button>`;
          (h.querySelector(".padd") as HTMLButtonElement).onclick = (e) => {
            e.stopPropagation();
            openNew({ name: projectLabel(proj), path: proj });
          };
        }
        const inWs = wsMap.has(proj);
        h.className = "proj" + (inWs ? " in-ws" : "") + dragClass(`p:${proj}`);
        h.dataset.proj = proj;
        h.onpointerdown = (e) => startSidebarDrag(e, `p:${proj}`);
        // Clicking a project opens it: inside a group, the group's grid.
        h.onclick = (e) => {
          if ((e.target as HTMLElement).closest(".branch, button")) return;
          const first = snap.tabs.find((t) => groupKey(t) === proj && !closing.has(t.name));
          if (!first) return;
          if (wsNow.get(proj)) {
            const key = viewKey(first);
            const p = prefsFor(key);
            if (p.layout === "single") savePrefs(key, { ...p, layout: p.multi });
          }
          open(first.name);
        };
        h.title = `${proj}\nRight-click to rename or move to a workspace`;
        setText(h, ".pn", inWs ? leafLabel(proj) : projectLabel(proj));
        h.oncontextmenu = (e) => projectMenu(e, proj);
        paintIcon(h.querySelector(".pi") as HTMLElement, proj);
        (h.querySelector(".padd") as HTMLElement).title = `New terminal in ${projectLabel(proj)}`;
        const g = snap.git[proj];
        const br = h.querySelector(".branch") as HTMLElement;
        br.hidden = !g;
        if (g) {
          setText(h, ".branch", g.branch + (g.changed ? ` ●${g.changed}` : ""));
          br.title = `${gitText(g)}\nClick for details`;
          br.onclick = () => openGit(proj);
        }
      },
    });
    // Terminals in your order (the same one the grid uses).
    const torder = prefsFor(tabs[0] ? viewKey(tabs[0]) : "").order;
    const trank = (n: string) => { const i = torder.indexOf(n); return i < 0 ? Infinity : i; };
    for (const t of [...tabs].sort((a, b) => trank(a.name) - trank(b.name))) {
      const num = ++n;
      items.push({
        key: `t:${t.name}`,
        tag: "button",
        fill: (b) => {
          const st = doneMarks.get(t.name) ?? t.state;
          rowFill(b, "sess" + (ovUp ? "" : t.name === current ? " on" : panes.has(t.name) ? " vis" : "") + (t.title ? "" : " plain") + (wsMap.has(proj) ? " in-ws" : "") + dragClass(`t:${t.name}`), st,
            t.title || t.agent,
            st === "running" ? ago(t.since) : SHORT[st],
            [t.title, `${t.agent} · ${LABEL[st]}${t.detail ? " · " + t.detail : ""}`, t.name].filter(Boolean).join("\n"),
            () => open(t.name),
            num <= 9 ? `⌘${num}` : "");
          b.oncontextmenu = (e) => tabMenu(e, t.name);
          b.onpointerdown = (e) => startSidebarDrag(e, `t:${t.name}`);
        },
      });
    }
    for (const e of nested.get(proj) ?? []) items.push(extItem(e, true, wsMap.has(proj)));
    }
  }
  sync($("tabs"), items);
  // A group's rows form one box: tinted, rounded at its first and last row.
  for (const u of sidebarUnits()) {
    const box = u.key.startsWith("w:");
    u.rows.forEach((el, i) => {
      el.classList.toggle("wsbox", box);
      el.classList.toggle("wsbox-top", box && i === 0);
      el.classList.toggle("wsbox-end", box && i === u.rows.length - 1);
    });
  }
  sendLabels();

  const ext: Item[] = [];
  if (outside.length) {
    const movable = outside.filter((e) => !e.unlisted && (e.background || e.state === "idle") && !moving.has(e.session_id));
    ext.push({
      key: "ext-head",
      tag: "div",
      fill: (h) => {
        if (!h.firstChild) h.innerHTML = `<span class="pn"></span><button class="link small"></button>`;
        h.className = "proj proj-row";
        h.title = "Claude sessions started in other terminals (e.g. VS Code)";
        setText(h, ".pn", `Outside Toranj · ${outside.length}`);
        const all = h.querySelector("button") as HTMLButtonElement;
        all.hidden = movable.length < 2;
        setText(h, "button", `Move ${movable.length}`);
        all.title = "Move all sessions that are not working right now";
        all.onclick = () => askMove(movable);
      },
    });
    for (const e of outside) ext.push(extItem(e, false));
  }
  sync($("external"), ext);

  const waiting = snap.tabs.filter((t) => (doneMarks.get(t.name) ?? t.state) === "waiting").length;
  const finished = snap.tabs.filter((t) => doneMarks.get(t.name) === "done").length;
  const failed = snap.tabs.filter((t) => doneMarks.get(t.name) === "error").length;
  const q = $("q-wait");
  q.hidden = waiting + finished + failed === 0;
  q.classList.toggle("done", !waiting && !failed);
  const qText = [waiting ? `${waiting} waiting` : "", failed ? plural(failed, "error", "errors") : "", finished ? `${finished} done` : ""].filter(Boolean).join(" · ");
  if (q.dataset.t !== qText) {
    q.dataset.t = qText;
    // One kind spelled out; several as colored counts, so it stays one line.
    const kinds = [[waiting, "waiting"], [failed, "error"], [finished, "done"]].filter(([n]) => n) as [number, string][];
    q.innerHTML = (kinds.length === 1 ? escapeHtml(qText) : kinds.map(([n, k]) => `<span class="pc ${k}">${n}</span>`).join("")) + ` <kbd>⌘U</kbd>`;
    q.title = `${qText}\nGo to the next terminal that needs you (⌘U)`;
  }
  document.title = waiting ? `(${waiting}) Toranj` : "Toranj";
  setBadge(attentionCount());

  let cur = snap.tabs.find((t) => t.name === current);
  if (!cur && current) { // tab was closed elsewhere: move to a pane still on screen
    current = [...panes.keys()].find((n) => snap.tabs.some((t) => t.name === n)) ?? null;
    cur = snap.tabs.find((t) => t.name === current);
  }
  // Tabs opened or closed elsewhere join or leave the grid.
  const want = !cur ? [] : prefsFor(viewKey(cur)).layout === "single" ? [cur.name] : groupTabs(cur).map((t) => t.name);
  const have = [...$("grid").querySelectorAll<HTMLElement>(".tp")].map((el) => el.dataset.name);
  if (want.join("\n") !== have.join("\n")) layoutPanes();
  else paintPanes();
  if (!snap.tabs.length) overview = false;
  const showOv = snap.tabs.length > 0 && (overview || !cur);
  $("empty").hidden = !!cur || snap.tabs.length > 0;
  $("overview").hidden = !showOv;
  $("brand").classList.toggle("on", showOv);
  $("ov-row").hidden = snap.tabs.length === 0;
  $("ov-row").classList.toggle("on", showOv);
  $("head").hidden = !cur || showOv;
  if (showOv) renderOverview();
  const lay = $("layout");
  const groupSize = cur ? groupTabs(cur).length : 0;
  lay.hidden = groupSize < 2;
  if (cur && groupSize >= 2) {
    const active = prefsFor(viewKey(cur)).layout;
    lay.querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
      b.classList.toggle("on", b.dataset.layout === active);
      b.setAttribute("aria-pressed", String(b.dataset.layout === active));
    });
  }
  if (cur) {
    const st = doneMarks.get(cur.name) ?? cur.state;
    $("h-st").className = `st ${st}`;
    const hp = groupKey(cur);
    paintIcon($("h-ico"), hp);
    const hTitle = cur.title || cur.agent;
    const hKey = `${projectLabel(hp)}\n${hTitle}`;
    if ($("h-name").dataset.k !== hKey) {
      $("h-name").dataset.k = hKey;
      $("h-name").innerHTML = `<b>${escapeHtml(projectLabel(hp))}</b><span> · ${escapeHtml(hTitle)}</span>`;
    }
    $("h-name").title = cur.title ? `${cur.agent} · ${cur.name}` : cur.name;
    const g = snap.git[cur.project];
    // The project is in the name already; the path only says something when
    // the terminal runs elsewhere (a worktree, a subfolder).
    const parts = cur.cwd !== cur.project ? [cur.cwd.replace(/^\/Users\/[^/]+/, "~")] : [];
    if (cur.detail) parts.push(cur.detail);
    $("h-meta").title = cur.cwd;
    $("h-meta").textContent = parts.join("  ·  ");
    const gb = $("h-git");
    gb.hidden = !g;
    if (g) {
      gb.textContent = gitText(g);
      gb.classList.toggle("dirty", g.changed > 0 || g.ahead > 0);
    }
  } else {
    $("h-git").hidden = true;
  }
}

let firstRefresh = true;
let refreshSeq = 0;
let inflight: Promise<void> | null = null;
let queued: Promise<void> | null = null;

// Only adapters with a real status source can say "finished". A generic
// terminal going quiet may just be thinking, so it only signals via the bell.
const preciseDone = (t: Tab) => t.agent === "claude";

/**
 * Loads a fresh snapshot. A slow ps/tmux call must not let snapshots overlap,
 * so a call made meanwhile waits for the running one and then loads once
 * more. The returned promise always resolves after a snapshot that started
 * after the call, so `await refresh(); open(newTab)` sees the new tab.
 */
function refresh(): Promise<void> {
  if (!inflight) {
    inflight = loadSnapshot().finally(() => { inflight = null; });
    return inflight;
  }
  if (!queued) queued = inflight.then(() => { queued = null; return refresh(); });
  return queued;
}

async function loadSnapshot() {
  const seq = ++refreshSeq;
  try {
    const s = await invoke<Snapshot>("snapshot");
    if (seq !== refreshSeq) return;
    const focused = document.hasFocus();
    for (const t of s.tabs) {
      const before = prevState.get(t.name);
      const watching = panes.has(t.name) && focused && !ovShown();
      if (before === "running" && t.state !== "running" && !watching && preciseDone(t)) {
        const mark: State = t.state === "idle" ? "done" : t.state;
        doneMarks.set(t.name, mark);
        if (t.project) invoke("refresh_git", { path: t.project }); // the agent probably changed files
        if (mark === "done" || mark === "error") notifyTab(t.name, tabTitle(t), mark, t.detail, soundFor(mark));
      }
      // Asking something new outranks "finished": it gets the waiting card.
      if (t.state === "running" || t.state === "waiting" || watching) doneMarks.delete(t.name);
      // On screen, but not the pane you're in: its border turns green or red.
      if (before === "running" && t.state !== "running" && panes.has(t.name) && t.name !== current && preciseDone(t)) {
        paneDone.set(t.name, t.state === "error" ? "error" : "done");
      }
      if (t.state === "running" || t.state === "waiting") paneDone.delete(t.name);
      if (t.state === "waiting" && before !== "waiting" && !firstRefresh && !watching) {
        notifyTab(t.name, tabTitle(t), "waiting", t.detail, soundFor("waiting"));
      }
      prevState.set(t.name, t.state);
    }
    for (const name of [...doneMarks.keys(), ...paneDone.keys()]) {
      if (!s.tabs.some((t) => t.name === name)) { doneMarks.delete(name); paneDone.delete(name); }
    }
    firstRefresh = false;
    snap = s;
    render();
    updateDock();
  } catch (e) {
    console.error(e);
  }
}

/** Tabs that need the user: waiting (unless being looked at), or finished and not seen. */
function attentionCount() {
  const focused = document.hasFocus();
  return snap.tabs.filter((t) => {
    if (doneMarks.has(t.name)) return true;
    return t.state === "waiting" && !(panes.has(t.name) && focused);
  }).length;
}

function gitText(g: Git) {
  const bits = [`⎇ ${g.branch}`];
  bits.push(g.changed ? plural(g.changed, "change", "changes") : "clean");
  if (g.ahead) bits.push(`↑${g.ahead}`);
  if (g.behind) bits.push(`↓${g.behind}`);
  return bits.join(" ");
}

function tabTitle(t: Tab) {
  return `${projectLabel(t.project || t.cwd)} · ${t.title || t.agent}`;
}

/** Goes to the next terminal that needs you: waiting ones first, then finished ones not seen yet. */
function nextWaiting() {
  const st = (t: Tab) => doneMarks.get(t.name) ?? t.state;
  // One cycle, waiting ones first, so a waiting tab you're on doesn't hide the rest.
  const w = [...snap.tabs.filter((t) => st(t) === "waiting"), ...snap.tabs.filter((t) => st(t) !== "waiting" && doneMarks.has(t.name))];
  if (!w.length) return;
  const i = w.findIndex((t) => t.name === current);
  open(w[(i + 1) % w.length].name);
}

// ---------- overview ----------
// All projects on one page: who works, who waits, and whether git is clean.
let overview = false;
let ovSig = "";
/** What the summary line narrows the cards to ("" = all projects). */
type OvFilter = "" | State | "dirty" | "ahead";
let ovFilter: OvFilter = "";
const ovShown = () => !$("overview").hidden;

function toggleOverview(show = !ovShown()) {
  if (!show && !current) return; // nothing selected: the overview is the page
  recordMove({ tab: current, ov: show });
  overview = show && snap.tabs.length > 0;
  if (overview) rememberView("");
  render();
  if (overview) $("overview").focus();
  else panes.get(current ?? "")?.term.focus();
}

interface OvProject { key: string; tabs: Tab[]; git?: Git; score: number }

function ovProjects(): OvProject[] {
  const by = new Map<string, Tab[]>();
  for (const t of snap.tabs) if (!closing.has(t.name)) by.set(groupKey(t), [...(by.get(groupKey(t)) ?? []), t]);
  const st = (t: Tab) => doneMarks.get(t.name) ?? t.state;
  return [...by].map(([key, tabs]) => {
    const git = snap.git[key];
    const score = (tabs.some((t) => st(t) === "waiting" || st(t) === "error") ? 8 : 0)
      + (tabs.some((t) => st(t) === "done") ? 4 : 0)
      + (tabs.some((t) => st(t) === "running") ? 2 : 0)
      + (git && (git.changed || git.ahead) ? 1 : 0);
    return { key, tabs, git, score };
  }).sort((a, b) => b.score - a.score || projectLabel(a.key).localeCompare(projectLabel(b.key)));
}

function renderOverview() {
  const list = ovProjects();
  const st = (t: Tab) => doneMarks.get(t.name) ?? t.state;
  const all = snap.tabs.filter((t) => !closing.has(t.name));
  // Rebuilt only when something shown changes (see sync() on dropped clicks);
  // details count only where they are shown, so running agents don't churn it.
  const keys = list.map((p) => p.key);
  const extOf = (key: string) => snap.external.filter((e) => extHome(e, keys) === key);
  const sig = JSON.stringify([ovFilter, list.map((p) => [p.key, wsNow.get(p.key)?.id ?? "", wsNow.get(p.key)?.label ?? "", p.git, p.tabs.map((t) => [t.name, t.title, t.agent, st(t), st(t) === "waiting" || st(t) === "error" ? t.detail : "", sidebarNumber(t.name)]),
    extOf(p.key).map((e) => [e.session_id, e.title, e.name, e.state, e.background, e.unlisted, e.detail])]), snap.external.length]);
  if (sig === ovSig) return;
  ovSig = sig;

  const count = (s: State) => all.filter((t) => st(t) === s).length;
  const dirty = list.filter((p) => p.git?.changed).length;
  const ahead = list.filter((p) => p.git?.ahead).length;
  const withGit = list.filter((p) => p.git).length;
  const match = (p: OvProject, f: OvFilter) =>
    f === "" ? true
    : f === "dirty" ? !!p.git?.changed
    : f === "ahead" ? !!p.git?.ahead
    : p.tabs.some((t) => st(t) === f);
  // The state behind a filter may be gone by now: show everything again.
  if (ovFilter && !list.some((p) => match(p, ovFilter))) ovFilter = "";
  const chip = (f: OvFilter, html: string, cls = "", tip = "Show only these projects") =>
    `<button class="ov-chip ${cls}${ovFilter === f ? " on" : ""}" data-f="${f}" title="${ovFilter === f && f ? "Show all projects" : tip}">${html}</button>`;
  const bits: string[] = [chip("", `${plural(list.length, "project", "projects")} · ${plural(all.length, "terminal", "terminals")}`, "", "Show all projects")];
  if (count("waiting")) bits.push(chip("waiting", `${count("waiting")} waiting`, "c-wait b"));
  if (count("error")) bits.push(chip("error", plural(count("error"), "error", "errors"), "c-err b"));
  if (count("done")) bits.push(chip("done", `${count("done")} finished`, "c-done b"));
  if (count("running")) bits.push(chip("running", `${count("running")} running`, "c-run"));
  const gitBits: string[] = [];
  if (dirty) gitBits.push(chip("dirty", `${plural(dirty, "project", "projects")} with uncommitted changes`, "c-wait b"));
  if (ahead) gitBits.push(chip("ahead", `${plural(ahead, "project", "projects")} with unpushed commits`, "c-wait b"));
  if (!gitBits.length && withGit) gitBits.push(`<span class="c-done">git: everything committed and pushed</span>`);

  const ext = snap.external.length
    ? `<p class="ov-ext"><button class="ov-chip" data-ext title="Show them in the sidebar">${plural(snap.external.length, "Claude session runs", "Claude sessions run")} outside Toranj</button></p>` : "";
  const cardHtml = list.map((p, i) => {
    if (!match(p, ovFilter)) return "";
    const g = p.git;
    const gitBtn = g ? `<button class="btn gitbtn${g.changed || g.ahead ? " dirty" : ""}" data-git="${i}" title="Git details">${escapeHtml(gitText(g))}</button>` : "";
    const rows = p.tabs.map((t) => {
      const s = st(t);
      const num = sidebarNumber(t.name);
      const right = s === "running" ? "running" : SHORT[s];
      return `<button class="ov-tab" data-tab="${escapeHtml(t.name)}" title="${escapeHtml(LABEL[s])}">
        <i class="st ${s}"></i><span class="n${t.title ? "" : " plain"}">${escapeHtml(t.title || t.agent)}</span>
        ${t.detail && (s === "waiting" || s === "error") ? `<span class="d">${escapeHtml(t.detail)}</span>` : ""}
        <span class="t ${right ? s : ""}">${right}</span>${num ? `<kbd>⌘${num}</kbd>` : ""}</button>`;
    }).join("") + extOf(p.key).map((e) => {
      const kind = e.unlisted ? "manual" : e.background ? "background" : "outside Toranj";
      return `<button class="ov-tab sub" data-ext-move="${escapeHtml(e.session_id)}" title="${escapeHtml(e.unlisted ? "Runs outside Toranj · click for how to move it" : "Runs outside Toranj · click to move it in")}">
        <i class="st ${e.state}"></i><span class="n">${escapeHtml(extName(e, true))}</span><span class="t">${kind}</span></button>`;
    }).join("");
    return `<section class="ov-card${p.score >= 4 ? " attn" : ""}${p.tabs.length ? " go" : ""}" data-card="${i}" title="${p.tabs.length ? "Open this project's terminal" : ""}">
      <header><i class="pi" data-icon="${i}" data-proj="${escapeHtml(p.key)}" aria-hidden="true"></i><button class="ov-name" data-first="${i}" title="Open its first terminal · ${escapeHtml(p.key)}">${escapeHtml(projectLabel(p.key))}</button>${gitBtn}</header>
      <div class="ov-path">${escapeHtml(tilde(p.key))}</div>
      <div class="ov-tabs">${rows}</div>
      <footer><button class="link" data-new="${i}">+ Terminal</button><button class="link" data-code="${i}">Open in VS Code</button></footer>
    </section>`;
  });
  // A group's projects sit together in one box, where its first project
  // would be (the list is sorted by what needs you).
  const doneWs = new Set<string>();
  const cards = list.map((p, i) => {
    const w = wsNow.get(p.key);
    if (!w) return cardHtml[i];
    if (doneWs.has(w.id)) return "";
    doneWs.add(w.id);
    const members = list.map((q, j) => [q, j] as const).filter(([q]) => wsNow.get(q.key)?.id === w.id);
    const inner = members.map(([, j]) => cardHtml[j]).join("");
    if (!inner) return "";
    const n = members.reduce((s, [q]) => s + q.tabs.length, 0);
    return `<div class="ov-group" data-ovgroup="${escapeHtml(w.id)}" title="Show all its terminals together">
      <button class="ov-ghead" data-ovws="${escapeHtml(w.id)}" title="Show all its terminals together">
        <i aria-hidden="true">▦</i><b>${escapeHtml(w.label)}</b><span>${plural(members.length, "project", "projects")} · ${plural(n, "terminal", "terminals")}</span>
      </button>
      <div class="ov-ggrid">${inner}</div>
    </div>`;
  }).join("");

  const box = $("overview");
  // Keep keyboard focus across the rebuild.
  const fa = document.activeElement as HTMLElement | null;
  const focusKey = fa && box.contains(fa) && fa !== box
    ? ["tab", "git", "new", "code", "f", "first", "extMove"].map((k) => fa.dataset[k] !== undefined ? `[data-${k.replace(/[A-Z]/g, (c) => "-" + c.toLowerCase())}="${CSS.escape(fa.dataset[k]!)}"]` : "").find(Boolean) ?? "" : "";
  box.innerHTML = `<div class="ov-head">
      <h2>Overview</h2>
      <p class="ov-chips">${bits.join("")}</p>
      <p class="ov-chips">${gitBits.join("")}</p>${ext}
    </div>
    <div class="ov-grid">${cards}</div>`;
  box.querySelectorAll<HTMLElement>("[data-icon]").forEach((el) => paintIcon(el, list[Number(el.dataset.icon)].key));
  const showGroup = (id: string) => showWorkspaceGrid(snap.tabs.filter((t) => wsNow.get(groupKey(t))?.id === id));
  box.querySelectorAll<HTMLButtonElement>("[data-ovws]").forEach((b) => b.addEventListener("click", () => showGroup(b.dataset.ovws!)));
  // Anywhere in a group's box opens the group; its cards and buttons keep
  // their own actions (a card opens its terminal, inside the group's grid).
  box.querySelectorAll<HTMLElement>("[data-ovgroup]").forEach((g) => g.addEventListener("click", (e) => {
    const t = e.target as HTMLElement;
    if (t.closest("button, a, .ov-card") || getSelection()?.toString()) return;
    showGroup(g.dataset.ovgroup!);
  }));
  box.querySelectorAll<HTMLButtonElement>("[data-tab]").forEach((b) => b.addEventListener("click", () => open(b.dataset.tab!)));
  box.querySelectorAll<HTMLButtonElement>("[data-ext-move]").forEach((b) => b.addEventListener("click", () => {
    const e = snap.external.find((x) => x.session_id === b.dataset.extMove);
    if (!e) return;
    if (e.unlisted) explainUnlisted(e);
    else if (!moving.has(e.session_id)) askMove([e]);
  }));
  box.querySelectorAll<HTMLButtonElement>("[data-tab]").forEach((b) => b.addEventListener("contextmenu", (e) => tabMenu(e, b.dataset.tab!)));
  box.querySelectorAll<HTMLButtonElement>("[data-f]").forEach((b) => b.addEventListener("click", () => {
    const f = b.dataset.f as OvFilter;
    ovFilter = ovFilter === f ? "" : f;
    renderOverview();
  }));
  box.querySelector<HTMLButtonElement>("[data-ext]")?.addEventListener("click", () => {
    // Under their projects, or under Outside Toranj.
    const first = document.querySelector<HTMLElement>("#tabs .sess.ext, #external .sess");
    first?.scrollIntoView({ block: "nearest", behavior: "smooth" });
    first?.focus();
    $("external").classList.remove("flash");
    void $("external").offsetWidth; // restart the animation
    $("external").classList.add("flash");
  });
  const openFirst = (i: number) => {
    const p = list[i];
    const t = p.tabs.find((x) => st(x) === "waiting") ?? p.tabs[0];
    if (t) open(t.name);
  };
  box.querySelectorAll<HTMLButtonElement>("[data-first]").forEach((b) => b.addEventListener("click", () => openFirst(Number(b.dataset.first))));
  // The whole card opens the project; its own buttons keep their actions.
  box.querySelectorAll<HTMLElement>("[data-card]").forEach((c) => c.addEventListener("click", (e) => {
    if ((e.target as HTMLElement).closest("button, a")) return;
    if (getSelection()?.toString()) return; // selecting the path to copy it
    openFirst(Number(c.dataset.card));
  }));
  box.querySelectorAll<HTMLButtonElement>("[data-git]").forEach((b) => b.addEventListener("click", () => openGit(list[Number(b.dataset.git)].key)));
  box.querySelectorAll<HTMLButtonElement>("[data-code]").forEach((b) => b.addEventListener("click", () => {
    invoke("open_in_editor", { path: list[Number(b.dataset.code)].key }).catch((e) => console.error(e));
  }));
  box.querySelectorAll<HTMLButtonElement>("[data-new]").forEach((b) => b.addEventListener("click", async () => {
    const key = list[Number(b.dataset.new)].key;
    try {
      const name = await invoke<string>("create_tab", { project: key, cwd: key, command: "" });
      await refresh();
      open(name);
    } catch (e) {
      showOvError(`Terminal did not open: ${e}`);
    }
  }));
  if (focusKey) (box.querySelector<HTMLElement>(focusKey) ?? box).focus();
  else if (fa && box.contains(fa)) box.focus();
}

function showOvError(msg: string) {
  const dlg = $<HTMLDialogElement>("err-dlg");
  $("err-msg").textContent = msg;
  if (!dlg.open) dlg.showModal();
}

$("brand").onclick = () => toggleOverview();
$("ov-row").onclick = () => toggleOverview(true);
window.addEventListener("keydown", (e) => {
  if (e.key !== "Escape" || !ovShown() || !current || document.querySelector("dialog[open]") || !$("ctx").hidden || !$("res-panel").hidden) return;
  e.preventDefault();
  toggleOverview(false);
});

// ---------- settings dialog ----------
interface Picker { update(disabled: boolean): void }
const soundPickers: Picker[] = [];

/**
 * A dropdown in the app's colors (a native select opens as a macOS menu).
 * The list is fixed-positioned so the dialog doesn't clip or scroll it.
 */
function picker(host: HTMLElement, options: string[], get: () => string, set: (v: string) => void): Picker {
  // The label and the current value are both announced.
  const id = host.id;
  const label = host.dataset.label ?? "";
  host.innerHTML = `<button type="button" class="pick-btn" id="${id}-btn" role="combobox" aria-haspopup="listbox" aria-controls="${id}-list" aria-expanded="false" aria-labelledby="${label} ${id}-val"><span id="${id}-val"></span><i aria-hidden="true">▾</i></button>
    <div class="pick-list" id="${id}-list" role="listbox" hidden>${options.map((o, i) => `<div role="option" id="${id}-o${i}" data-v="${escapeHtml(o)}">${escapeHtml(o)}</div>`).join("")}</div>`;
  const btn = host.querySelector<HTMLButtonElement>(".pick-btn")!;
  const list = host.querySelector<HTMLElement>(".pick-list")!;
  const items = [...list.children] as HTMLElement[];
  let hi = -1;
  const mark = (i: number) => {
    hi = i;
    items.forEach((el, j) => el.classList.toggle("hi", j === i));
    if (items[i]) btn.setAttribute("aria-activedescendant", items[i].id);
    items[i]?.scrollIntoView({ block: "nearest" });
  };
  const close = (focus = true) => {
    if (list.hidden) return;
    list.hidden = true;
    btn.setAttribute("aria-expanded", "false");
    btn.removeAttribute("aria-activedescendant");
    if (focus) btn.focus();
  };
  const choose = (i: number) => {
    close();
    if (options[i] !== undefined) set(options[i]);
  };
  const show = () => {
    const r = btn.getBoundingClientRect();
    list.style.left = `${r.left}px`;
    list.style.minWidth = `${r.width}px`;
    list.hidden = false;
    const h = list.offsetHeight;
    list.style.top = `${r.bottom + 4 + h > innerHeight ? Math.max(8, r.top - 4 - h) : r.bottom + 4}px`;
    btn.setAttribute("aria-expanded", "true");
    btn.focus(); // WebKit doesn't focus a clicked button: keys must reach it
    mark(Math.max(0, options.indexOf(get())));
  };
  btn.onclick = () => (list.hidden ? show() : close());
  btn.onkeydown = (e) => {
    if (e.metaKey || e.ctrlKey || e.altKey) return; // app shortcuts
    if (list.hidden) {
      if (e.key === "ArrowDown" || e.key === "ArrowUp") { e.preventDefault(); show(); }
      return;
    }
    if (e.key === "ArrowDown") mark(Math.min(options.length - 1, hi + 1));
    else if (e.key === "ArrowUp") mark(Math.max(0, hi - 1));
    else if (e.key === "Enter" || e.key === " ") choose(hi);
    else if (e.key === "Escape") close(); // the list, not the dialog
    else if (e.key === "Tab") { close(false); return; }
    else if (e.key.length === 1) { // type-ahead
      const i = options.findIndex((o) => o.toLowerCase().startsWith(e.key.toLowerCase()));
      if (i >= 0) mark(i);
    } else return;
    e.preventDefault();
    e.stopPropagation();
  };
  list.onmousemove = (e) => {
    const el = (e.target as HTMLElement).closest<HTMLElement>("[data-v]");
    if (el) mark(items.indexOf(el));
  };
  list.onmousedown = (e) => e.preventDefault(); // keep focus on the button
  list.onclick = (e) => {
    const el = (e.target as HTMLElement).closest<HTMLElement>("[data-v]");
    if (el) choose(items.indexOf(el));
  };
  document.addEventListener("pointerdown", (e) => { if (!host.contains(e.target as Node)) close(false); }, true);
  host.closest("dialog")?.addEventListener("close", () => close(false));
  return {
    update(disabled) {
      btn.querySelector("span")!.textContent = get();
      btn.disabled = disabled;
      if (disabled) close(false);
      items.forEach((el) => el.setAttribute("aria-selected", String(el.dataset.v === get())));
    },
  };
}

function saveSettings() {
  try { localStorage.setItem("toranj.settings", JSON.stringify(settings)); } catch { /* not saved; still applied */ }
}

function applySettings() {
  document.documentElement.dataset.theme = isLight() ? "light" : "dark";
  for (const p of panes.values()) {
    p.term.options.fontSize = settings.fontSize;
    p.term.options.theme = termTheme();
    p.refit();
  }
  $("set-font").textContent = `${settings.fontSize} px`;
  $<HTMLButtonElement>("set-font-dn").disabled = settings.fontSize <= FONT_MIN;
  $<HTMLButtonElement>("set-font-up").disabled = settings.fontSize >= FONT_MAX;
  $("set-theme").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    b.classList.toggle("on", b.dataset.theme === settings.theme);
    b.setAttribute("aria-pressed", String(b.dataset.theme === settings.theme));
  });
  $("set-sound").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    const on = (b.dataset.sound === "on") === settings.sound;
    b.classList.toggle("on", on);
    b.setAttribute("aria-pressed", String(on));
  });
  for (const p of soundPickers) p.update(!settings.sound);
  $("set-quick").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
    const on = Number(b.dataset.secs) === settings.quickSecs;
    b.classList.toggle("on", on);
    b.setAttribute("aria-pressed", String(on));
  });
  quickKeyPicker?.update(!settings.quickSecs);
}
let quickKeyPicker: Picker | null = null;

function setFont(n: number) {
  settings.fontSize = Math.min(FONT_MAX, Math.max(FONT_MIN, n));
  saveSettings();
  applySettings();
}

interface StatusLineStatus { installed: boolean; other: boolean; settings_path: string }
async function paintStatusLine() {
  const b = $<HTMLButtonElement>("set-sl");
  const note = $("set-sl-note");
  try {
    const s = await invoke<StatusLineStatus>("claude_statusline_status");
    b.disabled = s.other;
    b.setAttribute("aria-checked", String(s.installed));
    b.dataset.on = String(s.installed);
    note.textContent = s.other
      ? "You already have a status line in Claude Code, so Toranj leaves it alone."
      : `Shows "■ project" in its color under Claude's input box, in Toranj only. Turning it on adds a statusLine to ${s.settings_path.replace(/^\/Users\/[^/]+/, "~")} (a backup is made first).`;
  } catch (e) {
    b.disabled = true; // unknown state: don't write blind
    note.textContent = `Could not read Claude's settings: ${e}`;
  }
}
$("set-sl").onclick = async () => {
  const b = $<HTMLButtonElement>("set-sl");
  b.disabled = true;
  try {
    await invoke(b.dataset.on === "true" ? "uninstall_claude_statusline" : "install_claude_statusline");
  } catch (e) {
    $("set-sl-note").textContent = String(e);
    b.disabled = false;
    return;
  }
  b.disabled = false;
  paintStatusLine();
};

// ---------- phone ----------
interface MobileDevice { id: string; name: string; created: number; last_seen: number; push: boolean }
interface PushPrefs { done: boolean; waiting: boolean; error: boolean; only_away: boolean }
interface MobileStatus { enabled: boolean; running: boolean; port: number; tailscale_installed: boolean; tailscale_name: string | null; serving: boolean; funnel: boolean; url: string | null; devices: MobileDevice[]; push: PushPrefs }
let mobile: MobileStatus | null = null;
let mobileSeq = 0;
let pairTimer = 0;

function mobileErr(msg: string) {
  $("set-mob-err").textContent = msg;
  $("set-mob-err").hidden = !msg;
}

async function paintMobile(s?: MobileStatus) {
  const seq = ++mobileSeq;
  const b = $<HTMLButtonElement>("set-mob");
  if (!mobile) b.disabled = true; // unknown state: don't toggle blind
  let m: MobileStatus;
  try { m = s ?? await invoke<MobileStatus>("mobile_status"); }
  catch (e) { if (seq === mobileSeq) mobileErr(String(e)); return; }
  if (seq !== mobileSeq) return; // a newer paint is on its way
  mobile = m;
  b.disabled = false;
  b.setAttribute("aria-checked", String(m.enabled));
  const note = $("set-mob-note");
  note.replaceChildren();
  if (!m.tailscale_installed) {
    note.append("Uses Tailscale (free) so only your own devices can reach Toranj. Install it on this Mac and your phone and sign in with the same account. ");
    const a = document.createElement("button");
    a.type = "button"; a.className = "link"; a.textContent = "Get Tailscale";
    a.onclick = () => invoke("open_url", { url: "https://tailscale.com/download" });
    note.append(a);
  } else if (!m.tailscale_name) {
    note.textContent = "Open Tailscale on this Mac and sign in. Your phone needs it too, with the same account.";
  } else if (m.enabled && m.funnel) {
    note.textContent = "Warning: Tailscale Funnel is on for port 8443, which would put Toranj on the internet. Run “tailscale funnel 8443 off”. Pairing is blocked until then.";
  } else if (m.enabled && m.url) {
    note.textContent = `On your phone: ${m.url}. Only devices signed in to Tailscale with your account get through, and each needs to be paired.`;
  } else if (m.enabled && !m.running) {
    note.textContent = "Phone access is on, but Toranj's server isn't running. Turn it off and on again.";
  } else if (m.enabled) {
    note.textContent = "Tailscale is not publishing Toranj yet. Turn this off and on again; if it still fails, turn on HTTPS certificates in the Tailscale admin console (DNS page).";
  } else {
    note.textContent = "See your terminals, answer Claude and get notifications on your phone, through Tailscale. Nothing is reachable from the internet.";
  }
  $("set-mob-body").hidden = !m.enabled;
  const devs = $("set-mob-devs");
  devs.replaceChildren();
  for (const d of m.devices) {
    const row = document.createElement("div");
    row.className = "mob-dev";
    const n = document.createElement("span");
    n.textContent = d.name;
    const meta = document.createElement("small");
    meta.textContent = `${d.push ? "notifications on" : "notifications off"} · seen ${ago(d.last_seen)} ago`;
    const test = document.createElement("button");
    test.type = "button"; test.className = "link"; test.textContent = "Test";
    test.hidden = !d.push;
    test.onclick = () => invoke("mobile_test_push", { id: d.id }).then(() => { test.textContent = "Sent"; setTimeout(() => { test.textContent = "Test"; }, 2000); }, (e) => mobileErr(String(e)));
    const rm = document.createElement("button");
    rm.type = "button"; rm.className = "link danger"; rm.textContent = "Remove";
    rm.onclick = async () => { await invoke("mobile_revoke", { id: d.id }).catch((e) => mobileErr(String(e))); paintMobile(); };
    const left = document.createElement("div");
    left.append(n, meta);
    row.append(left, test, rm);
    devs.append(row);
  }
  $("set-mob-push").querySelectorAll<HTMLInputElement>("input[data-k]").forEach((i) => {
    i.checked = m.push[i.dataset.k as keyof PushPrefs];
  });
  if (!m.enabled) hidePair();
}

function hidePair() {
  clearInterval(pairTimer);
  $("set-mob-pair").hidden = true;
}

$("set-mob").onclick = async () => {
  const b = $<HTMLButtonElement>("set-mob");
  if (!mobile) return;
  b.disabled = true;
  mobileErr("");
  try { await paintMobile(await invoke<MobileStatus>("mobile_set_enabled", { on: !(mobile?.enabled ?? false) })); }
  catch (e) { mobileErr(String(e)); paintMobile(); }
  b.disabled = false;
};
$("set-mob-pairbtn").onclick = async () => {
  mobileErr("");
  try {
    const p = await invoke<{ code: string; url: string; qr_svg: string; expires_in: number }>("mobile_pair");
    $<HTMLImageElement>("set-mob-qr").src = `data:image/svg+xml,${encodeURIComponent(p.qr_svg)}`;
    $("set-mob-url").textContent = p.url;
    $("set-mob-code").textContent = `${p.code.slice(0, 4)} ${p.code.slice(4)}`;
    $("set-mob-pair").hidden = false;
    const until = Date.now() + p.expires_in * 1000;
    clearInterval(pairTimer);
    const tick = () => {
      const left = Math.max(0, Math.round((until - Date.now()) / 1000));
      $("set-mob-exp").textContent = left ? `Valid for ${Math.floor(left / 60)}:${String(left % 60).padStart(2, "0")}, once.` : "Expired. Press “Pair a phone” again.";
      if (!left) clearInterval(pairTimer);
    };
    tick();
    pairTimer = window.setInterval(tick, 1000);
  } catch (e) { mobileErr(String(e)); }
};
$("set-mob-push").addEventListener("change", () => {
  const prefs = {} as PushPrefs;
  $("set-mob-push").querySelectorAll<HTMLInputElement>("input[data-k]").forEach((i) => { prefs[i.dataset.k as keyof PushPrefs] = i.checked; });
  invoke("mobile_set_push", { prefs }).catch((e) => mobileErr(String(e)));
});
// A phone paired or turned on notifications: show it, and drop the used code.
void listen<string>("mobile-changed", (e) => {
  if (e.payload === "paired") hidePair(); // the code is used up
  if ($<HTMLDialogElement>("set-dlg").open) paintMobile();
});

function openSettings() {
  if (document.querySelector("dialog[open]")) return;
  hideMenu();
  applySettings();
  paintStatusLine();
  mobileErr("");
  paintMobile();
  $<HTMLDialogElement>("set-dlg").showModal();
}

$("set-btn").onclick = openSettings;
$("set-font-dn").onclick = () => setFont(settings.fontSize - 1);
$("set-font-up").onclick = () => setFont(settings.fontSize + 1);
$("set-font-reset").onclick = () => setFont(13);
$("set-theme").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
  b.onclick = () => { settings.theme = b.dataset.theme as Theme; saveSettings(); applySettings(); };
});
const pickSound = (key: "doneSound" | "waitSound") => (v: string) => {
  settings[key] = v;
  saveSettings(); applySettings();
  playSound(v); // hear it
};
soundPickers.push(
  picker($("set-done-sound"), SOUNDS, () => settings.doneSound, pickSound("doneSound")),
  picker($("set-wait-sound"), SOUNDS, () => settings.waitSound, pickSound("waitSound")),
);
$("set-quick").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
  b.onclick = () => { settings.quickSecs = Number(b.dataset.secs); saveSettings(); applySettings(); };
});
quickKeyPicker = picker($("set-quick-key"), Object.keys(QUICK_KEYS), () => Object.entries(QUICK_KEYS).find(([, c]) => c === settings.quickKey)?.[0] ?? "Space", (v) => {
  settings.quickKey = QUICK_KEYS[v] ?? "Space";
  saveSettings(); applySettings();
});
$("set-sound").querySelectorAll<HTMLButtonElement>("button").forEach((b) => {
  b.onclick = () => {
    settings.sound = b.dataset.sound === "on";
    saveSettings(); applySettings();
    playSound(soundFor("done")); // a sample
  };
});
// Sent after a delay, so it can be tried with another app in front too.
$("set-test").onclick = () => {
  const b = $<HTMLButtonElement>("set-test");
  b.disabled = true;
  b.textContent = "Sending in 5 s…";
  setTimeout(() => {
    notifyTab("", "Toranj", "done", "Test notification", soundFor("done"));
    b.disabled = false;
    b.textContent = "Send a test notification";
  }, 5000);
};
$("set-dlg").addEventListener("close", () => { hidePair(); panes.get(current ?? "")?.term.focus(); });
lightQuery.addEventListener("change", () => { if (settings.theme === "system") applySettings(); });

// ---------- quick switch (⌘K) ----------
interface PalItem { kind: string; label: string; sub: string; st?: State; key?: string; run: () => void }
let palItems: PalItem[] = [];
let palHits: PalItem[] = [];
let palSel = 0;

const KIND_RANK: Record<string, number> = { Terminal: 6, Action: 4, Git: 3, Outside: 3, Project: 2 };
const fold = (s: string) => s.normalize("NFD").replace(/[\u0300-\u036f]/g, "").toLowerCase();

function palAll(withProjects: boolean): PalItem[] {
  const items: PalItem[] = [];
  const tabs = snap.tabs.filter((t) => !closing.has(t.name));
  for (const t of tabs) {
    const st = doneMarks.get(t.name) ?? t.state;
    const num = sidebarNumber(t.name);
    items.push({ kind: "Terminal", label: t.title || t.agent, sub: `${projectLabel(groupKey(t))} · ${t.title ? t.agent + " · " : ""}${LABEL[st]}`, st, key: num ? `⌘${num}` : "",
      run: () => { if (snap.tabs.some((x) => x.name === t.name) && !closing.has(t.name)) open(t.name); } }); // may have closed meanwhile
  }
  if (tabs.length) items.push({ kind: "Action", label: "Overview of all projects", sub: "who waits, what is not committed", key: "⌘0", run: () => toggleOverview(true) });
  if (tabs.some((t) => (doneMarks.get(t.name) ?? t.state) === "waiting" || doneMarks.has(t.name))) items.push({ kind: "Action", label: "Next terminal that needs you", sub: "", key: "⌘U", run: nextWaiting });
  items.push({ kind: "Action", label: "New terminal…", sub: "", key: "⌘T", run: () => { openNew().catch((e) => showOvError(`Projects did not load: ${e}`)); } });
  for (const key of new Set(tabs.map(groupKey))) {
    if (snap.git[key]) items.push({ kind: "Git", label: `Git · ${projectLabel(key)}`, sub: gitText(snap.git[key]), run: () => openGit(key) });
  }
  for (const e of snap.external) {
    if (e.unlisted || moving.has(e.session_id)) continue;
    items.push({ kind: "Outside", label: `Move into Toranj: ${e.title || projectLabel(e.cwd) || e.name}`, sub: tilde(e.cwd), st: e.state, run: () => {
      const now = snap.external.find((x) => x.session_id === e.session_id);
      if (now && !now.unlisted && !moving.has(now.session_id)) askMove([now]);
    } });
  }
  if (withProjects) {
    for (const p of projects) items.push({ kind: "Project", label: `New terminal in ${p.name}`, sub: tilde(p.path), run: () => { openNew(p).catch((e) => showOvError(`Projects did not load: ${e}`)); } });
  }
  return items;
}

async function openPalette() {
  const dlg = $<HTMLDialogElement>("pal-dlg");
  if (dlg.open) { dlg.close(); return; } // ⌘K again closes it
  if (document.querySelector("dialog[open]")) return;
  hideMenu();
  palItems = palAll(true);
  const q = $<HTMLInputElement>("pal-q");
  q.value = "";
  palSel = 0;
  drawPalette();
  $<HTMLDialogElement>("pal-dlg").showModal();
  q.focus();
  if (!projects.length) {
    try { projects = await invoke<Project[]>("list_projects"); } catch { return; }
    if (!$<HTMLDialogElement>("pal-dlg").open) return;
    palItems = palAll(true);
    drawPalette();
  }
}

function drawPalette() {
  const words = fold($<HTMLInputElement>("pal-q").value).split(/\s+/).filter(Boolean);
  if (!words.length) {
    // Without a query: everything but the long project list.
    palHits = palItems.filter((i) => i.kind !== "Project");
  } else {
    const scored: [number, number, PalItem][] = [];
    palItems.forEach((it, idx) => {
      const label = fold(it.label);
      const hay = `${label} ${fold(it.sub)} ${fold(it.kind)}`;
      if (!words.every((w) => hay.includes(w))) return;
      // Open terminals first, then actions, then the long project list.
      let score = KIND_RANK[it.kind] ?? 0;
      if (words.every((w) => label.includes(w))) score += 2;
      if (label.startsWith(words[0]) || label.includes(` ${words[0]}`)) score += 1;
      scored.push([score, idx, it]);
    });
    palHits = scored.sort((a, b) => b[0] - a[0] || a[1] - b[1]).map((s) => s[2]);
  }
  palHits = palHits.slice(0, 60);
  palSel = Math.min(palSel, Math.max(0, palHits.length - 1));
  const list = $("pal-list");
  list.replaceChildren(...palHits.map((it, i) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "pal-item" + (i === palSel ? " on" : "");
    b.setAttribute("role", "option");
    b.setAttribute("aria-selected", String(i === palSel));
    const dot = document.createElement("i");
    dot.className = it.st ? `st ${it.st}` : "pal-k";
    if (!it.st) dot.textContent = it.kind === "Git" ? "⎇" : it.kind === "Project" ? "+" : "›";
    const txt = document.createElement("span");
    txt.className = "pal-t";
    const l = document.createElement("b"); l.textContent = it.label;
    const s = document.createElement("small"); s.textContent = it.sub;
    txt.append(l, s);
    const k = document.createElement("kbd"); k.textContent = it.key ?? "";
    b.append(dot, txt, k);
    b.onmousemove = () => { if (palSel !== i) { palSel = i; markPalette(); } };
    b.onclick = () => runPalette(i);
    return b;
  }));
  if (!palHits.length) {
    const p = document.createElement("p");
    p.className = "pal-none";
    p.textContent = "Nothing matches.";
    list.append(p);
  }
}

function markPalette() {
  $("pal-list").querySelectorAll<HTMLElement>(".pal-item").forEach((b, i) => {
    b.classList.toggle("on", i === palSel);
    b.setAttribute("aria-selected", String(i === palSel));
    if (i === palSel) b.scrollIntoView({ block: "nearest" });
  });
}

function runPalette(i: number) {
  const it = palHits[i];
  if (!it) return;
  $<HTMLDialogElement>("pal-dlg").close();
  it.run();
}

$("pal-q").addEventListener("input", () => { palSel = 0; drawPalette(); });
$("pal-q").addEventListener("keydown", (e) => {
  if (e.isComposing || e.keyCode === 229) return; // IME is still choosing
  if (e.key === "ArrowDown" || (e.ctrlKey && e.key === "n")) { e.preventDefault(); palSel = Math.min(palSel + 1, palHits.length - 1); markPalette(); }
  else if (e.key === "ArrowUp" || (e.ctrlKey && e.key === "p")) { e.preventDefault(); palSel = Math.max(palSel - 1, 0); markPalette(); }
  else if (e.key === "Enter") { e.preventDefault(); runPalette(palSel); }
});
$("pal-dlg").addEventListener("click", (e) => { if (e.target === $("pal-dlg")) $<HTMLDialogElement>("pal-dlg").close(); });
$("pal-dlg").addEventListener("close", () => {
  // Back to the terminal unless the chosen item opened something else.
  if (!document.querySelector("dialog[open]") && !ovShown()) panes.get(current ?? "")?.term.focus();
});

// ---------- waiting dock ----------
// Tabs that wait for the user and aren't on screen get a small card in the
// bottom-right corner: what they ask, the prompt's choices as buttons, and
// "Open". Nothing is modal; the terminal keeps the keyboard.
interface Choice { key: string; label: string }
interface Prompt { context: string[]; choices: Choice[]; fingerprint: string }
interface Peek { lines: string[]; prompt: Prompt | null }

const peeks = new Map<string, Peek>();
/** Hidden cards, by what they showed: a new question shows the card again. */
const dismissed = new Map<string, string>();
const answering = new Set<string>();
const dockErr = new Map<string, string>();
let dockSeq = 0;

/** What a card asks: the hook's detail or the question on screen. */
function askText(t: Tab, peek?: Peek) {
  return t.detail || [...(peek?.lines ?? [])].reverse().find((l) => /\?\s*$/.test(l.trim()))?.trim() || "Waiting for your reply";
}
const cardKey = (t: Tab) => { const p = peeks.get(t.name); return p?.prompt?.fingerprint ?? askText(t, p); };

/** Waiting tabs that aren't on screen, hidden cards included: they are still
 *  read, so a new question can bring a hidden card back. */
function waitingOffscreen(): Tab[] {
  return snap.tabs.filter((t) => t.state === "waiting" && !(panes.has(t.name) && !ovShown()) && !closing.has(t.name)).slice(0, 8);
}

/** Waiting tabs that need a card. */
function dockTabs(): Tab[] {
  for (const [n, k] of [...dismissed]) {
    const t = snap.tabs.find((x) => x.name === n);
    if (t?.state !== "waiting" || cardKey(t) !== k) dismissed.delete(n);
  }
  return waitingOffscreen().filter((t) => !dismissed.has(t.name)).slice(0, 4);
}

async function updateDock() {
  const seq = ++dockSeq;
  const tabs = waitingOffscreen();
  const got = await Promise.all(tabs.map((t) => invoke<Peek>("peek_tab", { name: t.name }).catch(() => null)));
  if (seq !== dockSeq) return; // a newer update is on its way
  const now = new Set(waitingOffscreen().map((t) => t.name));
  for (const n of [...peeks.keys()]) if (!now.has(n)) { peeks.delete(n); dockErr.delete(n); }
  tabs.forEach((t, i) => {
    if (!now.has(t.name) || answering.has(t.name)) return;
    const p = got[i];
    p ? peeks.set(t.name, p) : peeks.delete(t.name);
  });
  renderDock();
}

/** Finished tabs not seen yet (errors first), for cards under the waiting ones. */
function finishedTabs(): Tab[] {
  const f = snap.tabs.filter((t) => doneMarks.has(t.name) && t.state !== "waiting" && !closing.has(t.name));
  return [...f.filter((t) => doneMarks.get(t.name) === "error"), ...f.filter((t) => doneMarks.get(t.name) !== "error")].slice(0, 6);
}

// ---------- quick open ----------
// A card that newly comes to the front of the dock listens for a few
// seconds (a bar counts down): one press of the quick key opens its
// terminal. Afterwards the card simply stays. It never takes a key meant
// for something else: a key pressed within a second of other typing goes
// to the terminal, any other key, a click elsewhere or switching apps ends
// the countdown, it listens only when the keyboard is in a terminal (or
// nowhere), and cards already there at launch don't count down.
let quick: { tab: string; start: number; ms: number; timer: number } | null = null;
const quickSeen = new Set<string>();
let quickReady = false; // cards present at launch aren't new
let lastKeyAt = Date.now();
let swallowRepeat = ""; // the quick key, held down after it opened a terminal

function disarmQuick() {
  if (!quick) return;
  clearTimeout(quick.timer);
  quick = null;
  $("wdock").querySelector(".wcard-q")?.remove();
  $("wdock").classList.remove("quick"); // shrinks back (CSS transition)
  requestAnimationFrame(measureDock);
  setTimeout(measureDock, 400);
}

/** The prompt of a waiting dock card, if it has choices to press. */
function cardPrompt(tab: string): Prompt | null {
  const t = snap.tabs.find((x) => x.name === tab);
  return t?.state === "waiting" ? peeks.get(tab)?.prompt ?? null : null;
}

/** The countdown bar, picking up where it is (`elapsed` ms in). */
function quickBar(ms: number, elapsed: number): HTMLElement {
  const bar = document.createElement("div");
  bar.className = "wcard-q";
  const prompt = quick ? cardPrompt(quick.tab) : null;
  const n = prompt?.choices.length ?? 0;
  bar.innerHTML = n
    ? `<span>Press <kbd>1</kbd>${n > 1 ? `–<kbd>${n}</kbd>` : ""} to answer · <kbd class="qk"></kbd> to open</span><i></i>`
    : `<span>Press <kbd class="qk"></kbd> to open</span><i></i>`;
  bar.querySelector(".qk")!.textContent = quickLabel();
  bar.style.setProperty("--qms", `${ms}ms`);
  bar.style.setProperty("--qdelay", `-${Math.max(0, elapsed)}ms`);
  return bar;
}

const quickLabel = () => Object.entries(QUICK_KEYS).find(([, c]) => c === settings.quickKey)?.[0].split(" ")[0] ?? "Space";

/** After every dock render: arm for a front card seen for the first time. */
function armQuick(front: HTMLElement | null, key: string) {
  for (const k of [...quickSeen]) if (!$("wdock").querySelector(`[data-qkey="${CSS.escape(k)}"]`)) quickSeen.delete(k);
  if (!front || !key || quickSeen.has(key)) return;
  quickSeen.add(key);
  front.dataset.qkey = key;
  if (!quickReady) return;
  if (!settings.quickSecs || document.querySelector("dialog[open]")) return;
  disarmQuick();
  const ms = settings.quickSecs * 1000;
  quick = { tab: front.dataset.tab!, start: Date.now(), ms, timer: window.setTimeout(disarmQuick, ms) };
  front.append(quickBar(ms, 0));
  $("wdock").classList.add("quick"); // bigger while it counts down
  requestAnimationFrame(measureDock);
  setTimeout(measureDock, 400);
}

/** Where a key may go to the dock: a terminal, or no control at all. */
function quickTarget(): boolean {
  const a = document.activeElement;
  return (!a || a === document.body || a.classList.contains("xterm-helper-textarea")) && $("ctx").hidden && !document.querySelector("dialog[open]");
}

window.addEventListener("keydown", (e) => {
  // Capture phase: before the terminal sees the key.
  if (swallowRepeat && e.code === swallowRepeat && e.repeat) { e.preventDefault(); e.stopPropagation(); return; }
  if (e.isComposing || e.keyCode === 229) { lastKeyAt = Date.now(); disarmQuick(); return; }
  // ⌥1–9: answer the front dock card's question, any time.
  if (e.altKey && !e.metaKey && !e.ctrlKey && !e.shiftKey && /^Digit[1-9]$/.test(e.code) && !e.repeat) {
    const front = ($("wdock").firstElementChild as HTMLElement | null)?.dataset.tab;
    const p = front && !$("wdock").hidden ? cardPrompt(front) : null;
    const key = e.code.slice(5);
    if (front && p?.choices.some((c) => c.key === key) && !document.querySelector("dialog[open]")) {
      e.preventDefault();
      e.stopPropagation();
      disarmQuick();
      void answer(front, key, p.fingerprint);
      return;
    }
  }
  if (!quick) { lastKeyAt = Date.now(); return; }
  const plain = !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey;
  // 1–9 while it counts down: answer right from the card.
  if (plain && !e.repeat && Date.now() - lastKeyAt >= 1000 && /^Digit[1-9]$/.test(e.code) && quickTarget()) {
    const p = cardPrompt(quick.tab);
    const key = e.code.slice(5);
    if (p?.choices.some((c) => c.key === key)) {
      e.preventDefault();
      e.stopPropagation();
      const tab = quick.tab;
      swallowRepeat = e.code;
      disarmQuick();
      void answer(tab, key, p.fingerprint);
      return;
    }
  }
  // Right after other keys it's part of what you're typing, not a request.
  const typing = Date.now() - lastKeyAt < 1000;
  if (plain && !e.repeat && !typing && e.code === settings.quickKey && quickTarget()) {
    e.preventDefault();
    e.stopPropagation();
    const tab = quick.tab;
    swallowRepeat = e.code;
    disarmQuick();
    open(tab);
    return;
  }
  if (["Shift", "Meta", "Control", "Alt"].includes(e.key)) return;
  lastKeyAt = Date.now();
  disarmQuick(); // typing something else: leave it alone
}, true);
window.addEventListener("keyup", (e) => { if (e.code === swallowRepeat) swallowRepeat = ""; }, true);
window.addEventListener("blur", disarmQuick);
window.addEventListener("pointerdown", (e) => {
  if (quick && !(e.target as HTMLElement).closest?.(`[data-tab="${CSS.escape(quick.tab)}"]`)) disarmQuick();
}, true);

function renderDock() {
  const dock = $("wdock");
  // Same order as ⌘U: waiting first, then finished ones not seen yet.
  const entries: { t: Tab; kind: "waiting" | "done" | "error" }[] = [
    ...dockTabs().map((t) => ({ t, kind: "waiting" as const })),
    ...finishedTabs().map((t) => ({ t, kind: doneMarks.get(t.name) === "error" ? "error" as const : "done" as const })),
  ];
  const keep = new Set(entries.map((e) => e.t.name));
  for (const el of [...dock.children] as HTMLElement[]) if (!el.dataset.tab || !keep.has(el.dataset.tab)) el.remove();
  entries.forEach(({ t, kind }, i) => {
    const front = i === 0;
    const peek = peeks.get(t.name);
    const prompt = kind === "waiting" ? peek?.prompt ?? null : null;
    const busy = answering.has(t.name);
    const err = dockErr.get(t.name) ?? "";
    const ask = kind === "waiting" ? askText(t, peek) : kind === "error" ? `Finished with an error${t.detail ? `: ${t.detail}` : ""}` : "Finished";
    // Rebuild a card only when what it shows changed: replacing a button
    // under the pointer between press and release drops the click (WebKit).
    const sig = JSON.stringify([kind, ask, prompt?.fingerprint ?? "", busy, err, front]);
    let card = dock.querySelector<HTMLElement>(`[data-tab="${CSS.escape(t.name)}"]`);
    if (!card) { card = document.createElement("div"); card.dataset.tab = t.name; }
    card.className = `wcard ${kind}`;
    if (dock.children[i] !== card) dock.insertBefore(card, dock.children[i] ?? null);
    if (card.dataset.sig === sig) { setText(card, ".wcard-h b", tabTitle(t)); return; }
    card.dataset.sig = sig;
    card.replaceChildren();

    const head = document.createElement("div");
    head.className = "wcard-h";
    const dot = document.createElement("i"); dot.className = `st ${kind}`;
    const title = document.createElement("b"); title.textContent = tabTitle(t);
    const more = document.createElement("span"); more.className = "wcard-more";
    const x = document.createElement("button");
    x.className = "wcard-x"; x.textContent = "×"; x.setAttribute("aria-label", "Hide");
    if (kind === "waiting") {
      x.title = "Hide until it asks something new";
      x.onclick = () => { dismissed.set(t.name, cardKey(t)); renderDock(); };
    } else {
      x.title = "Mark as seen";
      x.onclick = () => { doneMarks.delete(t.name); render(); };
    }
    head.append(dot, title, more, x);
    card.append(head);

    const p = document.createElement("p"); p.className = "wcard-d"; p.textContent = ask; p.title = ask;
    card.append(p);
    // What a "Yes" would approve: the lines above the choices (the command).
    if (prompt?.context.length) {
      const ctx = document.createElement("pre"); ctx.className = "wcard-c";
      ctx.textContent = prompt.context.join("\n");
      card.append(ctx);
    }

    const acts = document.createElement("div");
    acts.className = "wcard-a";
    for (const c of prompt?.choices ?? []) {
      const b = document.createElement("button");
      b.className = "btn" + (c.key === "1" ? " primary" : "");
      b.textContent = `${c.key}. ${c.label}`;
      if (front) { const k = document.createElement("kbd"); k.textContent = `⌥${c.key}`; b.append(k); }
      b.title = front ? `${c.label} (⌥${c.key})` : c.label;
      b.disabled = busy;
      b.onclick = () => answer(t.name, c.key, prompt!.fingerprint);
      acts.append(b);
    }
    const open_ = document.createElement("button");
    open_.className = "btn" + (kind !== "waiting" || !prompt ? " primary" : "");
    open_.innerHTML = front ? `Open <kbd>⌘U</kbd>` : "Open";
    open_.title = front ? "Go to this terminal (⌘U)" : "Go to this terminal";
    open_.disabled = busy;
    open_.onclick = () => open(t.name);
    acts.append(open_);
    card.append(acts);

    if (err) { const e = document.createElement("p"); e.className = "err"; e.textContent = err; card.append(e); }
  });
  // More than one: a stack that opens on hover or keyboard focus.
  const n = entries.length;
  dock.classList.toggle("stack", n > 1);
  const front = dock.firstElementChild as HTMLElement | null;
  if (front) setText(front, ".wcard-more", n > 1 ? `+${n - 1}` : "");
  for (const [i, el] of ([...dock.children] as HTMLElement[]).entries()) {
    el.style.setProperty("--i", String(i));
    if (i > 0) setText(el, ".wcard-more", "");
  }
  dock.hidden = n === 0;
  if (quick && !entries.some((e) => e.t.name === quick!.tab)) disarmQuick();
  // A rebuilt front card lost its bar: put it back where it was.
  if (quick && front?.dataset.tab === quick.tab && !front.querySelector(".wcard-q")) front.append(quickBar(quick.ms, Date.now() - quick.start));
  else if (quick && front?.dataset.tab !== quick.tab) disarmQuick();
  const e0 = entries[0];
  // Stable per event (the question's text arrives later and must not re-arm).
  armQuick(front, e0 ? `${e0.t.name}|${e0.kind}|${e0.t.since}` : "");
  quickReady = true;
  if (front) requestAnimationFrame(measureDock);
}

/** The peeking cards take the front card's height. */
function measureDock() {
  const front = $("wdock").firstElementChild as HTMLElement | null;
  if (front) $("wdock").style.setProperty("--fh", `${front.offsetHeight}px`);
}
window.addEventListener("resize", measureDock);

async function answer(name: string, key: string, fingerprint: string) {
  if (answering.has(name)) return;
  answering.add(name);
  dockErr.delete(name);
  renderDock();
  try {
    await invoke("answer_tab", { name, key, fingerprint });
    peeks.delete(name); // that prompt is gone; never show its buttons again
  } catch (e) {
    dockErr.set(name, String(e));
  }
  // Stay busy until the next snapshot, so the old buttons can't be clicked twice.
  try { await refresh(); } finally { answering.delete(name); }
  await updateDock();
}

// ---------- new terminal ----------
let projects: Project[] = [];
let picked: Project | null = null;

/** `pre` picks the project; then Enter opens the terminal right away. */
async function openNew(pre?: Project) {
  const dlg = $<HTMLDialogElement>("new-dlg");
  $("new-err").hidden = true;
  $<HTMLInputElement>("proj-filter").value = "";
  resumeFor = "";
  pasts = [];
  setResume("");
  if (!projects.length) projects = await invoke<Project[]>("list_projects");
  void loadDialogWorkspaces();
  if (pre) {
    const path = pre.path;
    const known = projects.find((p) => p.path === path);
    if (known) pre = known;
    else projects = [pre, ...projects];
  }
  const cur = snap.tabs.find((t) => t.name === current);
  picked = pre ?? projects.find((p) => cur && p.path === cur.cwd) ?? picked ?? projects[0] ?? null;
  drawProjects("");
  dlg.showModal();
  if (pre) {
    $("proj-list").querySelector(".proj-item.on")?.scrollIntoView({ block: "nearest" });
    $("new-ok").focus();
  } else $("proj-filter").focus();
}

/** VS Code *.code-workspace files next to the known projects: a terminal from
 * one opens in its first folder and gives the agent exactly its folders. */
let dialogWs: Project[] = [];
async function loadDialogWorkspaces() {
  let files: WsFile[];
  try {
    files = await invoke<WsFile[]>("workspace_files", { paths: projects.filter((p) => !p.count).map((p) => p.path) });
  } catch { return; }
  const next = files.map((w) => {
    const folders = [...new Set(w.folders)];
    return { name: w.name, path: w.file, count: folders.length, folders };
  }).filter((w) => w.count >= 2);
  if (JSON.stringify(next) === JSON.stringify(dialogWs)) return;
  dialogWs = next;
  if ($<HTMLDialogElement>("new-dlg").open) drawProjects($<HTMLInputElement>("proj-filter").value);
}

/** `to` relative to `from` (both absolute), for short --add-dir arguments. */
function relPath(from: string, to: string): string {
  const a = from.split("/").filter(Boolean);
  const b = to.split("/").filter(Boolean);
  let i = 0;
  while (i < a.length && i < b.length && a[i] === b[i]) i++;
  const up = a.length - i;
  if (i < 3 || up > 3) return to; // nothing useful in common: keep it absolute
  const r = [...Array(up).fill(".."), ...b.slice(i)].join("/") || ".";
  // Never a bare name that could start with "-" and read as an option.
  return r.startsWith("..") || r === "." ? r : `./${r}`;
}

/** The command for a workspace entry: claude gets every other folder as --add-dir. */
function workspaceCommand(cmd: string, folders: string[]): string {
  if (cmd.split(/\s+/)[0] !== "claude") return cmd;
  // Typed into a shell: no control characters or backslashes, ever.
  if (folders.some((f) => /[\u0000-\u001f\u007f\\]/.test(f))) return cmd;
  const [first, ...rest] = folders;
  return [cmd, ...rest.map((f) => `--add-dir ${shq(relPath(first, f))}`)].join(" ");
}

/** Folders that hold two or more projects (e.g. an app, its backend and its
 * website): a terminal there gives an agent all of them at once. */
function workspaceFolders(): Project[] {
  const by = new Map<string, number>();
  for (const p of projects) {
    if (p.count) continue;
    const parent = p.path.replace(/\/[^/]+\/?$/, "");
    const name = parent.split("/").pop() ?? "";
    if (parent.split("/").filter(Boolean).length < 3 || GENERIC_DIRS.has(name.toLowerCase())) continue;
    by.set(parent, (by.get(parent) ?? 0) + 1);
  }
  return [...by].filter(([, n]) => n >= 2).map(([path, count]) => ({ name: path.split("/").pop() ?? path, path, count }));
}

const tildePath = (p: string) => p.replace(/^\/Users\/[^/]+/, "~");

function sectionHead(title: string, hint: string): HTMLElement {
  const h = document.createElement("div");
  h.className = "proj-sec";
  h.setAttribute("role", "presentation");
  const b = document.createElement("b");
  b.textContent = title;
  const s = document.createElement("small");
  s.textContent = hint;
  h.append(b, s);
  return h;
}

function projectItem(p: Project): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.setAttribute("role", "option");
  b.className = "proj-item" + (p.count ? " wsf" : "") + (p === picked ? " on" : "");
  b.setAttribute("aria-selected", String(p === picked));
  const top = document.createElement("span");
  top.className = "pt";
  const nm = document.createElement("span");
  nm.className = "pn";
  nm.textContent = p.name;
  top.append(nm);
  if (p.count) {
    const tag = document.createElement("em");
    tag.textContent = p.folders ? "VS Code workspace" : "Folder";
    top.append(tag);
  }
  b.append(top);
  if (p.folders) {
    // What the agent will be able to read, in plain sight: relative to the
    // workspace file, so a folder outside it shows as ../ or a full path.
    const base = p.path.replace(/\/[^/]+$/, "");
    const f = document.createElement("small");
    f.className = "wsfolders";
    f.textContent = `${p.count} folders · ${[...new Set(p.folders)].map((x) => tildePath(relPath(base, x))).join(", ")}`;
    b.append(f);
    b.title = `Opens in ${p.folders[0].split("/").pop()}; claude also gets the other folders (--add-dir):\n${p.folders.map(tildePath).join("\n")}`;
  } else if (p.count) {
    const f = document.createElement("small");
    f.className = "wsfolders";
    f.textContent = `${p.count} projects · ${projects.filter((x) => !x.count && x.path.startsWith(p.path + "/")).map((x) => x.name).join(", ")}`;
    b.append(f);
    b.title = "Opens the terminal in this folder, so the agent sees every project in it";
  }
  const s = document.createElement("small");
  s.textContent = tildePath(p.path);
  b.append(s);
  b.onclick = () => { picked = p; drawProjects($<HTMLInputElement>("proj-filter").value); };
  return b;
}

function drawProjects(f: string) {
  const list = $("proj-list");
  list.replaceChildren();
  const q = f.toLowerCase();
  const match = (p: Project) => p.name.toLowerCase().includes(q) || p.path.toLowerCase().includes(q);
  const ws = [...workspaceFolders(), ...dialogWs];
  // Keep the picked entry's identity across redraws (workspace entries are rebuilt).
  if (picked?.count) picked = ws.find((p) => p.path === picked!.path) ?? picked;
  const wsHits = ws.filter(match).sort((a, b) => a.name.localeCompare(b.name) || a.path.localeCompare(b.path)).slice(0, 40);
  const projHits = projects.filter((p) => !p.count && match(p)).sort((a, b) => a.path.localeCompare(b.path)).slice(0, 80);
  // Typing picks the first project; workspaces are picked on purpose.
  const hits = [...projHits, ...wsHits];
  if (q && hits.length && !hits.includes(picked!)) picked = hits[0];
  if (wsHits.length) {
    list.append(sectionHead("Workspaces", "several projects in one terminal"));
    for (const p of wsHits) list.append(projectItem(p));
  }
  if (projHits.length) {
    list.append(sectionHead("Projects", "one repository each"));
    for (const p of projHits) list.append(projectItem(p));
  }
  if (!hits.length) {
    const e = document.createElement("p");
    e.className = "proj-none";
    e.textContent = "No project matches.";
    list.append(e);
  }
  void loadResume();
}

// ---------- continue a Claude conversation ----------
interface Past { id: string; title: string; modified: number }
let resumeFor = ""; // the folder whose conversations are listed
let pasts: Past[] = [];
let resumeId = "";

/** The folder a terminal for the picked entry starts in. */
const pickedCwd = () => (picked ? picked.folders?.[0] ?? picked.path : "");
const isClaudeCmd = () => $<HTMLInputElement>("cmd").value.trim().split(/\s+/)[0] === "claude";

async function loadResume() {
  const box = $("resume");
  const cwd = pickedCwd();
  if (!cwd || !isClaudeCmd()) { box.hidden = true; return; }
  if (cwd === resumeFor) { box.hidden = !pasts.length; return; }
  resumeFor = cwd;
  pasts = [];
  setResume("");
  box.hidden = true;
  const got = await invoke<Past[]>("recent_sessions", { cwd }).catch(() => [] as Past[]);
  if (resumeFor !== cwd) return; // another project was picked meanwhile
  pasts = got;
  drawResume();
  box.hidden = !pasts.length || !isClaudeCmd();
}

/** Picks a conversation ("" = a new one) and shows it in the command. */
function setResume(id: string) {
  resumeId = id;
  const cmd = $<HTMLInputElement>("cmd");
  const base = cmd.value.replace(/\s+--resume\s+\S+/g, "").trim();
  if (base.split(/\s+/)[0] === "claude") cmd.value = id ? `${base} --resume ${id}` : base;
  $("new-ok").textContent = id ? "Continue" : "Open";
  drawResume();
}

function drawResume() {
  const list = $("resume-list");
  list.replaceChildren();
  const row = (id: string, title: string, when: string) => {
    const b = document.createElement("button");
    b.type = "button";
    b.setAttribute("role", "option");
    b.className = "resume-item" + (id === resumeId ? " on" : "") + (id ? "" : " new");
    b.setAttribute("aria-selected", String(id === resumeId));
    const t = document.createElement("span");
    t.textContent = title;
    b.append(t);
    if (when) {
      const w = document.createElement("small");
      w.textContent = when;
      b.append(w);
    }
    b.onclick = () => setResume(id);
    list.append(b);
  };
  row("", "New conversation", "");
  for (const p of pasts) row(p.id, p.title, `${ago(Math.floor(p.modified / 1000))} ago`);
}

$("proj-filter").addEventListener("input", (e) => drawProjects((e.target as HTMLInputElement).value));
$("cmds").addEventListener("click", (e) => {
  const b = (e.target as HTMLElement).closest("button");
  if (!b) return;
  document.querySelectorAll("#cmds .chip").forEach((c) => c.classList.toggle("on", c === b));
  $<HTMLInputElement>("cmd").value = b.dataset.cmd ?? "";
  setResume("");
  void loadResume();
});
$("cmd").addEventListener("input", () => {
  // Typed by hand: follow whatever --resume the command now has.
  const m = /\s--resume\s+(\S+)/.exec($<HTMLInputElement>("cmd").value);
  resumeId = m && pasts.some((p) => p.id === m[1]) ? m[1] : "";
  $("new-ok").textContent = resumeId ? "Continue" : "Open";
  drawResume();
  void loadResume();
});
$("new-cancel").onclick = () => $<HTMLDialogElement>("new-dlg").close();
$("new-form").addEventListener("submit", async (e) => {
  e.preventDefault();
  if (!picked) { $("new-err").textContent = "Choose a project."; $("new-err").hidden = false; return; }
  try {
    const cmd = $<HTMLInputElement>("cmd").value.trim();
    const ws = picked.folders;
    const name = ws
      ? await invoke<string>("create_tab", { project: ws[0], cwd: ws[0], command: workspaceCommand(cmd, ws) })
      : await invoke<string>("create_tab", { project: picked.path, cwd: picked.path, command: cmd });
    $<HTMLDialogElement>("new-dlg").close();
    await refresh();
    open(name);
  } catch (err) {
    $("new-err").textContent = `Terminal did not open: ${err}`;
    $("new-err").hidden = false;
  }
});

// ---------- move sessions from other terminals ----------
const moving = new Set<string>(); // session ids being moved right now

function askMove(list: External[]) {
  const dlg = $<HTMLDialogElement>("move-dlg");
  const busy = list.filter((e) => !e.background && e.state !== "idle");
  // A single busy session can still be moved on purpose: Claude is stopped
  // mid-turn and the same conversation continues here with --resume.
  const force = list.length === 1 && busy.length === 1;
  const ok = force ? list : list.filter((e) => e.background || e.state === "idle");
  const err = $("move-err");
  err.hidden = true;
  const okBtn = $<HTMLButtonElement>("move-ok");
  okBtn.disabled = false;
  okBtn.hidden = ok.length === 0;
  okBtn.textContent = force ? "Interrupt and move" : "Move";
  okBtn.classList.toggle("danger", force);
  okBtn.classList.toggle("primary", !force);
  ($("move-dlg").querySelector('button[value="no"]') as HTMLButtonElement).textContent = ok.length === 0 ? "OK" : "Cancel";
  if (force) {
    $("move-title").textContent = "Agent is working";
    $("move-msg").textContent = `Claude reports that ${list[0].name || projectLabel(list[0].cwd)} is working right now. If you move it now, the current step is interrupted and the conversation continues here (claude --resume, same id). If you don’t want to interrupt the work, wait for it to finish.`;
  } else {
    $("move-title").textContent = ok.length === 1 ? "Move session to Toranj?" : `Move ${ok.length} sessions to Toranj?`;
    const names = ok.map((e) => projectLabel(e.cwd)).join(", ");
    const inTerm = ok.some((e) => !e.background);
    $("move-msg").textContent =
      `${names}. ` +
      (inTerm
        ? "Claude is closed in the terminal where it is running now (e.g. VS Code) and continues here with the same conversation and context. You can then close that terminal or VS Code window."
        : "The background session opens in a new tab and keeps running.") +
      (busy.length ? ` ${busy.length} that are working right now stay where they are.` : "");
  }
  dlg.onclose = async () => {
    if (dlg.returnValue !== "yes") return;
    dlg.returnValue = "";
    let last: string | null = null;
    const failed: string[] = [];
    const todo = ok.filter((e) => !moving.has(e.session_id));
    todo.forEach((e) => moving.add(e.session_id));
    render();
    for (const e of todo) {
      try { last = await invoke<string>("move_external", { sessionId: e.session_id, force }); }
      catch (x) { failed.push(`${projectLabel(e.cwd)}: ${x}`); }
      finally { moving.delete(e.session_id); }
    }
    await refresh();
    if (last) open(last);
    if (failed.length) {
      // Show the errors in the same dialog, with nothing left to confirm.
      err.textContent = `Not moved:\n${failed.join("\n\n")}`;
      err.hidden = false;
      okBtn.hidden = true;
      ($("move-dlg").querySelector('button[value="no"]') as HTMLButtonElement).textContent = "OK";
      dlg.onclose = null;
      dlg.returnValue = "";
      dlg.showModal();
    }
  };
  dlg.returnValue = "";
  dlg.showModal();
}

/** A Claude that never registered with `claude agents`: its conversation id
 *  is unknown, so Toranj can't resume it; explain the two manual steps. */
function explainUnlisted(e: External) {
  const dlg = $<HTMLDialogElement>("move-dlg");
  $("move-title").textContent = "This session has to be moved manually";
  $("move-msg").textContent =
    `The Claude session in ${e.cwd || "an unknown folder"} (PID ${e.pid}${e.detail ? ", " + e.detail : ""}) does not appear in the claude agents list, ` +
    "probably because it was started by an older version of Claude Code. Toranj therefore doesn’t know which conversation it holds and can’t safely continue it here.\n\n" +
    "1) In that session (e.g. in the VS Code terminal) type /exit. On exit, Claude prints the command claude --resume <id>.\n" +
    "2) In Toranj, open a new terminal (⌘T), choose that project and type that command.";
  $("move-err").hidden = true;
  $<HTMLButtonElement>("move-ok").hidden = true;
  (dlg.querySelector('button[value="no"]') as HTMLButtonElement).textContent = "OK";
  dlg.onclose = null;
  dlg.returnValue = "";
  dlg.showModal();
}

// ---------- actions ----------
$("new-btn").onclick = () => openNew();
$("pal-btn").onclick = () => openPalette();
$("empty-new").onclick = () => openNew();
$("q-wait").onclick = nextWaiting;
$("layout").addEventListener("click", (e) => {
  const b = (e.target as HTMLElement).closest<HTMLButtonElement>("button[data-layout]");
  if (b) setLayout(b.dataset.layout as Layout);
});
$("open-editor").onclick = () => {
  const cur = snap.tabs.find((t) => t.name === current);
  if (cur) invoke("open_in_editor", { path: cur.cwd }).catch((e) => console.error(e));
};
/** ⌘W, the menu item and the header button: close the selected terminal. */
function closeCurrent() {
  if (document.querySelector("dialog[open]")) return; // another dialog is up
  hideMenu();
  if (current && !ovShown()) askClose(current); // nothing to close on the overview
}
$("kill").onclick = closeCurrent;

// ---------- right-click menu for a terminal ----------
function tabMenu(e: MouseEvent, name: string) {
  e.preventDefault();
  e.stopPropagation();
  const tab = snap.tabs.find((t) => t.name === name);
  if (!tab) return;
  // The tab may have closed while the menu was open.
  const live = (run: () => void) => () => { if (snap.tabs.some((t) => t.name === name)) run(); };
  showMenu(e, [
    ["Open", live(() => open(name))],
    ["Open in VS Code", live(() => { invoke("open_in_editor", { path: tab.cwd }).catch((err) => console.error(err)); })],
    ["Close…", live(() => askClose(name)), "danger"],
  ]);
}

function hideMenu() { $("ctx").hidden = true; }

$("ctx").addEventListener("keydown", (e) => {
  const btns = [...$("ctx").querySelectorAll<HTMLButtonElement>("button")];
  const i = btns.indexOf(document.activeElement as HTMLButtonElement);
  if (e.key === "Escape") { e.preventDefault(); hideMenu(); panes.get(current ?? "")?.term.focus(); }
  else if (e.key === "ArrowDown") { e.preventDefault(); btns[(i + 1) % btns.length].focus(); }
  else if (e.key === "ArrowUp") { e.preventDefault(); btns[(i - 1 + btns.length) % btns.length].focus(); }
});
window.addEventListener("pointerdown", (e) => { if (!$("ctx").contains(e.target as Node)) hideMenu(); }, true);
window.addEventListener("blur", hideMenu);
window.addEventListener("resize", hideMenu);
$("tabs").parentElement!.addEventListener("scroll", hideMenu);

/** Asks before closing one terminal; the other panes stay open. */
function askClose(name: string) {
  const cur = snap.tabs.find((t) => t.name === name);
  if (!cur || closing.has(name) || $<HTMLDialogElement>("kill-dlg").open) return;
  const num = sidebarNumber(cur.name);
  const tag = `${cur.agent}${num ? ` ⌘${num}` : ""}`;
  const which = `${cur.title ? `“${cur.title}” (${tag})` : tag} in ${projectLabel(cur.project || cur.cwd)}`;
  // A `claude attach` tab is only a window into a background session:
  // closing it alone leaves the session running (and listed outside Toranj).
  const bg = cur.background;
  $("kill-msg").textContent = bg
    ? `${which} shows a background Claude session. If you close only the view, the session keeps running and returns under “Outside Toranj”. If you stop it, it ends, and the conversation stays saved (claude --resume). For a session that has already stopped, Claude removes it from the list and deletes its worktree, but only if the worktree has no unsaved changes.`
    : cur.state === "running"
      ? `${which} is still working. Closing it ends that process.`
      : `This closes the terminal ${which}.`;
  $("kill-ok").textContent = bg ? "Close view only" : "Close terminal";
  $("kill-ok").className = bg ? "btn" : "btn danger";
  $("kill-stop").hidden = !bg;
  $("kill-ok").hidden = false;
  const dlg = $<HTMLDialogElement>("kill-dlg");
  dlg.returnValue = "";
  dlg.onclose = async () => {
    const stop = dlg.returnValue === "stop";
    if (dlg.returnValue !== "yes" && !stop) { panes.get(current ?? "")?.term.focus(); return; }
    // Hide the tab and move focus first, so a refresh during kill_tab does
    // not attach to it again. Stopping a background session can fail, so
    // that tab keeps its pane until it is really gone.
    closing.add(cur.name);
    if (!stop) {
      if (current === cur.name) current = [...panes.keys()].find((n) => n !== cur.name) ?? null;
      panes.get(cur.name)?.dispose();
      panes.delete(cur.name);
    }
    let failed = "";
    try { await invoke("kill_tab", { name: cur.name, stopSession: stop }); }
    catch (e) { failed = String(e); }
    finally {
      await refresh();
      closing.delete(cur.name);
      if (current) open(current); else render();
      if (failed) showCloseError(failed);
    }
  };
  dlg.showModal();
}

/** Asks before closing several terminals at once (a project's or a group's). */
function askCloseAll(names: string[], where: string) {
  const tabs = snap.tabs.filter((t) => names.includes(t.name) && !closing.has(t.name));
  if (!tabs.length || $<HTMLDialogElement>("kill-dlg").open) return;
  if (tabs.length === 1) { askClose(tabs[0].name); return; }
  const busy = tabs.filter((t) => t.state === "running").length;
  const bg = tabs.filter((t) => t.background).length;
  $("kill-msg").textContent = [
    `This closes ${tabs.length} terminals in ${where}: ${tabs.map((t) => t.title || t.agent).join(", ")}.`,
    busy ? (busy === 1 ? "One of them is still working; closing ends it." : `${busy} of them are still working; closing ends them.`) : "",
    bg ? "Background Claude sessions keep running and return under “Outside Toranj”." : "",
  ].filter(Boolean).join(" ");
  $("kill-ok").textContent = `Close ${tabs.length} terminals`;
  $("kill-ok").className = "btn danger";
  $("kill-ok").hidden = false;
  $("kill-stop").hidden = true;
  const dlg = $<HTMLDialogElement>("kill-dlg");
  dlg.returnValue = "";
  dlg.onclose = async () => {
    if (dlg.returnValue !== "yes") { panes.get(current ?? "")?.term.focus(); return; }
    for (const t of tabs) {
      closing.add(t.name);
      panes.get(t.name)?.dispose();
      panes.delete(t.name);
    }
    if (current && tabs.some((t) => t.name === current)) current = [...panes.keys()][0] ?? null;
    const failed: string[] = [];
    await Promise.all(tabs.map((t) => invoke("kill_tab", { name: t.name, stopSession: false }).catch((e) => { failed.push(String(e)); })));
    await refresh();
    for (const t of tabs) closing.delete(t.name);
    if (current) open(current); else render();
    if (failed.length) showCloseError(failed[0]);
  };
  dlg.showModal();
}

/** Shows why closing failed. Its own dialog: by the time a slow stop fails,
 *  the close dialog may already be open again for another tab. */
function showCloseError(msg: string) {
  const dlg = $<HTMLDialogElement>("err-dlg");
  $("err-msg").textContent = `Not closed: ${msg}`;
  if (!dlg.open) dlg.showModal();
}

window.addEventListener("keydown", (e) => {
  if (!e.metaKey) return;
  if (e.key === "t") { e.preventDefault(); openNew(); }
  else if (e.key === "k") { e.preventDefault(); openPalette(); }
  else if ((e.key === "=" || e.key === "+" || e.key === "-") && document.querySelector("dialog[open]:not(#set-dlg)")) return;
  else if (e.key === "=" || e.key === "+") { e.preventDefault(); setFont(settings.fontSize + 1); }
  else if (e.key === "-") { e.preventDefault(); setFont(settings.fontSize - 1); }
  else if (e.key === ",") { e.preventDefault(); openSettings(); }
  else if (e.key === "0") { e.preventDefault(); if (!document.querySelector("dialog[open]")) toggleOverview(); }
  else if (e.key === "u") { e.preventDefault(); nextWaiting(); }
  // By position too: on a Croatian keyboard [ and ] are the Š and Đ keys.
  else if ((e.code === "BracketLeft" || e.key === "[") && !e.shiftKey) { e.preventDefault(); goBack(); }
  else if ((e.code === "BracketRight" || e.key === "]") && !e.shiftKey) { e.preventDefault(); goForward(); }
  else if (e.key === "g") { e.preventDefault(); if (!ovShown()) toggleLayout(); }
  // macOS: the app menu handles ⌘W (see toranj://close-tab); this covers
  // platforms without that menu.
  else if (e.key === "w") { e.preventDefault(); closeCurrent(); }
  else if (/^[1-9]$/.test(e.key)) {
    e.preventDefault();
    const order = [...document.querySelectorAll<HTMLButtonElement>("#tabs .sess:not(.ext)")];
    order[Number(e.key) - 1]?.click();
  }
});

// ---------- resources ----------
interface Proc { pid: number; label: string; cwd: string; bytes: number; count: number; killable: boolean }
interface Group { name: string; bytes: number; count: number; procs: Proc[] }
interface Resources { total: number; used: number; swap_total: number; swap_used: number; groups: Group[]; others: Proc[] }
interface Tip { weight: number; level: "crit" | "hot" | ""; html: string; action?: { label: string; confirm: string; run: () => Promise<unknown> } }

const GB = 2 ** 30;
const MB = 2 ** 20;
const gb = (b: number) => (b / GB).toLocaleString("en-US", { maximumFractionDigits: 1, minimumFractionDigits: 1 });
const size = (b: number) => (b >= GB ? `${gb(b)} GB` : `${Math.round(b / MB)} MB`);
/** "1 process", "2 processes". */
const plural = (n: number, one: string, other: string) => `${n} ${n === 1 ? one : other}`;
const procs = (n: number) => plural(n, "process", "processes");

let lastRes: Resources | null = null;
const resOpen = new Set<string>();

function bar(frac: number) {
  const pct = Math.max(0, Math.min(1, frac));
  const cls = pct > 0.9 ? "crit" : pct > 0.5 ? "hot" : "";
  return `<span class="bar"><i class="${cls}" style="width:${(pct * 100).toFixed(0)}%"></i></span>`;
}

/** Hover text for a footer chip: its biggest processes. */
function chipTitle(g: Group) {
  const top = g.procs.slice(0, 4).map((p) => `${p.label}${p.cwd ? ` (${p.cwd.split("/").pop()})` : ""}: ${size(p.bytes)}`);
  return `${g.name}: ${procs(g.count)}\n${top.join("\n")}\nClick for details and tips.`;
}

async function loadResources() {
  if (document.visibilityState !== "visible") return;
  const el = $("res");
  let r: Resources | null = null;
  try { r = await invoke<Resources>("resources"); } catch { /* shown as stale below */ }
  if (!r) { el.classList.add("stale"); el.title = "Memory data is not available right now"; return; }
  lastRes = r;
  el.classList.remove("stale");
  el.hidden = false;
  el.title = "";
  const parts = [`<span title="Click for details and tips">RAM <b>${gb(r.used)} / ${gb(r.total)} GB</b>${bar(r.used / r.total)}</span>`];
  // macOS grows swap on demand, so used/total is always near 100%. Measure
  // swap against RAM instead: half of RAM is a lot, more than RAM is critical.
  if (r.swap_used > 0) parts.push(`<span title="Memory written to disk. The bigger it is, the slower your Mac.">Swap <b>${gb(r.swap_used)} GB</b>${bar(r.swap_used / r.total)}</span>`);
  // Only the biggest groups; the panel lists everything.
  for (const g of r.groups.filter((g) => g.bytes >= GB).slice(0, 3)) {
    parts.push(`<span title="${escapeHtml(chipTitle(g))}">${escapeHtml(g.name)} <b>${gb(g.bytes)} GB</b></span>`);
  }
  const tips = buildTips(r).filter((t) => t.level);
  if (tips.length) parts.push(`<span class="res-tip ${tips.some((t) => t.level === "crit") ? "crit" : ""}">${plural(tips.length, "tip", "tips")}</span>`);
  $("res-sum").innerHTML = parts.join("");
  if (!$("res-panel").hidden && !resHeld()) renderResPanel();
}

/** Advice on what to close, biggest win first. */
function buildTips(r: Resources): Tip[] {
  const tips: Tip[] = [];
  const g = (name: string) => r.groups.find((x) => x.name === name);
  const swapFrac = r.swap_used / r.total;
  if (swapFrac >= 0.25) {
    tips.push({
      weight: Infinity, level: swapFrac >= 0.5 ? "crit" : "hot",
      html: `Swap is <b>${gb(r.swap_used)} GB</b>: your Mac is moving memory to disk, so everything runs slower. Close some of the items below until swap starts to drop.`,
    });
  }
  const sim = g("iOS simulator");
  if (sim && sim.bytes >= GB) {
    tips.push({
      weight: sim.bytes, level: "hot",
      html: `<b>iOS simulator</b> uses ${size(sim.bytes)} (${procs(sim.count)}). Shut it down if you are not testing an app right now. It starts again with <code>flutter run</code> or from Xcode.`,
      action: { label: "Shut down all simulators", confirm: "All running iOS simulators will shut down, along with the apps running in them.", run: () => invoke("shutdown_simulators") },
    });
  }
  const dock = g("Docker/VM");
  if (dock && dock.bytes >= GB) {
    tips.push({
      weight: dock.bytes, level: "hot",
      html: `<b>Docker/VM</b> holds ${size(dock.bytes)}. Stop containers you don’t need (<code>docker compose down</code> in the project), and quit OrbStack or Docker Desktop (⌘Q) when you are not using them.`,
    });
  }
  const ag = g("AI agents");
  if (ag && ag.bytes >= GB) {
    // Count sessions from what Toranj shows, not processes: background
    // sessions bring helpers (daemon, terminals) that aren't agents.
    const AGENT_NAMES = ["claude", "codex", "gemini", "aider", "opencode", "cursor-agent", "amp"];
    const inTabs = snap.tabs.filter((t) => AGENT_NAMES.includes(t.agent)).length;
    const outside = snap.external.length;
    const idleTabs = snap.tabs.filter((t) => AGENT_NAMES.includes(t.agent) && (t.state === "idle" || t.state === "done"));
    const idleNames = idleTabs.map((t) => { const n = sidebarNumber(t.name); return n ? `⌘${n}` : projectLabel(t.project || t.cwd); });
    const per = new Map<string, number>();
    for (const p of ag.procs) if (p.cwd && AGENT_NAMES.includes(p.label)) per.set(p.cwd, (per.get(p.cwd) ?? 0) + 1);
    const dup = [...per].filter(([, n]) => n > 1).sort((a, b) => b[1] - a[1])[0];
    tips.push({
      weight: ag.bytes, level: ag.bytes >= 4 * GB ? "hot" : "",
      html: `<b>AI agents</b> use ${size(ag.bytes)} (${procs(ag.count)})`
        + (inTabs || outside
          ? `: ${plural(inTabs, "session", "sessions")} in Toranj` + (outside ? ` and ${plural(outside, "session", "sessions")} outside it` : "")
            + `, and the rest are helper and spare processes (e.g. the Claude daemon for background sessions).`
          : `, all of them helper and spare processes.`)
        + ` The list is under “AI agents” below.`
        + (idleTabs.length ? ` Close idle sessions (${escapeHtml(idleNames.slice(0, 6).join(", ") + (idleNames.length > 6 ? ` and ${idleNames.length - 6} more` : ""))}) if you are not using them.` : "")
        + (dup ? ` <code>${escapeHtml(dup[0].split("/").pop()!)}</code> has ${plural(dup[1], "agent", "agents")} running at once.` : ""),
    });
  }
  const vsc = g("VS Code");
  if (vsc && vsc.bytes >= 2 * GB) {
    const ext = vsc.procs.map((p) => /\/extensions\/(?:[^/.]+\.)?([^/]+?)-\d/.exec(p.cwd)?.[1]).find(Boolean);
    tips.push({
      weight: vsc.bytes, level: vsc.bytes >= 4 * GB ? "hot" : "",
      html: `<b>VS Code</b> uses ${size(vsc.bytes)}. Close windows of projects you are not working on.`
        + (ext ? ` The biggest extension is <code>${escapeHtml(ext)}</code>; disable it in projects where you don’t need it (Extensions → Disable (Workspace)).` : ""),
    });
  }
  const ls = g("Language servers");
  if (ls && ls.bytes >= GB) {
    tips.push({
      weight: ls.bytes, level: "",
      html: `<b>Language servers</b> hold ${size(ls.bytes)}. They are safe to stop: the editor restarts them when you need them, usually with less memory.`,
      action: { label: "Stop language servers", confirm: `This will stop ${plural(ls.procs.filter((p) => p.killable).length, "language server", "language servers")}. The editor restarts them when needed.`, run: () => stopAll(ls) },
    });
  }
  const gr = g("Gradle/Kotlin");
  if (gr && gr.bytes >= 512 * MB) {
    tips.push({
      weight: gr.bytes, level: "hot",
      html: `<b>Gradle/Kotlin daemons</b> hold ${size(gr.bytes)} even after a build. The next build starts them again.`,
      action: { label: "Stop daemons", confirm: `This will stop ${plural(gr.procs.filter((p) => p.killable).length, "Gradle/Kotlin daemon", "Gradle/Kotlin daemons")}. A build that is running right now will fail, so do this only after the build finishes.`, run: () => stopAll(gr) },
    });
  }
  const emu = g("Android emulator");
  if (emu && emu.bytes >= GB) {
    tips.push({ weight: emu.bytes, level: "hot", html: `<b>Android emulator</b> uses ${size(emu.bytes)}. Close it if you are not using it.` });
  }
  const br = g("Browsers");
  if (br && br.bytes >= 2 * GB) {
    tips.push({
      weight: br.bytes, level: "",
      html: `<b>Browsers</b> use ${size(br.bytes)}. Close tabs you don’t need; in Chrome and Brave, <code>Shift+Esc</code> shows which tab uses the most.`,
    });
  }
  for (const p of r.others.filter((p) => p.bytes >= GB && p.count > 1)) {
    tips.push({ weight: p.bytes, level: "", html: `<b>${escapeHtml(p.label)}</b> uses ${size(p.bytes)}. Quit it (⌘Q) if you are not using it right now.` });
  }
  return tips.sort((a, b) => b.weight - a.weight);
}

async function stopAll(g: Group) {
  const errs: string[] = [];
  for (const p of g.procs.filter((p) => p.killable)) {
    try { await invoke("stop_process", { pid: p.pid }); } catch (e) { errs.push(`${p.label}: ${e}`); }
  }
  if (errs.length) throw errs.join("\n");
}

function procRow(p: Proc) {
  const name = p.count > 1 ? `${escapeHtml(p.label)} <small>· ${procs(p.count)}</small>` : escapeHtml(p.label);
  const btn = p.killable ? `<button class="link danger" data-stop="${p.pid}" data-label="${escapeHtml(p.label)}">Close</button>` : "";
  return `<div class="res-row"><span class="res-n" title="PID ${p.pid}">${name}${p.cwd ? `<small class="res-cwd">${escapeHtml(p.cwd)}</small>` : ""}</span><b>${size(p.bytes)}</b><span class="res-act">${btn}</span></div>`;
}

function renderResPanel() {
  const r = lastRes;
  const body = $("res-body");
  if (!r) { body.innerHTML = `<p class="dlg-p">Memory data is not available right now.</p>`; return; }
  const scroll = body.scrollTop;
  const tips = buildTips(r);
  const free = Math.max(0, r.total - r.used);
  let html = `<div class="res-top">
    <div><small>RAM</small><b>${gb(r.used)} / ${gb(r.total)} GB</b>${bar(r.used / r.total)}<small>${size(free)} free</small></div>
    <div><small>Swap</small><b>${gb(r.swap_used)} GB</b>${bar(r.swap_used / r.total)}<small>${r.swap_used / r.total >= 0.5 ? "full, your Mac is slowing down" : r.swap_used / r.total >= 0.25 ? "elevated" : "OK"}</small></div>
  </div>`;
  html += `<h4>Tips</h4>`;
  html += tips.length
    ? `<ul class="res-tips">${tips.slice(0, 7).map((t, i) => `<li class="${t.level}"><span>${t.html}</span>${t.action ? `<button class="btn" data-tip="${i}">${escapeHtml(t.action.label)}</button>` : ""}</li>`).join("")}</ul>`
    : `<p class="dlg-p">There is enough memory; nothing needs to be closed.</p>`;
  html += `<h4>By group</h4>`;
  for (const g of r.groups) {
    const more = g.count > g.procs.length ? `<p class="res-more">and ${g.count - g.procs.length} smaller</p>` : "";
    html += `<details data-g="${escapeHtml(g.name)}"${resOpen.has(g.name) ? " open" : ""}><summary><span>${escapeHtml(g.name)} <small>· ${procs(g.count)}</small></span><b>${size(g.bytes)}</b></summary>${g.procs.map(procRow).join("")}${more}</details>`;
  }
  if (r.others.length) {
    html += `<details data-g="__others"${resOpen.has("__others") ? " open" : ""}><summary><span>Other apps <small>· over ${size(300 * MB)}</small></span><b>${size(r.others.reduce((a, p) => a + p.bytes, 0))}</b></summary>${r.others.map(procRow).join("")}</details>`;
  }
  html += `<p class="res-note">Process memory includes the compressed and swapped part (like Activity Monitor), so the total can exceed RAM. Closing sends the process a polite request to exit (SIGTERM).</p>`;
  body.innerHTML = html;
  body.scrollTop = scroll;
  body.querySelectorAll<HTMLDetailsElement>("details").forEach((d) => d.addEventListener("toggle", () => {
    d.open ? resOpen.add(d.dataset.g!) : resOpen.delete(d.dataset.g!);
  }));
  body.querySelectorAll<HTMLButtonElement>("[data-tip]").forEach((b) => b.addEventListener("click", () => {
    const a = tips[Number(b.dataset.tip)]?.action;
    if (a) confirmRun(a.label, a.confirm, a.run);
  }));
  body.querySelectorAll<HTMLButtonElement>("[data-stop]").forEach((b) => b.addEventListener("click", () => confirmStop(Number(b.dataset.stop), b.dataset.label!)));
}

/** Panel actions in flight; the panel is not rebuilt under them. */
let resBusy = 0;
let resPressed = false;
/** Rebuilding the panel under the pointer drops the click (WebKit), and it
 *  would also wipe a text selection; wait for the next tick instead. */
function resHeld() {
  const sel = document.getSelection();
  const selecting = !!sel && !sel.isCollapsed && $("res-panel").contains(sel.anchorNode);
  return resPressed || resBusy > 0 || selecting;
}
$("res-panel").addEventListener("pointerdown", () => { resPressed = true; });
document.addEventListener("pointerup", () => { resPressed = false; });

function showResErr(e: unknown) {
  $("res-err").textContent = e ? String(e) : "";
  $("res-err").hidden = !e;
}

function confirmRun(title: string, msg: string, run: () => Promise<unknown>, ok = title) {
  $("stop-title").textContent = `${title}?`;
  $("stop-ok").textContent = ok;
  $("stop-msg").textContent = msg;
  const dlg = $<HTMLDialogElement>("stop-dlg");
  dlg.onclose = async () => {
    if (dlg.returnValue !== "yes") return;
    resBusy++;
    try { await run(); showResErr(null); } catch (e) { showResErr(e); } finally { resBusy--; }
    setTimeout(loadResources, 1000);
  };
  dlg.showModal();
}

function confirmStop(pid: number, label: string) {
  const agent = lastRes?.groups.find((g) => g.name === "AI agents")?.procs.some((p) => p.pid === pid);
  const resume = label === "claude" ? " The conversation stays saved and you can continue it with claude --resume." : "";
  confirmRun("Close process", agent
    ? `Agent “${label}” (PID ${pid}) will shut down and interrupt the work it is doing.${resume}`
    : `Process “${label}” (PID ${pid}) will be asked to shut down gracefully.`,
    () => invoke("stop_process", { pid }), "Close");
}

function toggleResPanel(show = $("res-panel").hidden) {
  $("res-panel").hidden = !show;
  $("res").setAttribute("aria-expanded", String(show));
  if (show) { $("res-err").hidden = true; renderResPanel(); loadResources(); }
}
$("res-sum").addEventListener("click", () => toggleResPanel());
$("res-sum").addEventListener("keydown", (e) => { if (e.key === "Enter" || e.key === " ") { e.preventDefault(); toggleResPanel(); } });
$("res-close").addEventListener("click", () => toggleResPanel(false));
// Escape belongs to the terminal unless the panel has focus.
document.addEventListener("keydown", (e) => {
  if (e.key !== "Escape" || $("res-panel").hidden || document.querySelector("dialog[open]")) return;
  const f = document.activeElement;
  if (f === document.body || $("res").contains(f)) toggleResPanel(false);
});
document.addEventListener("mousedown", (e) => {
  if ($("res-panel").hidden || document.querySelector("dialog[open]")) return;
  if (!$("res").contains(e.target as Node)) toggleResPanel(false);
});

// ---------- git panel ----------
interface FileChange { code: string; path: string }
interface Commit { hash: string; time: number; author: string; subject: string; pushed: boolean }
interface Worktree { path: string; branch: string; head: string; current: boolean; locked: boolean; prunable: boolean; changed: number | null; unpushed: number | null; merged: boolean }
interface Branch { name: string; upstream: string; gone: boolean; ahead: number; behind: number; unpushed: number | null; merged: boolean; time: number; worktree: string }
interface Stash { name: string; time: number; subject: string }
interface GitDetails {
  root: string; branch: string; upstream: string; ahead: number; behind: number; base: string; base_local: string; has_remote: boolean;
  status_skipped: boolean; skip_reason: string; unpushed: number | null; push_known: boolean; branches_truncated: boolean; changed: number; files: FileChange[]; commits: Commit[]; worktrees: Worktree[];
  branches: Branch[]; stashes: Stash[]; last_fetch: number | null;
}

let gitPath = "";
let gitData: GitDetails | null = null;
let gitSeq = 0;

const commitsWord = (n: number) => plural(n, "commit", "commits");
const uncommitted = (n: number) => plural(n, "uncommitted change", "uncommitted changes");
const unpushedWord = (n: number) => plural(n, "unpushed commit", "unpushed commits");
const filesWord = (n: number) => plural(n, "change", "changes");
const tilde = (p: string) => p.replace(/^\/Users\/[^/]+/, "~");
const shq = (s: string) => `'${s.replace(/'/g, `'\\''`)}'`;
const cmd = (c: string) => `<code>${escapeHtml(c)}</code>`;

function openGit(path: string) {
  if (!path) return;
  gitData = null;
  gitPath = path;
  $("git-title").textContent = `Git · ${projectLabel(path)}`;
  $("git-err").hidden = true;
  renderGit();
  const dlg = $<HTMLDialogElement>("git-dlg");
  if (!dlg.open) dlg.showModal();
  loadGit();
}

async function loadGit() {
  const seq = ++gitSeq;
  const path = gitPath;
  $("git-reload").textContent = "Loading…";
  invoke("refresh_git", { path });
  try {
    const d = await invoke<GitDetails>("git_details", { path });
    if (seq !== gitSeq) return;
    gitData = d;
    $("git-err").hidden = true;
  } catch (e) {
    if (seq !== gitSeq) return;
    gitData = null;
    $("git-err").textContent = `Could not load git status: ${e}`;
    $("git-err").hidden = false;
  } finally {
    if (seq === gitSeq) $("git-reload").textContent = "Refresh";
  }
  renderGit();
}

/** Short label for a worktree: relative to the main checkout when inside it. */
function wtLabel(d: GitDetails, w: Worktree) {
  const main = d.worktrees[0]?.path ?? d.root;
  if (w.path === main) return "main checkout";
  return w.path.startsWith(main + "/") ? w.path.slice(main.length + 1) : tilde(w.path);
}

function fileCode(code: string) {
  if (code === "??") return { ch: "+", cls: "new", title: "new file (not in git)" };
  if (code.includes("U") || code === "AA" || code === "DD") return { ch: "!", cls: "conf", title: "conflict" };
  const c = code[0] !== "." ? code[0] : code[1];
  const what: Record<string, string> = { M: "modified", A: "added", D: "deleted", R: "renamed", C: "copied", T: "type changed" };
  const staged = code[0] !== "." ? (code[1] !== "." ? ", partly staged" : ", staged") : "";
  return { ch: c, cls: c === "D" ? "del" : c === "A" ? "new" : "", title: (what[c] ?? code) + staged };
}

interface GitNote { level: "ok" | "info" | "hot" | "crit"; html: string }

/** Why "all clean" can't be claimed, or [] when every check could run. */
function gitUnknowns(d: GitDetails): string[] {
  const u: string[] = [];
  if (d.status_skipped) u.push("changes in this folder");
  const blind = d.worktrees.filter((w) => !w.current && !w.prunable && w.changed === null);
  if (blind.length) u.push(`changes in ${plural(blind.length, "worktree", "worktrees")}`);
  if (!d.has_remote) u.push("what has been pushed (the repo has no remote)");
  else if (!d.push_known || d.unpushed === null || d.worktrees.some((w) => !w.prunable && w.unpushed === null)) u.push("what has been pushed");
  else if (d.branches.some((b) => b.unpushed === null)) u.push("what has been pushed on some branches");
  return u;
}

/** What needs attention, most important first. Empty list means all clean. */
function gitNotes(d: GitDetails): GitNote[] {
  const notes: GitNote[] = [];
  const baseLocal = d.base_local;
  const byName = new Map(d.branches.map((b) => [b.name, b]));
  const cur = byName.get(d.branch);
  const conflicts = d.files.filter((f) => fileCode(f.code).cls === "conf").length;

  if (conflicts) notes.push({ level: "crit", html: `<b>${plural(conflicts, "file", "files")} in conflict</b> on branch ${cmd(d.branch)}. Resolve the conflicts before committing.` });
  if (d.status_skipped) notes.push({ level: "info", html: d.skip_reason === "filters"
    ? `The repo has its own git filters, so for safety reasons Toranj does not read file changes. Check them in a terminal with ${cmd("git status")}.`
    : `${cmd("git status")} failed or took too long, so file changes are unknown. Try Refresh or run it in a terminal.` });
  if (d.changed) notes.push({ level: "hot", html: `<b>${uncommitted(d.changed)}</b> on branch ${cmd(d.branch)}.` });
  if (d.unpushed) {
    const where = d.branch === "detached" ? `on a detached HEAD (no branch, easily lost on checkout)` : `on branch ${cmd(d.branch)}${cur?.upstream ? "" : " (the branch has no upstream)"}`;
    notes.push({ level: "hot", html: `<b>${unpushedWord(d.unpushed)}</b> ${where}.` });
  } else if (d.has_remote && d.branch !== "detached" && cur && !cur.upstream) {
    notes.push({ level: "info", html: `Branch ${cmd(d.branch)} has no upstream on the remote.` });
  }
  if (d.behind) notes.push({ level: "info", html: `${cmd(d.branch)} is <b>${commitsWord(d.behind)}</b> behind ${cmd(d.upstream)}. Pull with ${cmd("git pull")}.` });
  if (cur?.gone) notes.push({ level: "info", html: `The remote branch of ${cmd(d.branch)} was deleted (probably after a merge).` });

  // Other worktrees.
  for (const w of d.worktrees) {
    if (w.current) continue;
    const name = cmd(wtLabel(d, w));
    if (w.prunable) { notes.push({ level: "info", html: `Worktree ${name} no longer exists on disk. Clean it up with ${cmd("git worktree prune")}.` }); continue; }
    const bits: string[] = [];
    if (w.changed) bits.push(`<b>${uncommitted(w.changed)}</b>`);
    if (w.unpushed) bits.push(`<b>${unpushedWord(w.unpushed)}</b>`);
    if (bits.length) {
      notes.push({ level: "hot", html: `Worktree ${name} (${w.branch ? cmd(w.branch) : "detached HEAD"}): ${bits.join(", ")}.` });
    } else if (w.changed === 0 && w.unpushed === 0 && w.merged && w.branch !== baseLocal && !w.locked) {
      const used = tabsIn(w.path);
      notes.push({ level: "info", html: `Worktree ${name} is clean and already merged into ${cmd(d.base)}, so it can be removed${used ? ` (it is open in Toranj: ${escapeHtml(used)})` : ""}. Removing it also deletes ignored files in it (e.g. ${cmd(".env")}, build output): ${cmd(`git worktree remove ${shq(w.path)}`)}` });
    }
  }

  // Branches without a worktree.
  const free = d.branches.filter((b) => !b.worktree && b.name !== baseLocal);
  for (const b of free) {
    if (b.unpushed && !b.merged) notes.push({ level: "hot", html: `Branch ${cmd(b.name)} has <b>${unpushedWord(b.unpushed)}</b> and is not merged into ${cmd(d.base || "the main branch")}.` });
  }
  const merged = free.filter((b) => b.merged && b.unpushed === 0);
  if (merged.length) {
    const names = merged.slice(0, 5).map((b) => cmd(b.name)).join(", ") + (merged.length > 5 ? ` and ${merged.length - 5} more` : "");
    notes.push({ level: "info", html: `Already merged into ${cmd(d.base)} and safe to delete (${plural(merged.length, "branch", "branches")}): ${names}. Delete with ${cmd("git branch -d <name>")}.` });
  }
  if (d.stashes.length) notes.push({ level: "info", html: `${plural(d.stashes.length, "stash", "stashes")} saved (${cmd("git stash list")}). There may be forgotten work in there.` });

  if (d.has_remote) {
    const age = d.last_fetch ? Date.now() / 1000 - d.last_fetch : Infinity;
    if (age > 86400) notes.push({ level: "info", html: d.last_fetch
      ? `The last fetch was ${ago(d.last_fetch)} ago, so the remote state (pushed, behind, merged) may be out of date. Refresh with ${cmd("git fetch --prune")}.`
      : `The repo has never been fetched, so the remote state is unknown. Refresh with ${cmd("git fetch --prune")}.` });
  } else {
    notes.push({ level: "info", html: `The repo has no remote, so none of this has been pushed anywhere.` });
  }
  return notes;
}

/** Toranj tabs working inside `path`, as "⌘N" labels. */
function tabsIn(path: string) {
  return snap.tabs
    .filter((t) => [t.project, t.cwd].some((p) => p === path || p.startsWith(path + "/")))
    .map((t) => { const n = sidebarNumber(t.name); return n ? `⌘${n}` : t.agent; })
    .join(", ");
}

function renderGit() {
  const body = $("git-body");
  const d = gitData;
  if (!d) { body.innerHTML = `<p class="dlg-p">${$("git-err").hidden ? "Loading…" : "No data."}</p>`; return; }
  const scroll = body.scrollTop;
  const notes = gitNotes(d);
  const serious = notes.some((n) => n.level === "hot" || n.level === "crit");

  const track = [d.upstream ? `tracks <b>${escapeHtml(d.upstream)}</b>` : "no upstream"];
  if (d.ahead) track.push(`↑${d.ahead}`);
  if (d.behind) track.push(`↓${d.behind}`);
  let html = `<p class="git-sum">⎇ <b>${escapeHtml(d.branch)}</b> · ${track.join(" ")} · ${escapeHtml(tilde(d.root))}</p>`;

  html += `<h4>Status</h4><ul class="res-tips">`;
  const unknown = gitUnknowns(d);
  if (!serious && !unknown.length) html += `<li class="ok"><span><b>All clean.</b> No uncommitted changes or unpushed commits${d.worktrees.length > 1 ? ", including in other worktrees" : ""}.</span></li>`;
  else if (!serious) html += `<li class="hot"><span><b>Can’t confirm that everything is clean.</b> Unknown: ${unknown.join(", ")}.</span></li>`;
  html += notes.map((n) => `<li class="${n.level === "info" ? "" : n.level}"><span>${n.html}</span></li>`).join("");
  html += `</ul>`;

  if (!d.status_skipped) {
    html += `<h4>Changes on ${escapeHtml(d.branch)} (${d.changed})</h4>`;
    if (!d.files.length) html += `<p class="git-more">No uncommitted changes.</p>`;
    for (const f of d.files) {
      const c = fileCode(f.code);
      html += `<div class="git-file" title="${escapeHtml(c.title)}"><span class="git-code ${c.cls}">${escapeHtml(c.ch)}</span><span>‎${escapeHtml(f.path)}</span></div>`;
    }
    if (d.changed > d.files.length) html += `<p class="git-more">and ${d.changed - d.files.length} more</p>`;
  }

  const byName = new Map(d.branches.map((b) => [b.name, b]));
  html += `<h4>Worktrees (${d.worktrees.length})</h4>`;
  d.worktrees.forEach((w, i) => {
    const b = byName.get(w.branch);
    const tags: string[] = [];
    if (w.current) tags.push(`<span class="tag">this one</span>`);
    if (w.prunable) tags.push(`<span class="tag bad">folder missing</span>`);
    else if (w.changed === null) tags.push(`<span class="tag">changes unknown</span>`);
    else tags.push(w.changed ? `<span class="tag warn">${filesWord(w.changed)}</span>` : `<span class="tag ok">clean</span>`);
    if (w.unpushed) tags.push(`<span class="tag warn">↑${w.unpushed} unpushed</span>`);
    else if (w.unpushed === null && d.has_remote && !w.prunable) tags.push(`<span class="tag">push unknown</span>`);
    if (b?.behind) tags.push(`<span class="tag">↓${b.behind}</span>`);
    if (w.merged && w.branch !== d.base_local) tags.push(`<span class="tag ok">merged</span>`);
    if (w.locked) tags.push(`<span class="tag">locked</span>`);
    const used = tabsIn(w.path);
    const acts = w.prunable ? "" : `<span class="acts"><button class="link" data-wt-term="${i}">Terminal</button><button class="link" data-wt-code="${i}">VS Code</button></span>`;
    html += `<div class="git-row"><div><span class="t">${escapeHtml(wtLabel(d, w))}${tags.join("")}</span><small>${w.branch ? `⎇ ${escapeHtml(w.branch)}` : `detached HEAD ${escapeHtml(w.head)}`}${used ? ` · in Toranj: ${escapeHtml(used)}` : ""}</small></div>${acts}</div>`;
  });

  html += `<h4>Latest commits on ${escapeHtml(d.branch)}</h4>`;
  for (const c of d.commits) {
    const local = d.push_known && !c.pushed;
    html += `<div class="git-commit${local ? " local" : ""}" title="${!d.push_known ? "push unknown" : c.pushed ? "pushed" : "not pushed"} · ${escapeHtml(c.author)}"><code>${escapeHtml(c.hash)}</code><span>${escapeHtml(c.subject)}</span><small>${local ? "↑ " : ""}${ago(c.time)}</small></div>`;
  }
  if (!d.commits.length) html += `<p class="git-more">No commits yet.</p>`;

  const others = d.branches.filter((b) => b.name !== d.branch);
  if (others.length) {
    html += `<details data-g="branches"${gitOpen.has("branches") ? " open" : ""}><summary><span>Other local branches (${others.length}${d.branches_truncated ? ", newest only" : ""})</span></summary>`;
    for (const b of others) {
      const tags: string[] = [];
      if (b.unpushed) tags.push(`<span class="tag warn">↑${b.unpushed} unpushed</span>`);
      if (b.behind) tags.push(`<span class="tag">↓${b.behind}</span>`);
      if (b.gone) tags.push(`<span class="tag">remote deleted</span>`);
      if (b.merged) tags.push(`<span class="tag ok">merged</span>`);
      if (b.worktree) tags.push(`<span class="tag">worktree</span>`);
      html += `<div class="git-row"><div><span class="t">${escapeHtml(b.name)}${tags.join("")}</span><small>${b.upstream ? `tracks ${escapeHtml(b.upstream)} · ` : ""}last commit ${ago(b.time)} ago</small></div></div>`;
    }
    html += `</details>`;
  }
  if (d.stashes.length) {
    html += `<details data-g="stash"${gitOpen.has("stash") ? " open" : ""}><summary><span>Stash (${d.stashes.length})</span></summary>`;
    for (const st of d.stashes) html += `<div class="git-row"><div><span class="t">${escapeHtml(st.subject)}</span><small>${escapeHtml(st.name)} · ${ago(st.time)} ago</small></div></div>`;
    html += `</details>`;
  }
  html += `<p class="res-note">Toranj only reads the state here and changes nothing. “Pushed” and “merged” are calculated from the last fetch${d.last_fetch ? ` (${ago(d.last_fetch)} ago)` : ""}.</p>`;

  body.innerHTML = html;
  body.scrollTop = scroll;
  body.querySelectorAll<HTMLDetailsElement>("details").forEach((el) => el.addEventListener("toggle", () => {
    el.open ? gitOpen.add(el.dataset.g!) : gitOpen.delete(el.dataset.g!);
  }));
  body.querySelectorAll<HTMLButtonElement>("[data-wt-term]").forEach((b) => b.addEventListener("click", async () => {
    const w = d.worktrees[Number(b.dataset.wtTerm)];
    if (!w) return;
    try {
      const name = await invoke<string>("create_tab", { project: w.path, cwd: w.path, command: "" });
      $<HTMLDialogElement>("git-dlg").close();
      await refresh();
      open(name);
    } catch (e) {
      $("git-err").textContent = `Terminal did not open: ${e}`;
      $("git-err").hidden = false;
    }
  }));
  body.querySelectorAll<HTMLButtonElement>("[data-wt-code]").forEach((b) => b.addEventListener("click", () => {
    const w = d.worktrees[Number(b.dataset.wtCode)];
    if (w) invoke("open_in_editor", { path: w.path }).catch((e) => console.error(e));
  }));
}
const gitOpen = new Set<string>();

$("h-git").addEventListener("click", () => {
  const cur = snap.tabs.find((t) => t.name === current);
  if (cur) openGit(cur.project || cur.cwd);
});
$("git-reload").addEventListener("click", () => loadGit());
$("git-close").addEventListener("click", () => $<HTMLDialogElement>("git-dlg").close());
$("git-dlg").addEventListener("close", () => { gitSeq++; });

function escapeHtml(s: string) {
  return s.replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]!);
}

// ---------- Claude hooks ----------
interface HookStatus { installed: boolean; broken: boolean; hook_path: string; hook_exists: boolean; settings_path: string; preview: string }

async function loadHooks() {
  let st: HookStatus;
  try { st = await invoke<HookStatus>("claude_hooks_status"); } catch { $("hooks").hidden = true; return; }
  if (!st) { $("hooks").hidden = true; return; }
  const box = $("hooks");
  box.hidden = false;
  box.classList.toggle("on", st.installed);
  box.classList.toggle("on", st.installed && !st.broken);
  $("hooks-text").textContent = st.broken
    ? "The Claude hook program is missing, so the hooks report errors."
    : st.installed ? "Claude statuses: on" : "Turn on precise statuses for Claude Code";
  const btn = $<HTMLButtonElement>("hooks-btn");
  btn.textContent = st.broken ? "Remove hooks" : st.installed ? "Turn off" : "Turn on…";
  btn.onclick = async () => {
    if (st.installed || st.broken) {
      try { await invoke("uninstall_claude_hooks"); } catch (e) { $("hooks-text").textContent = `Could not turn off: ${e}`; return; }
      return loadHooks();
    }
    const home = /^\/Users\/[^/]+|^\/home\/[^/]+/;
    $("hooks-settings").textContent = st.settings_path.replace(home, "~");
    $("hooks-bin").textContent = st.hook_path.replace(home, "~");
    $("hooks-preview").textContent = st.preview;
    const err = $("hooks-err");
    err.hidden = st.hook_exists;
    err.textContent = st.hook_exists ? "" : "The hook program was not found next to the app. In development, build it with: npm run build:hook";
    $<HTMLButtonElement>("hooks-ok").disabled = !st.hook_exists;
    const dlg = $<HTMLDialogElement>("hooks-dlg");
    dlg.returnValue = "";
    dlg.onclose = async () => {
      if (dlg.returnValue !== "yes") return;
      try {
        await invoke("install_claude_hooks");
        loadHooks();
      } catch (e) {
        $("hooks-text").textContent = `Could not turn on: ${e}`;
      }
    };
    dlg.showModal();
  };
}

// ---------- diagnostics ----------
// UI errors go to ~/Library/Logs/Toranj/ui.log (Linux: ~/.local/state/toranj/ui.log).
function logUi(msg: string) { invoke("log_ui", { msg: msg.slice(0, 2000) }).catch(() => {}); }
window.addEventListener("error", (e) => logUi(`error: ${e.message} @ ${e.filename}:${e.lineno}`));
window.addEventListener("unhandledrejection", (e) => logUi(`rejection: ${String(e.reason)}`));

// ---------- boot ----------
(async () => {
  if (import.meta.env.DEV && location.search.includes("mock")) {
    await (await import("./dev/mock")).installMock();
  }
  if (!(await invoke<boolean>("tmux_available"))) {
    $("empty").querySelector("h2")!.textContent = "tmux is not installed";
    $("empty").querySelector("p")!.textContent = "Toranj keeps terminals in tmux. Install it with: brew install tmux";
  }
  loadHooks();
  // Hooks can fire many times a second; refresh at most every 500 ms, always
  // once more after the last event so the final state is never missed.
  let spoolTimer: number | undefined;
  let spoolLast = 0;
  listen("toranj://close-tab", closeCurrent);
  listen("toranj://spool", () => {
    clearTimeout(spoolTimer);
    const wait = Math.max(0, 500 - (Date.now() - spoolLast));
    spoolTimer = window.setTimeout(() => { spoolLast = Date.now(); refresh(); }, wait);
  });
  loadResources();
  setInterval(loadResources, 5000);
  document.addEventListener("visibilitychange", loadResources);
  onNotificationClick((tab) => { if (snap.tabs.some((t) => t.name === tab)) open(tab); });
  window.addEventListener("focus", () => { for (const n of panes.keys()) doneMarks.delete(n); render(); });
  await refresh();
  // Back to the terminal you last had open, else the overview (never just
  // the first tab by name).
  let last: string | null = null;
  try { last = localStorage.getItem("toranj.view"); } catch { /* none */ }
  if (last && snap.tabs.some((t) => t.name === last)) open(last);
  else { overview = snap.tabs.length > 0; render(); }
  setInterval(refresh, 2000);
})();
