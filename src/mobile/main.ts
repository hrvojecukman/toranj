// Toranj on a phone: a web app served by Toranj on the Mac (through
// `tailscale serve`). It shows every terminal's status, answers Claude's
// questions, shows a terminal and types into it, opens new terminals and
// receives push notifications.
import { renderAnsi } from "./ansi";

type State = "running" | "waiting" | "idle" | "done" | "error";
interface Tab { name: string; project: string; cwd: string; agent: string; state: State; detail: string; title: string; since: number; background: boolean; session_id: string }
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
  for (const close of [...openSheets]) close();
  stopView?.();
  stopView = null;
  viewGen++;
  window.scrollTo(0, 0);
  if (!token()) return pairView();
  if (codeFromUrl()) history.replaceState(null, "", "/"); // already paired, drop the used code
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
/** A pairing code in a URL: `?pair=CODE` (the Home Screen icon) or
 *  `#pair=CODE` (the QR code on the Mac). */
function codeIn(url: string): string {
  try {
    const u = new URL(url, location.href);
    const c = u.searchParams.get("pair") ?? new URLSearchParams(u.hash.slice(1)).get("pair") ?? "";
    return c.replace(/[^A-Za-z0-9]/g, "").slice(0, 12);
  } catch { return ""; }
}
const codeFromUrl = () => codeIn(location.href);

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
  const scan = h("button", { class: "btn primary big", type: "button", onclick: () => void scanQr((c) => { code.value = c; void pair(); }) }, "Scan the QR code on the Mac");
  const form = h("form", { class: "card pair", onsubmit: (e: Event) => { e.preventDefault(); void pair(); } },
    h("h1", {}, "Pair with Toranj"),
    h("p", { class: "mute" }, "On the Mac: Toranj → Settings → Phone → “Pair a phone”. Then scan the QR code shown there, or type its code."),
    scan,
    h("label", { for: "code" }, "Or type the code"), code,
    h("label", { for: "dname" }, "Name of this phone"), name,
    err, btn,
  );
  app.replaceChildren(h("div", { class: "page" }, form));
  // Opened with a code: pair right away, no typing.
  if (code.value) void pair();
}

/** Full-screen camera that reads the Mac's QR code and hands back its
 *  pairing code. The decoder is loaded only when needed. */
async function scanQr(done: (code: string) => void) {
  if (!navigator.mediaDevices?.getUserMedia) { toast("This browser can't use the camera.", true); return; }
  let stream: MediaStream;
  try {
    stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" }, audio: false });
  } catch {
    toast("Camera not allowed. Type the code instead.", true);
    return;
  }
  const { default: jsQR } = await import("jsqr");
  const video = h("video", { playsinline: true, muted: true, autoplay: true });
  video.srcObject = stream;
  const hint = h("p", {}, "Point the camera at the QR code in Toranj’s Settings on the Mac.");
  let live = true;
  const close = () => { live = false; stream.getTracks().forEach((t) => t.stop()); overlay.remove(); };
  const overlay = h("div", { class: "scanner" }, video, h("div", { class: "frame" }), hint,
    h("button", { class: "btn", type: "button", onclick: close }, "Cancel"));
  document.body.append(overlay);
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d", { willReadFrequently: true })!;
  const tick = () => {
    if (!live) return;
    if (video.readyState >= 2 && video.videoWidth) {
      // A smaller frame decodes faster and is plenty for a screen QR.
      const scale = Math.min(1, 640 / video.videoWidth);
      canvas.width = Math.round(video.videoWidth * scale);
      canvas.height = Math.round(video.videoHeight * scale);
      ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
      const img = ctx.getImageData(0, 0, canvas.width, canvas.height);
      const hit = jsQR(img.data, img.width, img.height, { inversionAttempts: "dontInvert" });
      const c = hit ? codeIn(hit.data) : "";
      if (c) { close(); done(c); return; }
      if (hit) hint.textContent = "That QR code isn't Toranj's pairing code.";
    }
    requestAnimationFrame(tick);
  };
  requestAnimationFrame(tick);
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
  const [clipEl, stopClip] = clipBar();
  const head = h("header", { class: "top sticky" },
    h("h1", {}, "Toranj"),
    h("div", { class: "top-actions" },
      h("button", { class: "icon-btn", type: "button", "aria-label": "Mac clipboard", onclick: async () => {
        // The text must be there before the tap that copies it.
        if (!lastClip) lastClip = await api<Clip>("/api/clipboard").catch(() => null);
        clipSheet();
      } }, icon("copy")),
      h("button", { class: "btn primary", type: "button", onclick: () => go("new") }, "+ New")),
  );
  app.replaceChildren(h("div", { class: "page" }, head, clipEl, pushBox, waitBox, list));

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
        h("span", { class: "rt" },
          h("span", { class: "rt-title" }, tabTitle(t)),
          t.detail && t.state !== "idle" ? h("small", { class: "rt-detail" }, t.detail) : null),
        h("small", { class: "rt-state" }, t.state === "running" ? ago(t.since) : t.state === "idle" ? "" : LABEL[t.state]),
        h("span", { class: "chev", "aria-hidden": "true" }, "›"),
      )),
    )));
  };
  const stopDraw = poll(draw, 3000);
  stopView = () => { stopDraw(); stopClip(); };
}

// ---------- copying ----------
/** Copies `text` to the phone's clipboard. Call it straight from a tap:
 *  iOS only allows it inside the gesture. */
function copyText(text: string, what = "Copied") {
  const fallback = () => {
    const ta = h("textarea", { readonly: true, class: "offscreen" });
    ta.value = text;
    document.body.append(ta);
    ta.select();
    ta.setSelectionRange(0, text.length);
    let ok = false;
    try { ok = document.execCommand("copy"); } catch { /* not allowed */ }
    ta.remove();
    toast(ok ? what : "Couldn’t copy", !ok);
  };
  // The fallback only works inside the tap, so it isn't tried after a rejection.
  if (navigator.clipboard?.writeText) navigator.clipboard.writeText(text).then(() => toast(what), () => toast("Couldn’t copy", true));
  else fallback();
}

const ns = "http://www.w3.org/2000/svg";
/** A small line icon. */
function icon(name: "copy" | "clip" | "send" | "more" | "back" | "x" | "check"): SVGSVGElement {
  const paths: Record<string, string[]> = {
    copy: ["M9 9h10v12H9z", "M5 15V3h10"],
    clip: ["M21 11.5l-8.6 8.6a5 5 0 0 1-7.1-7.1l8.6-8.6a3.3 3.3 0 0 1 4.7 4.7l-8.6 8.6a1.7 1.7 0 0 1-2.4-2.4l7.9-7.9"],
    send: ["M12 19V5", "M6 11l6-6 6 6"],
    more: ["M5 12h.01", "M12 12h.01", "M19 12h.01"],
    back: ["M15 5l-7 7 7 7"],
    x: ["M6 6l12 12", "M18 6L6 18"],
    check: ["M5 12.5l4.5 4.5L19 7"],
  };
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", `ic ic-${name}`);
  for (const d of paths[name]) {
    const p = document.createElementNS(ns, "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  return svg;
}

/** Open sheets: they belong to the view that opened them. */
const openSheets = new Set<() => void>();

/** A bottom sheet over the page; returns its closer. */
function sheet(title: string, ...kids: Kid[]): () => void {
  const close = () => { openSheets.delete(close); wrap.classList.remove("show"); setTimeout(() => wrap.remove(), 200); document.removeEventListener("keydown", esc); };
  openSheets.add(close);
  const esc = (e: KeyboardEvent) => { if (e.key === "Escape") close(); };
  const box = h("div", { class: "sheet", role: "dialog", "aria-label": title },
    h("div", { class: "sheet-head" }, h("b", {}, title), h("button", { class: "icon-btn", type: "button", "aria-label": "Close", onclick: close }, icon("x"))),
    ...kids);
  const wrap = h("div", { class: "sheet-wrap", onclick: (e: Event) => { if (e.target === wrap) close(); } }, box);
  document.body.append(wrap);
  document.addEventListener("keydown", esc);
  requestAnimationFrame(() => wrap.classList.add("show"));
  return close;
}

// ---------- the Mac's clipboard ----------
interface Clip { text: string; from: "mac" | "terminal"; id: string }
const CLIP_SEEN = "toranj.mobile.clipSeen";
let lastClip: Clip | null = null;
const clipSeen = () => { try { return localStorage.getItem(CLIP_SEEN) ?? ""; } catch { return ""; } };
const markClipSeen = (id: string) => { try { localStorage.setItem(CLIP_SEEN, id); } catch { /* private mode */ } };

/** Shows the full text the Mac copied, selectable, with Copy. */
function clipSheet() {
  const c = lastClip;
  if (!c || !c.text.trim()) { toast("Nothing copied on the Mac yet."); return; }
  const ta = h("textarea", { class: "sel-text", readonly: true });
  ta.value = c.text;
  const close = sheet(c.from === "terminal" ? "Copied in a terminal" : "Mac clipboard",
    ta,
    h("button", { class: "btn primary big", type: "button", onclick: () => { copyText(c.text); markClipSeen(c.id); close(); } }, icon("copy"), " Copy to this phone"));
}

/** A bar that appears when something new is copied on the Mac (an agent's
 *  pbcopy or /copy, or a selection in Toranj), one tap to copy it here too.
 *  Returns the bar and its stopper. */
function clipBar(): [HTMLElement, () => void] {
  const preview = h("span", { class: "clip-text" });
  const bar = h("div", { class: "clipbar", hidden: true },
    h("button", { class: "clip-main", type: "button", onclick: () => clipSheet() },
      h("small", {}, "Copied on the Mac"), preview),
    h("button", { class: "btn primary small", type: "button", onclick: () => {
      if (!lastClip) return;
      copyText(lastClip.text);
      markClipSeen(lastClip.id);
      bar.hidden = true;
    } }, icon("copy"), " Copy"),
    h("button", { class: "icon-btn", type: "button", "aria-label": "Dismiss", onclick: () => { if (lastClip) markClipSeen(lastClip.id); bar.hidden = true; } }, icon("x")));
  const check = async () => {
    const c = await api<Clip>("/api/clipboard").catch(() => null);
    if (!c) return;
    lastClip = c;
    const fresh = c.text.trim() !== "" && c.id !== clipSeen();
    if (fresh) preview.textContent = c.text.trim().replace(/\s+/g, " ").slice(0, 140);
    bar.hidden = !fresh;
  };
  return [bar, poll(check, 3000)];
}

// ---------- attachments ----------
interface Att { name: string; size: number; thumb: string; path: string; failed: boolean }

/** Large photos are scaled down (and HEIC becomes JPEG) before upload:
 *  quicker over a phone connection and within what Claude reads. */
async function prepare(file: File): Promise<{ blob: Blob; name: string; thumb: string }> {
  let thumb = "";
  if (!file.type.startsWith("image/")) return { blob: file, name: file.name, thumb };
  try {
    const bmp = await createImageBitmap(file);
    const draw = (max: number) => {
      const k = Math.min(1, max / Math.max(bmp.width, bmp.height));
      const c = document.createElement("canvas");
      c.width = Math.round(bmp.width * k);
      c.height = Math.round(bmp.height * k);
      c.getContext("2d")!.drawImage(bmp, 0, 0, c.width, c.height);
      return c;
    };
    thumb = draw(120).toDataURL("image/jpeg", 0.7);
    const keep = /^image\/(png|jpeg|gif|webp)$/.test(file.type) && file.size < 1_500_000 && Math.max(bmp.width, bmp.height) <= 2000;
    if (keep) return { blob: file, name: file.name, thumb };
    const big = draw(2000);
    const blob = await new Promise<Blob | null>((r) => big.toBlob(r, "image/jpeg", 0.86));
    if (blob) return { blob, name: file.name.replace(/\.[^.]*$/, "") + ".jpg", thumb };
  } catch { /* not decodable here: send as is */ }
  return { blob: file, name: file.name, thumb };
}

async function upload(file: File): Promise<{ path: string; thumb: string; name: string }> {
  const p = await prepare(file);
  const res = await fetch(`/api/upload?name=${encodeURIComponent(p.name || "file")}`, {
    method: "POST",
    headers: { Authorization: `Bearer ${token()}`, "Content-Type": "application/octet-stream" },
    body: p.blob,
  });
  if (!res.ok) throw new Error((await res.text()) || "Upload failed");
  const { path } = await res.json();
  return { path, thumb: p.thumb, name: p.name };
}

// ---------- chat (a Claude conversation as messages) ----------
interface Msg { id: string; role: "user" | "assistant"; text: string; at: string }

/** Message text with ``` fences as code blocks, each with its own Copy. */
function msgBody(text: string): HTMLElement {
  const body = h("div", { class: "mtext" });
  const parts = text.split(/^```[^\n]*\n?/m);
  parts.forEach((part, i) => {
    if (i % 2 === 0) {
      if (part.trim()) body.append(h("div", { class: "para" }, part.replace(/^\n+|\n+$/g, "")));
    } else {
      const code = part.replace(/\n$/, "");
      body.append(h("div", { class: "code" },
        h("pre", {}, code),
        h("button", { class: "copy-code", type: "button", onclick: () => copyText(code, "Code copied") }, icon("copy"), " Copy")));
    }
  });
  return body;
}

function msgEl(m: Msg): HTMLElement {
  const time = m.at ? new Date(m.at) : null;
  return h("div", { class: `msg ${m.role}`, "data-id": m.id },
    msgBody(m.text),
    h("div", { class: "mfoot" },
      time && !isNaN(+time) ? h("small", {}, time.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" })) : null,
      h("button", { class: "copy-msg", type: "button", onclick: () => copyText(m.text) }, icon("copy"), " Copy")));
}

// ---------- one terminal ----------
const MODE_KEY = "toranj.mobile.mode";
const FONT_KEY = "toranj.mobile.font";
const pref = (k: string, d: string) => { try { return localStorage.getItem(k) ?? d; } catch { return d; } };
const setPref = (k: string, v: string) => { try { localStorage.setItem(k, v); } catch { /* private mode */ } };

function termView(name: string) {
  const gen = viewGen;
  const title = h("span", { class: "tt" }, "…");
  const sub = h("small", { class: "tsub" });
  const st = h("i", { class: "st" });
  const screen = h("pre", { class: "screen" });
  const chat = h("div", { class: "chat" });
  const working = h("div", { class: "working", hidden: true });
  const promptBox = h("div", { class: "prompt", hidden: true });
  const attBox = h("div", { class: "atts", hidden: true });
  const input = h("textarea", { rows: "1", placeholder: "Message", enterkeyhint: "send", autocapitalize: "sentences" });
  const sendBtn = h("button", { class: "send-btn", type: "button", "aria-label": "Send", disabled: true, onclick: () => void send() }, icon("send"));
  const fileIn = h("input", { type: "file", multiple: true, hidden: true, accept: "image/*,.pdf,.txt,.md,.json,.csv,.log,.zip,*/*" });
  const modeBtns = { chat: h("button", { type: "button" }, "Chat"), term: h("button", { type: "button" }, "Terminal") };
  const seg = h("div", { class: "seg", hidden: true }, modeBtns.chat, modeBtns.term);
  const [clipEl, stopClip] = clipBar();

  let tab: Tab | undefined;
  let mode: "chat" | "term" = pref(MODE_KEY, "chat") === "term" ? "term" : "chat";
  let canChat = false;
  let atts: Att[] = [];
  let uploading = 0;
  let sending = false;
  let fontPx = Math.max(9, Math.min(18, Number(pref(FONT_KEY, "12")) || 12));
  screen.style.fontSize = `${fontPx}px`;

  const refreshSend = () => { sendBtn.disabled = sending || uploading > 0 || (!input.value.trim() && !atts.some((a) => a.path)); };
  const drawAtts = () => {
    attBox.hidden = !atts.length;
    attBox.replaceChildren(...atts.map((a) => h("div", { class: "att" + (a.failed ? " bad" : a.path ? "" : " busy") },
      a.thumb ? h("img", { src: a.thumb, alt: "" }) : h("span", { class: "att-ext" }, (a.name.split(".").pop() || "file").slice(0, 4)),
      h("span", { class: "att-name" }, a.failed ? "Failed" : a.path ? a.name : "Uploading…"),
      h("button", { class: "att-x", type: "button", "aria-label": "Remove", onclick: () => { atts = atts.filter((x) => x !== a); drawAtts(); refreshSend(); } }, icon("x")))));
    refreshSend();
  };
  fileIn.addEventListener("change", () => {
    const files = [...(fileIn.files ?? [])].slice(0, 10 - atts.length);
    fileIn.value = "";
    for (const f of files) {
      const a: Att = { name: f.name, size: f.size, thumb: "", path: "", failed: false };
      atts.push(a);
      uploading++;
      upload(f).then((r) => { a.path = r.path; a.thumb = r.thumb; a.name = r.name; },
        (x) => { a.failed = true; toast(x instanceof Error ? x.message : String(x), true); })
        .finally(() => { uploading--; if (gen === viewGen) drawAtts(); });
    }
    drawAtts();
  });

  const send = async () => {
    const text = input.value.trim();
    const files = atts.filter((a) => a.path).map((a) => a.path);
    if ((!text && !files.length) || sending || uploading) return;
    sending = true;
    refreshSend();
    input.readOnly = true; // keeps the keyboard up, unlike disabled
    try {
      await api("/api/send", { name, text, files });
      input.value = "";
      atts = atts.filter((a) => !files.includes(a.path));
      drawAtts();
      autoGrow();
      stick = true;
      setTimeout(() => void refresh(), 400);
    } catch (x) { toast(x instanceof Error ? x.message : String(x), true); }
    input.readOnly = false;
    sending = false;
    refreshSend();
  };
  const autoGrow = () => { input.style.height = "auto"; input.style.height = `${Math.min(140, input.scrollHeight)}px`; };
  input.addEventListener("input", () => { autoGrow(); refreshSend(); });
  // Enter adds a line on a phone keyboard; Send sends. A hardware keyboard's
  // Enter (no on-screen keyboard to tap Send on) sends.
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter" && !e.shiftKey && !e.isComposing && matchMedia("(hover: hover)").matches) { e.preventDefault(); void send(); }
  });
  const key = (k: string, label: string, aria: string) => h("button", { class: "key", type: "button", "aria-label": aria, onclick: async () => {
    try { await api("/api/send", { name, key: k }); setTimeout(() => void refresh(), 250); }
    catch (x) { toast(x instanceof Error ? x.message : String(x), true); }
  } }, label);

  const close = async () => {
    const msg = tab?.background
      ? "Close this terminal? The background Claude session keeps running; stop it from the Mac."
      : "Close this terminal? Whatever runs in it is stopped.";
    if (!confirm(msg)) return;
    try { await api("/api/kill", { name }); go(""); }
    catch (x) { toast(x instanceof Error ? x.message : String(x), true); }
  };
  /** The terminal's text, plain and selectable, with Copy all. */
  const selectSheet = () => {
    const ta = h("textarea", { class: "sel-text mono", readonly: true });
    ta.value = (screen.textContent ?? "").replace(/[ \t]+$/gm, "");
    const shut = sheet("Terminal text", h("p", { class: "mute small" }, "Touch and hold to select a part, or copy all of it."), ta,
      h("button", { class: "btn primary big", type: "button", onclick: () => { copyText(ta.value); shut(); } }, icon("copy"), " Copy all"));
    requestAnimationFrame(() => { ta.scrollTop = ta.scrollHeight; });
  };
  const menuSheet = () => {
    const item = (label: string, fn: () => void, cls = "") => h("button", { class: `menu-item ${cls}`, type: "button", onclick: () => { shut(); fn(); } }, label);
    const size = h("div", { class: "menu-row" }, h("span", {}, "Terminal text size"),
      h("div", { class: "stepper" },
        h("button", { type: "button", "aria-label": "Smaller", onclick: () => setFont(fontPx - 1) }, "A−"),
        h("button", { type: "button", "aria-label": "Larger", onclick: () => setFont(fontPx + 1) }, "A+")));
    const shut = sheet("Terminal",
      item("Copy from the Mac clipboard", clipSheet),
      item("Select terminal text", () => { if (!lastText) void drawScreen(true).then(selectSheet); else selectSheet(); }),
      item("Attach files or photos", () => fileIn.click()),
      size,
      item("Close terminal", () => void close(), "danger"));
  };
  const setFont = (px: number) => {
    fontPx = Math.max(9, Math.min(18, px));
    screen.style.fontSize = `${fontPx}px`;
    setPref(FONT_KEY, String(fontPx));
  };

  app.replaceChildren(h("div", { class: "term" },
    h("header", { class: "term-top" },
      h("button", { class: "icon-btn", type: "button", "aria-label": "Back", onclick: () => go("") }, icon("back")),
      h("div", { class: "tt-wrap" }, h("div", { class: "tt-line" }, st, title), sub),
      h("button", { class: "icon-btn", type: "button", "aria-label": "More", onclick: menuSheet }, icon("more"))),
    h("div", { class: "seg-wrap" }, seg),
    h("div", { class: "body" }, chat, screen),
    h("div", { class: "dock" },
      clipEl,
      working,
      promptBox,
      h("div", { class: "keys" }, key("Escape", "esc", "Escape"), key("BTab", "⇧⇥", "Shift-Tab: change mode"), key("Up", "↑", "Up"), key("Down", "↓", "Down"), key("Enter", "⏎", "Enter"), key("C-c", "⌃C", "Control-C")),
      attBox,
      h("div", { class: "send" },
        h("button", { class: "icon-btn attach", type: "button", "aria-label": "Attach files or photos", onclick: () => fileIn.click() }, icon("clip")),
        input, sendBtn, fileIn),
    ),
  ));

  const setMode = (m: "chat" | "term", save = true) => {
    mode = m;
    if (save) setPref(MODE_KEY, m);
    const showChat = canChat && mode === "chat";
    chat.hidden = !showChat;
    screen.hidden = showChat;
    modeBtns.chat.classList.toggle("on", showChat);
    modeBtns.term.classList.toggle("on", !showChat);
    stick = true;
    void refresh();
  };
  modeBtns.chat.addEventListener("click", () => setMode("chat"));
  modeBtns.term.addEventListener("click", () => setMode("term"));

  // Stay at the bottom unless the user scrolled up to read.
  let stick = true;
  const scroller = () => (chat.hidden ? screen : chat);
  for (const el of [screen, chat]) el.addEventListener("scroll", () => { if (el === scroller()) stick = el.scrollHeight - el.scrollTop - el.clientHeight < 60; });
  const toBottom = () => { const el = scroller(); el.scrollTop = el.scrollHeight; };
  /** A selection being made in `el`: redrawing now would drop it. */
  const selecting = (el: HTMLElement) => {
    const s = document.getSelection();
    return !!s && !s.isCollapsed && s.rangeCount > 0 && el.contains(s.getRangeAt(0).commonAncestorContainer);
  };

  let lastText = "";
  const drawScreen = async (force = false) => {
    if (!force && (screen.hidden || selecting(screen))) return;
    try {
      const { text } = await api<{ text: string; cols: number }>(`/api/screen?name=${encodeURIComponent(name)}`);
      if (gen !== viewGen || (!force && selecting(screen))) return;
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

  let shown: Msg[] = [];
  const drawChat = async () => {
    if (chat.hidden || !tab?.session_id || selecting(chat)) return;
    let msgs = await api<Msg[]>(`/api/messages?session=${encodeURIComponent(tab.session_id)}&cwd=${encodeURIComponent(tab.cwd)}`).catch((x) => x as Error);
    if (gen !== viewGen || selecting(chat)) return;
    // A new conversation has no transcript yet.
    if (msgs instanceof Error) { if (shown.length || !(msgs instanceof ApiError)) return; msgs = []; }
    if (!msgs.length && !shown.length) {
      chat.replaceChildren(h("div", { class: "chat-empty mute" }, "No messages yet. Write below to start."));
      return;
    }
    // Only what changed is redrawn, so a long press on an older message isn't interrupted.
    const same = (a: Msg, b: Msg) => a.id === b.id && a.text === b.text;
    // The oldest messages drop off the list as new ones come: remove those first.
    const ids = new Set(msgs.map((m) => m.id));
    while (shown.length && !ids.has(shown[0].id)) {
      shown = shown.slice(1);
      chat.querySelector(".msg")?.remove();
    }
    let keep = 0;
    while (keep < shown.length && keep < msgs.length && same(shown[keep], msgs[keep])) keep++;
    if (keep === shown.length && keep === msgs.length) return;
    if (keep === 0 || chat.querySelector(".chat-empty")) chat.replaceChildren();
    const els = [...chat.querySelectorAll(".msg")];
    els.slice(keep).forEach((e) => e.remove());
    chat.append(...msgs.slice(keep).map(msgEl));
    const grew = msgs.length > shown.length;
    shown = msgs;
    if (stick || grew) toBottom();
  };

  let lastPeekKey = "";
  const drawState = async () => {
    const snap = await api<Snapshot>("/api/snapshot").catch(() => null);
    if (gen !== viewGen || !snap) return;
    tab = snap.tabs.find((t) => t.name === name);
    if (!tab) { toast("This terminal was closed."); go(""); return; }
    title.textContent = tabTitle(tab);
    sub.textContent = `${base(tab.project || tab.cwd)} · ${LABEL[tab.state]}`;
    st.className = `st ${tab.state}`;
    const chatNow = tab.agent === "claude" && !!tab.session_id;
    if (chatNow !== canChat) { canChat = chatNow; seg.hidden = !canChat; setMode(mode, false); }
    // In chat, what the terminal shows meanwhile (Claude at work) is a line under the messages.
    working.hidden = !(canChat && mode === "chat" && tab.state === "running");
    if (!working.hidden) working.replaceChildren(h("i", { class: "st running" }), h("span", {}, tab.detail || "Working…"));
    if (tab.state === "waiting") {
      const k = `${tab.since}:${tab.detail}`;
      if (k !== lastPeekKey || promptBox.hidden) {
        lastPeekKey = k;
        const p = await peekFor(tab);
        if (gen !== viewGen) return;
        if (p?.prompt) {
          promptBox.replaceChildren(
            h("pre", { class: "ctx" }, p.prompt.context.join("\n")),
            choicesEl(tab, p.prompt, () => { promptBox.hidden = true; void refresh(); }));
          promptBox.hidden = false;
        }
      }
    } else {
      promptBox.hidden = true;
      lastPeekKey = "";
    }
  };
  const refresh = () => Promise.all([drawScreen(), drawChat()]);

  // Keep the input above the iPhone keyboard: size the view to what's visible.
  const termEl = app.querySelector<HTMLElement>(".term")!;
  const fit = () => {
    const vv = window.visualViewport;
    if (vv) { termEl.style.height = `${vv.height}px`; termEl.style.transform = `translateY(${vv.offsetTop}px)`; }
    if (stick) toBottom();
  };
  window.visualViewport?.addEventListener("resize", fit);
  window.visualViewport?.addEventListener("scroll", fit);
  // The dock grows (a question, attachments): keep the latest in view.
  const ro = new ResizeObserver(() => { if (stick) toBottom(); });
  ro.observe(app.querySelector(".body")!);
  setMode(mode, false);
  fit();
  const s1 = poll(drawScreen, 1200);
  const s2 = poll(drawState, 2500);
  const s3 = poll(drawChat, 2000);
  stopView = () => { s1(); s2(); s3(); stopClip(); ro.disconnect(); window.visualViewport?.removeEventListener("resize", fit); window.visualViewport?.removeEventListener("scroll", fit); };
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
    h("header", { class: "top" }, h("button", { class: "icon-btn", type: "button", "aria-label": "Back", onclick: () => go("") }, icon("back")), h("h1", {}, "New terminal"), h("span", { class: "spacer" })),
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
