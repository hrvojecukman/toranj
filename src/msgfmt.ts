// A Claude message (Markdown) as DOM, built only with textContent: code
// blocks and quotes become boxes with their own Copy, so a message Claude
// drafted for someone else can be copied in one click. Shared by the
// desktop's Messages drawer and the phone's Chat view.

export type CopyFn = (text: string, what: string) => void;

const ns = "http://www.w3.org/2000/svg";
export function copyIcon(): SVGSVGElement {
  const svg = document.createElementNS(ns, "svg");
  svg.setAttribute("viewBox", "0 0 24 24");
  svg.setAttribute("class", "ic ic-copy");
  svg.setAttribute("aria-hidden", "true");
  for (const d of ["M9 9h10v12H9z", "M5 15V3h10"]) {
    const p = document.createElementNS(ns, "path");
    p.setAttribute("d", d);
    svg.append(p);
  }
  return svg;
}

function el<K extends keyof HTMLElementTagNameMap>(tag: K, cls: string, ...kids: (Node | string)[]): HTMLElementTagNameMap[K] {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  e.append(...kids);
  return e;
}

export function copyButton(text: () => string, copy: CopyFn, what: string, cls: string, label = "Copy"): HTMLButtonElement {
  const b = el("button", cls, copyIcon(), label ? ` ${label}` : "");
  b.type = "button";
  b.title = `Copy ${what.toLowerCase()}`;
  b.addEventListener("click", (e) => { e.stopPropagation(); copy(text(), `${what} copied`); });
  return b;
}

/** `code` and **bold** inside a line. */
function inline(text: string): DocumentFragment {
  const f = document.createDocumentFragment();
  const re = /`([^`\n]+)`|\*\*([^*\n]+)\*\*/g;
  let at = 0;
  for (let m = re.exec(text); m; m = re.exec(text)) {
    if (m.index > at) f.append(text.slice(at, m.index));
    f.append(m[1] !== undefined ? el("code", "", m[1]) : el("b", "", m[2]));
    at = m.index + m[0].length;
  }
  if (at < text.length) f.append(text.slice(at));
  return f;
}

/** Plain text of a block, as it would be pasted: Markdown marks removed. */
const plain = (lines: string[]) => lines.join("\n").replace(/\*\*([^*\n]+)\*\*/g, "$1").replace(/`([^`\n]+)`/g, "$1").trim();

/**
 * Renders `text`. With `paraCopy`, every paragraph also gets a Copy
 * (shown on hover); code blocks and quotes always have one.
 */
export function renderMessage(text: string, copy: CopyFn, paraCopy = false): HTMLElement {
  const body = el("div", "mtext");
  const lines = text.replace(/\r/g, "").split("\n");
  let para: string[] = [];
  const flushPara = () => {
    while (para.length && !para[para.length - 1].trim()) para.pop();
    if (!para.length) return;
    const lines = para;
    const p = el("div", "para");
    lines.forEach((l, i) => {
      const h = /^#{1,6}\s+(.*)$/.exec(l);
      p.append(h ? el("b", "", h[1]) : inline(l));
      if (i < lines.length - 1) p.append("\n");
    });
    if (paraCopy && lines.join("").trim()) p.append(copyButton(() => plain(lines), copy, "Paragraph", "copy-para", ""));
    body.append(p);
    para = [];
  };
  for (let i = 0; i < lines.length; i++) {
    const line = lines[i];
    const fence = /^\s*```/.exec(line);
    if (fence) {
      flushPara();
      const code: string[] = [];
      for (i++; i < lines.length && !/^\s*```\s*$/.test(lines[i]); i++) code.push(lines[i]);
      const src = code.join("\n");
      body.append(el("div", "code", el("pre", "", src), copyButton(() => src, copy, "Code", "copy-block")));
      continue;
    }
    if (/^\s*>/.test(line)) {
      flushPara();
      const q: string[] = [];
      for (; i < lines.length && /^\s*>/.test(lines[i]); i++) q.push(lines[i].replace(/^\s*>\s?/, ""));
      i--;
      const box = el("div", "quote");
      q.forEach((l, j) => { box.append(inline(l)); if (j < q.length - 1) box.append("\n"); });
      body.append(el("div", "quote-wrap", box, copyButton(() => plain(q), copy, "Quote", "copy-block")));
      continue;
    }
    if (!line.trim()) { flushPara(); continue; }
    para.push(line);
  }
  flushPara();
  return body;
}

// ---------- conversation items ----------

/** A message, or a tool call Claude made (role "tool"). */
export interface Item {
  id: string;
  role: "user" | "assistant" | "tool";
  text: string;
  at: string;
  tool?: string;
  detail?: string;
  /** A tool's output; absent while it runs. */
  result?: string;
  /** Sent while Claude was busy; waiting in Claude Code's queue. */
  queued?: boolean;
}

const sig = (m: Item) => `${m.id}|${m.text}|${m.queued ? "q" : ""}|${m.result === undefined ? "…" : m.result.length}`;

/** Elapsed time since an ISO timestamp, short: "45 s", "3 min 52 s". */
export function since(iso: string): string {
  const s = Math.max(0, Math.round((Date.now() - Date.parse(iso)) / 1000));
  if (!Number.isFinite(s)) return "";
  if (s < 60) return `${s} s`;
  if (s < 3600) return `${Math.floor(s / 60)} min ${s % 60} s`;
  return `${Math.floor(s / 3600)} h ${Math.floor((s % 3600) / 60)} min`;
}

/** A tool call as one line (what it does); opened, its input and output. */
export function renderTool(m: Item, copy: CopyFn): HTMLElement {
  const running = m.result === undefined;
  const d = el("details", `citem tcall${running ? " running" : ""}`);
  d.dataset.id = m.id;
  const state = el("span", "tstate", running ? "running" : "");
  d.append(el("summary", "", el("span", "tname", m.tool ?? "tool"), el("span", "ttitle", m.text), state));
  const body = el("div", "tbody");
  if (m.detail) body.append(el("div", "tsec", el("small", "", "Input"), el("pre", "", m.detail), copyButton(() => m.detail ?? "", copy, "Input", "copy-block")));
  if (m.result) body.append(el("div", "tsec", el("small", "", "Output"), el("pre", "", m.result), copyButton(() => m.result ?? "", copy, "Output", "copy-block")));
  else if (running) body.append(el("p", "tnote", "Still running…"));
  d.append(body);
  return d;
}

/** The tool call that is still running, newest first. */
export function runningTool(items: Item[]): Item | undefined {
  for (let i = items.length - 1; i >= 0; i--) {
    const m = items[i];
    if (m.role === "tool") { if (m.result === undefined) return m; }
    else if (m.role === "assistant" || m.role === "user") return undefined;
  }
  return undefined;
}

/**
 * Brings `list` from `shown` to `items`, touching only what changed: old
 * items falling off the front are removed, unchanged ones are kept (so a
 * selection, scroll position or an opened tool call stays). Every rendered
 * element must have class "citem" and data-id. Returns the new `shown`.
 */
export function syncItems(list: HTMLElement, shown: Item[], items: Item[], render: (m: Item) => HTMLElement): Item[] {
  const els = () => [...list.querySelectorAll<HTMLElement>(":scope > .citem")];
  if (!shown.length) list.replaceChildren();
  const ids = new Set(items.map((m) => m.id));
  shown = [...shown];
  while (shown.length && !ids.has(shown[0].id)) { shown.shift(); els()[0]?.remove(); }
  let keep = 0;
  while (keep < shown.length && keep < items.length && sig(shown[keep]) === sig(items[keep])) keep++;
  if (keep === shown.length && keep === items.length) return shown;
  const open = new Set<string>();
  for (const e of els().slice(keep)) { if ((e as HTMLDetailsElement).open) open.add(e.dataset.id ?? ""); e.remove(); }
  for (const m of items.slice(keep)) {
    const e = render(m);
    if (open.has(m.id) && e instanceof HTMLDetailsElement) e.open = true;
    list.append(e);
  }
  return items;
}
