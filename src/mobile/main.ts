// Toranj on a phone: a web app served by Toranj on the Mac (through
// `tailscale serve`). It shows every terminal's status, answers Claude's
// questions, shows a terminal and types into it, opens new terminals and
// receives push notifications.
import { renderAnsi } from "./ansi";

type State = "running" | "waiting" | "idle" | "done" | "error";
interface Tab { name: string; project: string; cwd: string; agent: string; state: State; detail: string; title: string; since: number; background: boolean }
interface Snapshot { tabs: Tab[] }
interface Choice { key: string; label: string }
interface Peek { prompt: { context: string[]; choices: Choice[]; fingerprint: string } | null; lines: string[] }
interface Project { name: string; path: string }
interface Past { id: string; title: string; modified: number }
interface Me { name: string; push: boolean; vapid: string }

const TOKEN_KEY = "toranj.mobile.token";
const LABEL: Record<State, string> = { running: "running", waiting: "waiting for you", idle: "idle", done: "done", error: "error" };
const app = document.getElementById("app")!;

// ---------- small helpers ----------
function token(): string {
  try { return localStorage.getItem(TOKEN_KEY) ?? ""; } catch { return ""; }
}
function setToken(t: string) {
  try { if (t) localStorage.setItem(TOKEN_KEY, t); else localStorage.removeItem(TOKEN_KEY); } catch { /* private mode */ }
}

class ApiError extends Error { constructor(public status: number, msg: string) { super(msg); } }

async function api<T>(path: string, body?: unknown): Promise<T> {
  const res = await fetch(path, {
    method: body === undefined ? "GET" : "POST",
    headers: { Authorization: `Bearer ${token()}`, ...(body === undefined ? {} : { "Content-Type": "application/json" }) },
    body: body === undefined ? undefined : JSON.stringify(body),
    cache: "no-store",
  });
  if (res.status === 401) { setToken(""); route(); throw new ApiError(401, "not paired"); }
  if (!res.ok) throw new ApiError(res.status, (await res.text()) || res.statusText);
  return res.status === 204 ? (undefined as T) : res.json();
}

type Kid = Node | string | null | undefined | false;
function h<K extends keyof HTMLElementTagNameMap>(tag: K, attrs: Record<string, string | boolean | ((e: Event) => void)> = {}, ...kids: Kid[]): HTMLElementTagNameMap[K] {
  const el = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (typeof v === "function") el.addEventListener(k.replace(/^on/, ""), v);
    else if (v === true) el.setAttribute(k, "");
    else if (v !== false) el.setAttribute(k, v);
  }
  for (const k of kids) if (k) el.append(k);
  return el;
}

const base = (p: string) => p.replace(/\/+$/, "").split("/").pop() || p;
function ago(unix: number): string {
  const s = Math.max(0, Math.floor(Date.now() / 1000) - unix);
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min`;
  if (s < 86400) return `${Math.floor(s / 3600)} h`;
  return `${Math.floor(s / 86400)} d`;
}
const tabTitle = (t: Tab) => t.title || t.agent || "terminal";
const standalone = () => matchMedia("(display-mode: standalone)").matches || (navigator as unknown as { standalone?: boolean }).standalone === true;

let toastTimer = 0;
function toast(msg: string, bad = false) {
  let el = document.getElementById("toast");
  if (!el) { el = h("div", { id: "toast", role: "status" }); document.body.append(el); }
  el.textContent = msg;
  el.className = bad ? "show bad" : "show";
  clearTimeout(toastTimer);
  toastTimer = window.setTimeout(() => { el!.className = ""; }, 2600);
}

// ---------- routing ----------
let stopView: (() => void) | null = null;
/** Bumped on every navigation: work started by an old view checks it after
 *  each await and leaves the screen alone. */
let viewGen = 0;

function route() {
  stopView?.();
  stopView = null;
  viewGen++;
  window.scrollTo(0, 0);
  if (!token()) return pairView();
  if (codeFromUrl()) history.replaceState(null, "", "/"); // already paired
  const hash = location.hash.slice(1);
  const p = new URLSearchParams(hash);
  if (p.get("tab")) return termView(p.get("tab")!);
  if (hash === "new") return newView();
  return homeView();
}
window.addEventListener("hashchange", route);
const go = (hash: string) => { if (location.hash.slice(1) === hash) route(); else location.hash = hash; };

/** Polls `fn` every `ms` while the app is visible; returns a stopper. One
 *  loop only: coming back to the app while a request is out doesn't start
 *  a second one. */
function poll(fn: () => Promise<void> | void, ms: number): () => void {
  let timer = 0;
  let alive = true;
  let busy = false;
  const tick = async () => {
    if (!alive || busy) return;
    clearTimeout(timer);
    busy = true;
    if (document.visibilityState === "visible") { try { await fn(); } catch { /* shown by fn */ } }
    busy = false;
    if (alive) timer = window.setTimeout(tick, ms);
  };
  const vis = () => { if (document.visibilityState === "visible" && !busy) void tick(); };
  document.addEventListener("visibilitychange", vis);
  void tick();
  return () => { alive = false; clearTimeout(timer); document.removeEventListener("visibilitychange", vis); };
}

// ---------- pairing ----------
/** The pairing code from the QR (`#pair=CODE`), if the app was opened with one. */
function codeFromUrl(): string {
  return new URLSearchParams(location.hash.slice(1)).get("pair") ?? "";
}

const isIos = () => /iPhone|iPad|iPod/.test(navigator.userAgent) || (navigator.platform === "MacIntel" && navigator.maxTouchPoints > 1);

/** Safari's Share icon (a box with an arrow up), drawn so the steps match what's on screen. */
function shareIcon(): SVGSVGElement {
  const ns = "http://www.w3.org/2000/svg";
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", "ico");
  for (const d of ["M12 3v12", "M8 7l4-4 4 4", "M6 11H5v10h14V11h-1"]) {
    const path = document.createElementNS(ns, "path");
    path.setAttribute("d", d);
    svg.append(path);
  }
  return svg;
}

/** Opens the iOS share sheet from the page (the Share button itself is
 *  tucked under ••• in Safari's toolbar). */
function shareButton(): HTMLElement | null {
  if (!("share" in navigator)) return null;
  return h("button", { class: "btn big", type: "button", onclick: () => {
    navigator.share({ title: "Toranj", url: location.origin + "/" }).catch(() => { /* closed */ });
  } }, shareIcon(), " Share…");
}

/** One tap: a profile that puts the Toranj icon on the Home Screen. */
function installButton(): HTMLElement {
  const code = codeFromUrl();
  return h("a", { class: "btn primary big", href: `/toranj.mobileconfig${code ? `?code=${encodeURIComponent(code)}` : ""}` }, "Add to Home Screen");
}

function installSteps(): HTMLElement {
  const step = (n: string, ...kids: Kid[]) => h("li", {}, h("span", { class: "n" }, n), h("div", {}, ...kids));
  return h("div", {},
    installButton(),
    h("ol", { class: "steps" },
      step("1", "Tap ", h("b", {}, "Add to Home Screen"), " above, then ", h("b", {}, "Allow"), "."),
      step("2", "Open ", h("b", {}, "Settings"), " → ", h("b", {}, "Profile Downloaded"), " (at the top) → ", h("b", {}, "Install"), "."),
      step("3", "Open ", h("b", {}, "Toranj"), " from your Home Screen. ", codeFromUrl() ? "It pairs itself." : "If it asks for a code: on the Mac, Settings → Phone → Pair a phone."),
    ),
    h("details", { class: "alt" },
      h("summary", {}, "Or add it with Safari’s Share menu"),
      h("p", { class: "mute" }, "In Safari tap ••• (bottom right) → Share → Add to Home Screen, keep “Open as Web App” on, tap Add."),
      shareButton()),
  );
}

/** In Safari on an iPhone: only how to add Toranj to the Home Screen.
 *  Pairing happens once, in the Home Screen app (it has its own storage,
 *  and only it can receive notifications). */
function installView() {
  app.replaceChildren(h("div", { class: "page install" },
    h("img", { src: "/m-icon-180.png", class: "app-ico", alt: "" }),
    h("h1", {}, "Add Toranj to your Home Screen"),
    h("p", { class: "mute" }, "It then opens like an app and can send you notifications."),
    installSteps(),
    h("button", { class: "link", type: "button", onclick: () => pairView(true) }, "Pair in this browser instead"),
  ));
}

function pairView(force = false) {
  if (!force && isIos() && !standalone()) return installView();
  const err = h("p", { class: "err", hidden: true });
  const code = h("input", { id: "code", inputmode: "text", autocapitalize: "characters", autocomplete: "one-time-code", placeholder: "ABCD2345", maxlength: "12", spellcheck: "false", value: codeFromUrl() });
  const name = h("input", { id: "dname", placeholder: "iPhone", maxlength: "40", value: /iPad/.test(navigator.userAgent) ? "iPad" : "iPhone" });
  const btn = h("button", { class: "btn primary big", type: "submit" }, "Pair");
  const pair = async () => {
    err.hidden = true;
    btn.disabled = true;
    try {
      const res = await fetch("/api/pair", { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify({ code: code.value, name: name.value }) });
      if (!res.ok) throw new Error((await res.text()) || "Pairing failed.");
      const { token: t } = await res.json();
      setToken(t);
      history.replaceState(null, "", "/"); // the code is used up
      go("");
    } catch (x) {
      err.textContent = x instanceof Error ? x.message : String(x);
      err.hidden = false;
    } finally { btn.disabled = false; }
  };
  const form = h("form", { class: "card pair", onsubmit: (e: Event) => { e.preventDefault(); void pair(); } },
    h("h1", {}, "Pair with Toranj"),
    h("p", { class: "mute" }, "On the Mac: Toranj → Settings → Phone → “Pair a phone”, then type the code shown there."),
    h("label", { for: "code" }, "Code"), code,
    h("label", { for: "dname" }, "Name of this phone"), name,
    err, btn,
  );
  app.replaceChildren(h("div", { class: "page" }, form));
  // Opened from the QR: pair right away, no typing.
  if (code.value) void pair();
  else code.focus();
}

// ---------- notifications ----------
function b64ToBytes(b64: string): Uint8Array<ArrayBuffer> {
  const s = atob(b64.replace(/-/g, "+").replace(/_/g, "/") + "===".slice((b64.length + 3) % 4));
  const out = new Uint8Array(new ArrayBuffer(s.length));
  for (let i = 0; i < s.length; i++) out[i] = s.charCodeAt(i);
  return out;
}

function b64FromBytes(b: Uint8Array): string {
  let s = "";
  for (const x of b) s += String.fromCharCode(x);
  return btoa(s).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

async function swReg(): Promise<ServiceWorkerRegistration | null> {
  if (!("serviceWorker" in navigator)) return null;
  try { return await navigator.serviceWorker.register("/sw.js", { scope: "/" }); } catch { return null; }
}

async function enablePush(me: Me): Promise<boolean> {
  if (!("Notification" in window) || !("PushManager" in window)) {
    toast(standalone() ? "This browser can't receive notifications." : "Open Toranj from your Home Screen to turn on notifications.", true);
    return false;
  }
  const perm = await Notification.requestPermission();
  if (perm !== "granted") { toast("Notifications are not allowed for Toranj.", true); return false; }
  const reg = await swReg();
  if (!reg) { toast("Could not start notifications.", true); return false; }
  await navigator.serviceWorker.ready;
  let sub = await reg.pushManager.getSubscription();
  // Made for another key (the Mac's key changed): it would never deliver.
  const key = sub?.options.applicationServerKey;
  if (sub && key && b64FromBytes(new Uint8Array(key)) !== me.vapid) { await sub.unsubscribe(); sub = null; }
  if (!sub) sub = await reg.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: b64ToBytes(me.vapid) });
  await api("/api/push/subscribe", sub.toJSON());
  return true;
}

// ---------- home ----------
const peekCache = new Map<string, { key: string; peek: Peek }>();

async function peekFor(t: Tab): Promise<Peek | null> {
  const key = `${t.since}:${t.detail}`;
  const c = peekCache.get(t.name);
  if (c && c.key === key) return c.peek;
  try {
    const p = await api<Peek>(`/api/peek?name=${encodeURIComponent(t.name)}`);
    peekCache.set(t.name, { key, peek: p });
    return p;
  } catch { return null; }
}

function choicesEl(t: Tab, p: NonNullable<Peek["prompt"]>, after: () => void): HTMLElement {
  const box = h("div", { class: "choices" });
  for (const c of p.choices) {
    box.append(h("button", { class: "btn choice", type: "button", onclick: async (e: Event) => {
      const b = e.currentTarget as HTMLButtonElement;
      box.querySelectorAll("button").forEach((x) => { (x as HTMLButtonElement).disabled = true; });
      b.classList.add("busy");
      try {
        await api("/api/answer", { name: t.name, key: c.key, fingerprint: p.fingerprint });
        peekCache.delete(t.name);
        toast("Answered");
      } catch (x) {
        toast(x instanceof Error ? x.message : String(x), true);
      }
      after();
    } }, `${c.key}. ${c.label}`));
  }
  return box;
}

function homeView() {
  const list = h("div", { class: "list" });
  const waitBox = h("div", { class: "waits" });
  const pushBox = h("div");
  const head = h("header", { class: "top" },
    h("h1", {}, "Toranj"),
    h("button", { class: "btn primary", type: "button", onclick: () => go("new") }, "+ New"),
  );
  app.replaceChildren(h("div", { class: "page" }, head, pushBox, waitBox, list));

  // Notifications: offer to turn them on, or a test once they are.
  if (isIos() && !standalone()) {
    pushBox.replaceChildren(h("div", { class: "card install-card" },
      h("b", {}, "Add Toranj to your Home Screen"),
      h("p", { class: "mute" }, "Notifications only work in the Home Screen app."),
      installSteps()));
  } else void api<Me>("/api/me").then((me) => {
    const granted = "Notification" in window && Notification.permission === "granted";
    if (me.push && granted) return;
    const btn = h("button", { class: "btn", type: "button", onclick: async () => {
      btn.disabled = true;
      try {
        if (await enablePush(me)) {
          pushBox.replaceChildren(h("div", { class: "card ok" }, "Notifications are on. ", h("button", { class: "link", type: "button", onclick: () => api("/api/push/test", {}).then(() => toast("Test sent"), (x) => toast(String(x.message ?? x), true)) }, "Send a test")));
          return;
        }
      } catch (x) { toast(x instanceof Error ? x.message : String(x), true); }
      btn.disabled = false;
    } }, "Turn on notifications");
    pushBox.replaceChildren(h("div", { class: "card notice" },
      h("span", {}, standalone() || !/iPhone|iPad/.test(navigator.userAgent) ? "Get a notification when an agent finishes or needs you." : "Add Toranj to your Home Screen to get notifications."),
      btn));
  }).catch(() => {});

  const gen = viewGen;
  const draw = async () => {
    let snap: Snapshot;
    try { snap = await api<Snapshot>("/api/snapshot"); }
    catch (x) {
      if (!(x instanceof ApiError && x.status === 401)) list.replaceChildren(h("div", { class: "card err" }, "Toranj on the Mac is not reachable. Is the Mac awake, with Tailscale on?"));
      return;
    }
    if (gen !== viewGen) return;
    const tabs = snap.tabs;
    // Questions first: answer them right here.
    const waiting = tabs.filter((t) => t.state === "waiting");
    const cards: HTMLElement[] = [];
    for (const t of waiting) {
      const p = await peekFor(t);
      if (gen !== viewGen) return;
      const card = h("div", { class: "card wait" },
        h("button", { class: "wait-head", type: "button", onclick: () => go(`tab=${encodeURIComponent(t.name)}`) },
          h("b", {}, base(t.project || t.cwd)), h("span", {}, ` · ${tabTitle(t)}`)),
      );
      if (p?.prompt) {
        card.append(h("pre", { class: "ctx" }, p.prompt.context.join("\n")));
        card.append(choicesEl(t, p.prompt, () => void draw()));
      } else {
        card.append(h("p", { class: "mute" }, t.detail || "Waiting for you"));
      }
      cards.push(card);
    }
    waitBox.replaceChildren(...cards);

    // Then every terminal, by project.
    const by = new Map<string, Tab[]>();
    for (const t of tabs) {
      const k = t.project || t.cwd;
      by.set(k, [...(by.get(k) ?? []), t]);
    }
    const groups = [...by].sort((a, b) => base(a[0]).localeCompare(base(b[0])));
    if (!groups.length) {
      list.replaceChildren(h("div", { class: "card mute" }, "No terminals open. Tap “+ New” to start one."));
      return;
    }
    list.replaceChildren(...groups.map(([proj, ts]) => h("section", { class: "proj" },
      h("h2", {}, base(proj)),
      ...ts.map((t) => h("button", { class: `row ${t.state}`, type: "button", onclick: () => go(`tab=${encodeURIComponent(t.name)}`) },
        h("i", { class: `st ${t.state}` }),
        h("span", { class: "rt" }, tabTitle(t)),
        h("small", {}, t.state === "running" ? ago(t.since) : t.state === "idle" ? "" : LABEL[t.state]),
      )),
    )));
  };
  stopView = poll(draw, 3000);
}

// ---------- one terminal ----------
function termView(name: string) {
  const title = h("span", { class: "tt" }, "…");
  const st = h("i", { class: "st" });
  const screen = h("pre", { class: "screen" });
  const promptBox = h("div", { class: "prompt", hidden: true });
  const input = h("textarea", { rows: "1", placeholder: "Message", enterkeyhint: "send", autocapitalize: "sentences" });
  let sending = false;
  const gen = viewGen;
  const send = async () => {
    const text = input.value.trim();
    if (!text) return;
    if (sending) return;
    sending = true;
    input.readOnly = true; // keeps the keyboard up, unlike disabled
    try {
      await api("/api/send", { name, text });
      input.value = "";
      autoGrow();
      stick = true;
      setTimeout(() => void drawScreen(), 300);
    } catch (x) { toast(x instanceof Error ? x.message : String(x), true); }
    input.readOnly = false;
    sending = false;
  };
  const autoGrow = () => { input.style.height = "auto"; input.style.height = `${Math.min(120, input.scrollHeight)}px`; };
  input.addEventListener("input", autoGrow);
  input.addEventListener("keydown", (e) => { if (e.key === "Enter" && !e.shiftKey) { e.preventDefault(); void send(); } });
  const key = (k: string, label: string) => h("button", { class: "key", type: "button", onclick: async () => {
    try { await api("/api/send", { name, key: k }); setTimeout(() => void drawScreen(), 250); }
    catch (x) { toast(x instanceof Error ? x.message : String(x), true); }
  } }, label);
  const menu = h("button", { class: "link", type: "button", onclick: async () => {
    const msg = tab?.background
      ? "Close this terminal? The background Claude session keeps running; stop it from the Mac."
      : "Close this terminal? Whatever runs in it is stopped.";
    if (!confirm(msg)) return;
    try { await api("/api/kill", { name }); go(""); }
    catch (x) { toast(x instanceof Error ? x.message : String(x), true); }
  } }, "Close");
  app.replaceChildren(h("div", { class: "term" },
    h("header", { class: "top term-top" },
      h("button", { class: "link back", type: "button", onclick: () => go("") }, "‹ Back"),
      h("div", { class: "tt-wrap" }, st, title),
      menu),
    screen,
    h("div", { class: "dock" },
      promptBox,
      h("div", { class: "keys" }, key("Escape", "esc"), key("BTab", "⇧⇥"), key("Up", "↑"), key("Down", "↓"), key("Enter", "⏎"), key("C-c", "^C")),
      h("div", { class: "send" }, input, h("button", { class: "btn primary", type: "button", onclick: () => void send() }, "Send")),
    ),
  ));

  // Stay at the bottom unless the user scrolled up to read.
  let stick = true;
  screen.addEventListener("scroll", () => { stick = screen.scrollHeight - screen.scrollTop - screen.clientHeight < 60; });
  let lastText = "";
  let tab: Tab | undefined;
  let lastPeekKey = "";

  const drawScreen = async () => {
    try {
      const { text } = await api<{ text: string; cols: number }>(`/api/screen?name=${encodeURIComponent(name)}`);
      if (gen !== viewGen) return;
      const trimmed = text.replace(/\s+$/, "");
      if (trimmed !== lastText) {
        lastText = trimmed;
        renderAnsi(trimmed, screen);
        if (stick) screen.scrollTop = screen.scrollHeight;
      }
    } catch (x) {
      if (gen === viewGen && x instanceof ApiError && x.status === 404) { toast("This terminal was closed."); go(""); }
    }
  };
  const drawState = async () => {
    const snap = await api<Snapshot>("/api/snapshot").catch(() => null);
    if (gen !== viewGen || !snap) return;
    tab = snap.tabs.find((t) => t.name === name);
    if (!tab) { toast("This terminal was closed."); go(""); return; }
    title.textContent = `${base(tab.project || tab.cwd)} · ${tabTitle(tab)}`;
    st.className = `st ${tab.state}`;
    if (tab.state === "waiting") {
      const k = `${tab.since}:${tab.detail}`;
      if (k !== lastPeekKey || promptBox.hidden) {
        lastPeekKey = k;
        const p = await peekFor(tab);
        if (gen !== viewGen) return;
        if (p?.prompt) {
          promptBox.replaceChildren(choicesEl(tab, p.prompt, () => { promptBox.hidden = true; void drawScreen(); }));
          promptBox.hidden = false;
        }
      }
    } else {
      promptBox.hidden = true;
      lastPeekKey = "";
    }
  };
  // Keep the input above the iPhone keyboard: size the view to what's visible.
  const termEl = app.querySelector<HTMLElement>(".term")!;
  const fit = () => {
    const vv = window.visualViewport;
    if (vv) termEl.style.height = `${vv.height}px`;
    if (stick) screen.scrollTop = screen.scrollHeight;
  };
  window.visualViewport?.addEventListener("resize", fit);
  fit();
  const s1 = poll(drawScreen, 1200);
  const s2 = poll(drawState, 2500);
  stopView = () => { s1(); s2(); window.visualViewport?.removeEventListener("resize", fit); };
}

// ---------- new terminal ----------
function newView() {
  let projects: Project[] = [];
  let picked: Project | null = null;
  let cmd = "claude";
  let resume = "";
  const q = h("input", { placeholder: "Search projects…", autocapitalize: "off", spellcheck: "false" });
  const list = h("div", { class: "plist" });
  const chips = h("div", { class: "chips" });
  const convBox = h("div", { class: "convs" });
  const open = h("button", { class: "btn primary big", type: "button", disabled: true }, "Open");

  const drawList = () => {
    const f = q.value.toLowerCase();
    const hits = projects.filter((p) => p.name.toLowerCase().includes(f) || p.path.toLowerCase().includes(f)).slice(0, 60);
    list.replaceChildren(...hits.map((p) => h("button", { class: "prow" + (p === picked ? " on" : ""), type: "button", onclick: () => { picked = p; resume = ""; drawList(); void drawConvs(); open.disabled = false; } },
      h("b", {}, p.name), h("small", {}, p.path.replace(/^\/Users\/[^/]+/, "~")))));
  };
  const drawChips = () => {
    chips.replaceChildren(...[["claude", "claude"], ["codex", "codex"], ["", "shell"]].map(([c, label]) =>
      h("button", { class: "chip" + (c === cmd ? " on" : ""), type: "button", onclick: () => { cmd = c; resume = ""; drawChips(); void drawConvs(); } }, label)));
  };
  let convFor = "";
  let pasts: Past[] = [];
  const drawConvs = async () => {
    if (!picked || cmd !== "claude") { convBox.replaceChildren(); return; }
    if (convFor !== picked.path) {
      const forPath = picked.path;
      convFor = forPath;
      pasts = [];
      const got = await api<Past[]>(`/api/recent?cwd=${encodeURIComponent(forPath)}`).catch(() => [] as Past[]);
      // Another project was picked meanwhile: these belong to the old one.
      if (picked?.path !== forPath) return;
      pasts = got;
    }
    if (!pasts.length) { convBox.replaceChildren(); return; }
    convBox.replaceChildren(h("h3", {}, "Conversation"),
      ...[{ id: "", title: "New conversation", modified: 0 }, ...pasts].map((p) =>
        h("button", { class: "prow conv" + (p.id === resume ? " on" : ""), type: "button", onclick: () => { resume = p.id; void drawConvs(); open.textContent = resume ? "Continue" : "Open"; } },
          h("b", {}, p.title), p.modified ? h("small", {}, `${ago(Math.floor(p.modified / 1000))} ago`) : null)));
  };
  q.addEventListener("input", drawList);
  open.addEventListener("click", async () => {
    if (!picked) return;
    open.disabled = true;
    const command = cmd === "claude" && resume ? `claude --resume ${resume}` : cmd;
    try {
      const { name } = await api<{ name: string }>("/api/create", { project: picked.path, cwd: picked.path, command });
      go(`tab=${encodeURIComponent(name)}`);
    } catch (x) {
      toast(x instanceof Error ? x.message : String(x), true);
      open.disabled = false;
    }
  });
  app.replaceChildren(h("div", { class: "page" },
    h("header", { class: "top" }, h("button", { class: "link back", type: "button", onclick: () => go("") }, "‹ Back"), h("h1", {}, "New terminal"), h("span")),
    h("div", { class: "card" }, q, list),
    h("div", { class: "card" }, h("h3", {}, "Command"), chips, convBox),
    open,
  ));
  drawChips();
  void api<Project[]>("/api/projects").then((p) => { projects = p.filter((x) => !(x as { count?: number }).count); drawList(); }).catch((x) => toast(String(x.message ?? x), true));
}

// ---------- start ----------
void swReg();
navigator.serviceWorker?.addEventListener("message", (e) => {
  const tab = (e.data as { open?: string })?.open;
  if (typeof tab === "string" && tab) go(`tab=${encodeURIComponent(tab)}`);
});
route();
