// A small markdown reader for text the pond wrote, which becomes React elements and never HTML.
// The app has no markdown package, so this takes the parts a result uses and leaves the rest as text.

export type Inline =
  | { t: "text"; v: string }
  | { t: "code"; v: string }
  | { t: "strong"; c: Inline[] }
  | { t: "em"; c: Inline[] }
  | { t: "del"; c: Inline[] }
  | { t: "link"; href: string; c: Inline[] };

export type Align = "left" | "center" | "right" | null;

/** One entry per item of a list: `true` or `false` for a task item (`[x]`, `[ ]`), `null` for any other. */
export type Tasks = (boolean | null)[];

export type Block =
  | { t: "p"; c: Inline[] }
  | { t: "h"; level: number; c: Inline[] }
  | { t: "ul"; items: Block[][]; tasks?: Tasks }
  | { t: "ol"; start: number; items: Block[][]; tasks?: Tasks }
  | { t: "table"; align: Align[]; head: Inline[][]; rows: Inline[][][] }
  | { t: "quote"; c: Block[] }
  | { t: "code"; lang: string; v: string }
  | { t: "hr" };

/** Only links that leave the app for a web page or a mail client; anything else is shown as its text. */
const SAFE_LINK = /^(https?:\/\/|mailto:)/i;

export function isWebLink(url: string): boolean {
  return SAFE_LINK.test(url);
}

const ESCAPABLE = "\\`*_{}[]()#+-.!|<>~";
const LINK = /^\[((?:[^[\]\\]|\\.)*)\]\(([^)\s]*)(?:\s+"[^"]*")?\)/;
const BARE_URL = /https?:\/\/[^\s<>]+/iy;
const ANGLE_URL = /<(https?:\/\/[^\s<>]+)>/iy;
const MAX_DEPTH = 8;

function isAlnum(ch: string | undefined): boolean {
  return ch !== undefined && /[A-Za-z0-9]/.test(ch);
}

/** The next single `ch` that can close an emphasis, skipping doubled ones and ones that follow a space. */
function closeOf(s: string, from: number, ch: string): number {
  for (let j = from; j < s.length; j++) {
    if (s[j] !== ch) continue;
    if (s[j + 1] === ch) {
      j++;
      continue;
    }
    if (s[j - 1] !== " ") return j;
  }
  return -1;
}

/** A bare address ends before the punctuation a sentence puts after it, and before a bracket it did not open. */
function trimUrl(url: string): string {
  let end = url.length;
  while (end > 0) {
    const last = url[end - 1];
    if (".,;:!?'\"*_~".includes(last)) end--;
    else if (last === ")" && url.slice(0, end).split(")").length > url.slice(0, end).split("(").length) end--;
    else break;
  }
  return url.slice(0, end);
}

/** The next `~~` that can close a strikethrough, skipping ones that follow a space or run into a third tilde. */
function closeStrike(s: string, from: number): number {
  let j = s.indexOf("~~", from);
  while (j !== -1 && (s[j - 1] === " " || s[j + 2] === "~")) j = s.indexOf("~~", j + 1);
  return j;
}

/** `inLink` is set inside a link's text, where an address must not become a second link. */
export function parseInline(s: string, inLink = false): Inline[] {
  const out: Inline[] = [];
  let text = "";
  const flush = () => {
    if (text !== "") {
      out.push({ t: "text", v: text });
      text = "";
    }
  };
  let i = 0;
  while (i < s.length) {
    const c = s[i];
    if (c === "\\" && i + 1 < s.length && ESCAPABLE.includes(s[i + 1])) {
      text += s[i + 1];
      i += 2;
      continue;
    }
    if (c === "`") {
      const close = s.indexOf("`", i + 1);
      if (close > i + 1) {
        flush();
        out.push({ t: "code", v: s.slice(i + 1, close) });
        i = close + 1;
        continue;
      }
    }
    if (c === "!" && s[i + 1] === "[") {
      const image = LINK.exec(s.slice(i + 1));
      if (image) {
        text += image[1];
        i += 1 + image[0].length;
        continue;
      }
    }
    if (c === "[") {
      const link = LINK.exec(s.slice(i));
      if (link) {
        flush();
        const inner = parseInline(link[1], true);
        if (SAFE_LINK.test(link[2])) out.push({ t: "link", href: link[2], c: inner });
        else out.push(...inner);
        i += link[0].length;
        continue;
      }
    }
    if (!inLink && c === "<") {
      ANGLE_URL.lastIndex = i;
      const angle = ANGLE_URL.exec(s);
      if (angle) {
        flush();
        out.push({ t: "link", href: angle[1], c: [{ t: "text", v: angle[1] }] });
        i += angle[0].length;
        continue;
      }
    }
    if (!inLink && (c === "h" || c === "H") && !isAlnum(s[i - 1])) {
      BARE_URL.lastIndex = i;
      const bare = BARE_URL.exec(s);
      if (bare) {
        const url = trimUrl(bare[0]);
        if (/^https?:\/\/./i.test(url)) {
          flush();
          out.push({ t: "link", href: url, c: [{ t: "text", v: url }] });
          i += url.length;
          continue;
        }
      }
    }
    if (c === "~" && s[i + 1] === "~" && s[i + 2] !== "~" && s[i + 2] !== " " && s[i + 2] !== undefined) {
      const close = closeStrike(s, i + 2);
      if (close > i + 2) {
        flush();
        out.push({ t: "del", c: parseInline(s.slice(i + 2, close), inLink) });
        i = close + 2;
        continue;
      }
    }
    if ((c === "*" || c === "_") && s[i + 1] === c && (c === "*" || !isAlnum(s[i - 1]))) {
      const close = s.indexOf(c + c, i + 2);
      if (close > i + 2) {
        flush();
        out.push({ t: "strong", c: parseInline(s.slice(i + 2, close), inLink) });
        i = close + 2;
        continue;
      }
    }
    if ((c === "*" || c === "_") && s[i + 1] !== c && s[i + 1] !== " " && s[i + 1] !== undefined && (c === "*" || !isAlnum(s[i - 1]))) {
      const close = closeOf(s, i + 1, c);
      if (close > i + 1 && (c === "*" || !isAlnum(s[close + 1]))) {
        flush();
        out.push({ t: "em", c: parseInline(s.slice(i + 1, close), inLink) });
        i = close + 1;
        continue;
      }
    }
    text += c;
    i++;
  }
  flush();
  return out;
}

const FENCE = /^ {0,3}(`{3,}|~{3,})\s*([\w+-]*)\s*$/;
const HEADING = /^ {0,3}(#{1,6})\s+(.*?)(?:\s+#+)?\s*$/;
const HR = /^ {0,3}([-*_])(?:\s*\1){2,}\s*$/;
const QUOTE = /^ {0,3}>\s?(.*)$/;
const LIST = /^( {0,3})([-*+]|\d{1,9}[.)])\s+(.*)$/;
const TASK = /^\[([ xX])\](?:\s+|$)/;
const DELIMITER_CELL = /^:?-+:?$/;

function indentOf(line: string): number {
  let n = 0;
  for (const ch of line) {
    if (ch === " ") n++;
    else if (ch === "\t") n += 4;
    else break;
  }
  return n;
}

function closesFence(line: string, marker: string, length: number): boolean {
  const t = line.trim();
  return t.length >= length && [...t].every((ch) => ch === marker);
}

/** Whether `line` opens something other than a paragraph; a paragraph is only cut by a list that starts at 1 or is a bullet. */
function startsBlock(line: string, insideParagraph: boolean): boolean {
  if (FENCE.test(line) || HEADING.test(line) || HR.test(line) || QUOTE.test(line)) return true;
  const list = LIST.exec(line);
  if (!list) return false;
  return !insideParagraph || !/\d/.test(list[2]) || parseInt(list[2], 10) === 1;
}

/** The cells of a table row, split at pipes that are not escaped, or `null` for a line with no pipe. */
function splitRow(line: string): string[] | null {
  let t = line.trim();
  if (!t.includes("|")) return null;
  if (t.startsWith("|")) t = t.slice(1);
  if (t.endsWith("|") && !t.endsWith("\\|")) t = t.slice(0, -1);
  const cells: string[] = [];
  let cell = "";
  for (let i = 0; i < t.length; i++) {
    if (t[i] === "\\" && t[i + 1] === "|") {
      cell += "|";
      i++;
    } else if (t[i] === "|") {
      cells.push(cell.trim());
      cell = "";
    } else {
      cell += t[i];
    }
  }
  cells.push(cell.trim());
  return cells;
}

/** The alignments of a table's delimiter row (`| --- | :-: |`), or `null` when the line is not one. */
function delimiterOf(line: string | undefined): Align[] | null {
  const cells = line === undefined ? null : splitRow(line);
  if (!cells || !cells.every((c) => DELIMITER_CELL.test(c))) return null;
  return cells.map((c): Align => {
    const left = c.startsWith(":");
    const right = c.endsWith(":");
    return left && right ? "center" : right ? "right" : left ? "left" : null;
  });
}

/** A table opens only at a header row followed by a delimiter row with as many columns. */
function parseTable(lines: string[], start: number): { block: Block; next: number } | null {
  const head = splitRow(lines[start]);
  const align = delimiterOf(lines[start + 1]);
  if (!head || !align || head.length !== align.length) return null;
  const rows: Inline[][][] = [];
  let i = start + 2;
  while (i < lines.length && lines[i].trim() !== "" && !startsBlock(lines[i], false)) {
    const cells = splitRow(lines[i]);
    if (!cells) break;
    rows.push(align.map((_, col) => parseInline(cells[col] ?? "")));
    i++;
  }
  return { block: { t: "table", align, head: head.map((c) => parseInline(c)), rows }, next: i };
}

/** Whether an item opens with `[ ]` or `[x]`; the marker is taken off its text, which it is not part of. */
function taskOf(item: Block[]): boolean | null {
  const first = item[0];
  const lead = first?.t === "p" ? first.c[0] : undefined;
  if (!first || first.t !== "p" || lead?.t !== "text") return null;
  const marker = TASK.exec(lead.v);
  if (!marker) return null;
  const rest = lead.v.slice(marker[0].length);
  if (rest === "") first.c.shift();
  else lead.v = rest;
  return marker[1] !== " ";
}

function parseList(lines: string[], start: number, depth: number): { block: Block; next: number } {
  const first = LIST.exec(lines[start]) as RegExpExecArray;
  const baseIndent = first[1].length;
  const ordered = /\d/.test(first[2]);
  const startNumber = ordered ? parseInt(first[2], 10) : 1;
  const items: Block[][] = [];
  const tasks: Tasks = [];
  const sibling = (line: string | undefined): boolean => {
    const m = line === undefined ? null : LIST.exec(line);
    return m !== null && m[1].length === baseIndent && /\d/.test(m[2]) === ordered;
  };
  let i = start;
  while (i < lines.length && sibling(lines[i])) {
    const m = LIST.exec(lines[i]) as RegExpExecArray;
    const markerWidth = m[1].length + m[2].length + 1;
    const itemLines = [m[3]];
    i++;
    while (i < lines.length) {
      const l = lines[i];
      if (l.trim() === "") {
        let j = i;
        while (j < lines.length && lines[j].trim() === "") j++;
        if (j < lines.length && indentOf(lines[j]) > baseIndent) {
          itemLines.push("");
          i++;
          continue;
        }
        break;
      }
      const indent = indentOf(l);
      if (indent > baseIndent) {
        itemLines.push(l.slice(Math.min(indent, markerWidth)));
        i++;
        continue;
      }
      if (LIST.test(l) || startsBlock(l, true) || itemLines[itemLines.length - 1] === "") break;
      itemLines.push(l.trim());
      i++;
    }
    const item = parseBlocks(itemLines, depth + 1);
    tasks.push(taskOf(item));
    items.push(item);
    let j = i;
    while (j < lines.length && lines[j].trim() === "") j++;
    if (sibling(lines[j])) i = j;
    else break;
  }
  const marked = tasks.some((t) => t !== null) ? { tasks } : {};
  return { block: ordered ? { t: "ol", start: startNumber, items, ...marked } : { t: "ul", items, ...marked }, next: i };
}

function parseBlocks(lines: string[], depth: number): Block[] {
  const out: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "") {
      i++;
      continue;
    }
    const fence = FENCE.exec(line);
    if (fence) {
      const body: string[] = [];
      i++;
      while (i < lines.length && !closesFence(lines[i], fence[1][0], fence[1].length)) {
        body.push(lines[i]);
        i++;
      }
      i++;
      out.push({ t: "code", lang: fence[2], v: body.join("\n") });
      continue;
    }
    const heading = HEADING.exec(line);
    if (heading) {
      out.push({ t: "h", level: heading[1].length, c: parseInline(heading[2]) });
      i++;
      continue;
    }
    if (HR.test(line)) {
      out.push({ t: "hr" });
      i++;
      continue;
    }
    if (depth < MAX_DEPTH && QUOTE.test(line)) {
      const inner: string[] = [];
      while (i < lines.length && QUOTE.test(lines[i])) {
        inner.push((QUOTE.exec(lines[i]) as RegExpExecArray)[1]);
        i++;
      }
      out.push({ t: "quote", c: parseBlocks(inner, depth + 1) });
      continue;
    }
    if (depth < MAX_DEPTH && LIST.test(line)) {
      const list = parseList(lines, i, depth);
      out.push(list.block);
      i = list.next;
      continue;
    }
    const table = parseTable(lines, i);
    if (table) {
      out.push(table.block);
      i = table.next;
      continue;
    }
    const paragraph: string[] = [];
    while (i < lines.length && lines[i].trim() !== "" && (paragraph.length === 0 || !startsBlock(lines[i], true))) {
      paragraph.push(lines[i].trim());
      i++;
    }
    out.push({ t: "p", c: parseInline(paragraph.join(" ")) });
  }
  return out;
}

export function parseMarkdown(source: string): Block[] {
  return parseBlocks(source.replace(/\r\n?/g, "\n").split("\n"), 0);
}

/** The words of some inline text, without its marks, for a heading's place in an outline. */
export function inlineText(nodes: readonly Inline[]): string {
  return nodes.map((n) => (n.t === "text" || n.t === "code" ? n.v : inlineText(n.c))).join("");
}

export interface OutlineEntry {
  /** The heading's place among the top-level blocks, which is what its drawn element is named by. */
  index: number;
  level: number;
  text: string;
}

/** The top-level headings, in order; a heading inside a list or a quote is not a section. */
export function outlineOf(blocks: readonly Block[]): OutlineEntry[] {
  const out: OutlineEntry[] = [];
  blocks.forEach((block, index) => {
    if (block.t === "h") out.push({ index, level: block.level, text: inlineText(block.c) });
  });
  return out;
}

/** The element id of the top-level block at `index`, for an outline to move to. */
export function blockId(prefix: string, index: number): string {
  return `${prefix}-b${index}`;
}
