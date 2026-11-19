// Terminal text with SGR color escapes (tmux capture-pane -e) to safe DOM
// nodes. Only colors and bold/dim/italic/underline/inverse are kept; every
// other escape sequence is dropped. Text is only ever set as textContent.

const BASE = [
  "#3b4252", "#ef6a5f", "#4cc282", "#e5c07b", "#6d9cf0", "#c678dd", "#56b6c2", "#d8dee9",
  "#5c6370", "#ff7b72", "#7ee2a8", "#f2d58f", "#8fb4ff", "#d7a1ec", "#7fd4df", "#ffffff",
];

function xterm256(n: number): string {
  if (n < 16) return BASE[n];
  if (n < 232) {
    const i = n - 16;
    const v = (c: number) => (c === 0 ? 0 : 55 + c * 40);
    return `rgb(${v(Math.floor(i / 36))},${v(Math.floor(i / 6) % 6)},${v(i % 6)})`;
  }
  const g = 8 + (n - 232) * 10;
  return `rgb(${g},${g},${g})`;
}

interface Style { fg: string; bg: string; bold: boolean; dim: boolean; italic: boolean; under: boolean; inverse: boolean }
const plain = (): Style => ({ fg: "", bg: "", bold: false, dim: false, italic: false, under: false, inverse: false });

function apply(st: Style, params: number[]) {
  if (!params.length) params = [0];
  for (let i = 0; i < params.length; i++) {
    const p = params[i];
    if (p === 0) Object.assign(st, plain());
    else if (p === 1) st.bold = true;
    else if (p === 2) st.dim = true;
    else if (p === 3) st.italic = true;
    else if (p === 4) st.under = true;
    else if (p === 7) st.inverse = true;
    else if (p === 22) { st.bold = false; st.dim = false; }
    else if (p === 23) st.italic = false;
    else if (p === 24) st.under = false;
    else if (p === 27) st.inverse = false;
    // White text stays the theme's text color, so it shows on a light screen.
    else if (p >= 30 && p <= 37) st.fg = p === 37 ? "" : BASE[p - 30];
    else if (p >= 90 && p <= 97) st.fg = p === 97 ? "" : BASE[p - 90 + 8];
    else if (p === 39) st.fg = "";
    else if (p >= 40 && p <= 47) st.bg = BASE[p - 40];
    else if (p >= 100 && p <= 107) st.bg = BASE[p - 100 + 8];
    else if (p === 49) st.bg = "";
    else if (p === 38 || p === 48) {
      let c = "";
      if (params[i + 1] === 5) { c = xterm256((params[i + 2] ?? 0) & 255); i += 2; }
      else if (params[i + 1] === 2) {
        const [r, g, b] = [params[i + 2], params[i + 3], params[i + 4]].map((x) => Math.max(0, Math.min(255, x ?? 0)));
        c = `rgb(${r},${g},${b})`;
        i += 4;
      }
      if (p === 38) st.fg = c; else st.bg = c;
    }
  }
}

/** SGR parameters. Colon sub-parameters (e.g. 4:3 curly underline, or
 *  38:2::r:g:b) belong to their parameter: colors are expanded, anything
 *  else keeps only its first number. */
function sgrParams(body: string): number[] {
  const out: number[] = [];
  for (const part of body.split(";")) {
    if (part === "") { out.push(0); continue; }
    if (!part.includes(":")) { out.push(parseInt(part, 10) || 0); continue; }
    const sub = part.split(":").map((x) => parseInt(x, 10) || 0);
    if ((sub[0] === 38 || sub[0] === 48) && sub[1] === 2) out.push(sub[0], 2, ...sub.slice(-3));
    else if ((sub[0] === 38 || sub[0] === 48) && sub[1] === 5) out.push(sub[0], 5, sub[2] ?? 0);
    else out.push(sub[0]);
  }
  return out;
}

/** Renders `text` into `into` (replacing its children). */
export function renderAnsi(text: string, into: HTMLElement) {
  const frag = document.createDocumentFragment();
  const st = plain();
  let buf = "";
  const flush = () => {
    if (!buf) return;
    const plainStyle = !st.fg && !st.bg && !st.bold && !st.dim && !st.italic && !st.under && !st.inverse;
    if (plainStyle) frag.append(buf);
    else {
      const s = document.createElement("span");
      s.textContent = buf;
      const fg = st.inverse ? st.bg || "var(--term)" : st.fg;
      const bg = st.inverse ? st.fg || "var(--fg)" : st.bg;
      if (fg) s.style.color = fg;
      if (bg) s.style.background = bg;
      if (st.bold) s.style.fontWeight = "700";
      if (st.dim) s.style.opacity = ".6";
      if (st.italic) s.style.fontStyle = "italic";
      if (st.under) s.style.textDecoration = "underline";
      frag.append(s);
    }
    buf = "";
  };
  for (let i = 0; i < text.length; i++) {
    const ch = text[i];
    if (ch !== "\x1b") {
      // Keep newlines and tabs; drop other control characters.
      if (ch === "\n" || ch === "\t" || ch >= " ") buf += ch;
      continue;
    }
    const next = text[i + 1];
    if (next === "[") {
      let j = i + 2;
      while (j < text.length && !/[@-~]/.test(text[j])) j++;
      const body = text.slice(i + 2, j);
      if (text[j] === "m") {
        flush();
        apply(st, sgrParams(body));
      }
      i = j;
    } else if (next === "]") {
      // OSC (e.g. hyperlinks): skip to BEL or ESC \
      let j = i + 2;
      while (j < text.length && text[j] !== "\x07" && !(text[j] === "\x1b" && text[j + 1] === "\\")) j++;
      i = text[j] === "\x07" ? j : j + 1;
    } else if (next && "()*+".includes(next)) {
      i += 2; // charset selection, e.g. ESC ( B
    } else {
      i += 1;
    }
  }
  flush();
  into.replaceChildren(frag);
}
