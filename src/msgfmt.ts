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
