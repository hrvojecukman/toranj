// Reading Claude Code's screen: where its input box, completion list,
// status line and the lines under the box (mode, background tasks, agents)
// are, and the dialogs it shows. Works on the screen's plain text lines,
// so the desktop (xterm buffer) and the phone (tmux capture) share it.

const rule = (l: string) => { const t = l.trim(); return t.length >= 10 && [...t].filter((c) => c === "─" || c === "━").length >= t.length * 0.6; };

export interface Edges {
  /** The status line above the box ("Catapulting… (4m 30s · 639 tokens)"). */
  status: string;
  /** What's in Claude's input (a suggested prompt shows there, dimmed). */
  input: string;
  /** Line indexes, [from, to): the input box, the completion list, the lines under the box. */
  inputLines: [number, number];
  menuLines: [number, number];
  footLines: number[];
}

export function screenEdges(text: string[]): Edges | null {
  // The rule under Claude's input box: the last one on screen.
  let r2 = -1;
  for (let i = text.length - 1; i >= 0; i--) if (rule(text[i])) { r2 = i; break; }
  if (r2 < 0) return null;
  // The input starts at the last prompt line above it (❯, or ! in shell mode).
  const PROMPT = /^\s*[>❯!]\s?/;
  let p = -1;
  if (PROMPT.test(text[r2 + 1] ?? "")) {
    // Only a top rule (shell mode draws none below): the box runs from it
    // to the first blank line; what follows is the footer.
    p = r2 + 1;
    let end = p + 1;
    while (end < text.length && text[end].trim() && !rule(text[end])) end++;
    r2 = end;
  } else {
    for (let i = r2 - 1; i >= Math.max(0, r2 - 12); i--) {
      if (rule(text[i])) break;
      if (PROMPT.test(text[i])) { p = i; break; }
    }
  }
  if (p < 0) return null;
  const input = text.slice(p, r2).map((l, i) => (i === 0 ? l.replace(/^\s*[>❯›!]\s?/, "") : l.replace(/^\s{0,2}/, ""))).join("\n").trim();
  // While Claude offers completions (slash commands, @ files) they list
  // right above the box's top rule, the chosen one marked ❯ (indented).
  let r1 = p - 1;
  let top = r1;
  // (Without a top rule the box isn't Claude's usual one: no menu then.)
  if (r1 >= 0 && rule(text[r1])) for (let j = r1 - 1; j >= Math.max(0, r1 - 16); j--) {
    if (!text[j].trim() || rule(text[j])) break;
    top = j;
  }
  let menuLines: [number, number] = [0, 0];
  if (top < r1 && text.slice(top, r1).some((l) => /^\s{1,6}❯\s/.test(l))) {
    menuLines = [top, r1];
    r1 = top;
  }
  let status = "";
  for (let i = r1 - 1; i >= Math.max(0, r1 - 3); i--) {
    const l = text[i].trim();
    if (!l) continue;
    // A spinner or note line starts with a symbol: ✻ · * ✶ ✳ ✢ ⎿ (not ⏺, Claude's reply bullet).
    if (/^[^\p{L}\p{N}\s⏺❯>]\s/u.test(l)) status = l.replace(/^\S+\s+/, "").replace(/\s{3,}.*$/, "");
    break;
  }
  const footLines: number[] = [];
  let last = text.length - 1;
  while (last > r2 && !text[last].trim()) last--;
  for (let i = r2 + 1; i <= last; i++) if (text[i].trim()) footLines.push(i);
  return { status, input, inputLines: [p, r2], menuLines, footLines };
}

export interface Dialog {
  /** Line indexes, [from, to), of the dialog. */
  lines: [number, number];
  options: { n: string; label: string }[];
  keys: [string, string, string][];
}

/**
 * A question Claude shows that isn't a plain prompt (multiple choice, free
 * text, /usage…): its lines from the last rule above it, its numbered
 * options, and the keys its hint line names.
 */
export function screenDialog(text: string[]): Dialog {
  let last = text.length - 1;
  while (last > 0 && !text[last].trim()) last--;
  let start = Math.max(0, last - 24);
  for (let i = last - 1; i >= Math.max(0, last - 40); i--) if (rule(text[i])) { start = i + 1; break; }
  const options: { n: string; label: string }[] = [];
  for (let i = start; i <= last; i++) {
    const m = /^\s{0,3}(?:[❯›>]\s*)?(\d{1,2})\.\s+(.+?)\s*$/.exec(text[i]);
    if (m && !options.some((o) => o.n === m[1])) options.push({ n: m[1], label: m[2] });
  }
  return { lines: [start, last + 1], options, keys: dialogKeys(text.slice(start, last + 1).join("\n")) };
}

/**
 * Keys for a dialog: those its hint line names ("←/→ to switch · f to fix ·
 * Esc to close"), else ↑ ↓ Enter Esc. Each is [label, what to send, what it does].
 */
export function dialogKeys(screen: string): [string, string, string][] {
  const NAMED: Record<string, string> = {
    "←": "\x1b[D", "→": "\x1b[C", "↑": "\x1b[A", "↓": "\x1b[B", esc: "\x1b", enter: "\r", "⏎": "\r", return: "\r",
    tab: "\t", space: " ", "shift+tab": "\x1b[Z", "⇧tab": "\x1b[Z",
  };
  const out: [string, string, string][] = [];
  const seen = new Set<string>();
  const add = (k: string, title: string) => {
    const key = k.trim();
    const data = NAMED[key.toLowerCase()] ?? (/^[a-z0-9]$/i.test(key) ? key : /^ctrl\+([a-z])$/i.test(key) ? String.fromCharCode(key.slice(-1).toLowerCase().charCodeAt(0) - 96) : "");
    if (!data || seen.has(data)) return;
    seen.add(data);
    out.push([key.length === 1 ? key : key[0].toUpperCase() + key.slice(1), data, title]);
  };
  // The hint line is the last one with "… to …" parts joined by "·".
  const hint = screen.split("\n").reverse().find((l) => /\S+ to \S/.test(l) && l.includes("·"));
  for (const part of hint?.split("·") ?? []) {
    const m = /^\s*(\S+(?:\s?\/\s?\S+)*)\s+to\s+(.+?)\s*$/.exec(part);
    if (!m) continue;
    for (const k of m[1].split("/")) add(k, m[2]);
  }
  if (!out.length) for (const [k, t] of [["↑", "Up"], ["↓", "Down"], ["Enter", "Choose the highlighted option"], ["Esc", "Cancel"]]) add(k, t);
  return out;
}
