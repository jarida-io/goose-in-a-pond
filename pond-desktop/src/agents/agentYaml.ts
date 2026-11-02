// The desktop has no YAML package, and the builder needs only what recipes use: block maps and
// lists, flow lists and maps, quoted and block scalars. Anything else throws with its line number.

export type YamlValue = null | boolean | number | string | YamlValue[] | YamlMap;
export interface YamlMap {
  [key: string]: YamlValue;
}

export class YamlError extends Error {
  readonly line: number;
  constructor(message: string, line: number) {
    super(message);
    this.name = "YamlError";
    this.line = line;
  }
}

export function isYamlMap(value: YamlValue | undefined): value is YamlMap {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

function indentOf(line: string): number {
  let i = 0;
  while (i < line.length && line[i] === " ") i++;
  return i;
}

function setKey(map: YamlMap, key: string, value: YamlValue, line: number): void {
  if (key === "__proto__") throw new YamlError("this key name is not allowed", line);
  if (Object.prototype.hasOwnProperty.call(map, key)) {
    throw new YamlError(`"${key}" appears twice in the same map`, line);
  }
  map[key] = value;
}

interface Quoted {
  value: string;
  end: number;
}

function parseDoubleQuoted(s: string, start: number, line: number): Quoted {
  let out = "";
  let i = start + 1;
  while (i < s.length) {
    const c = s[i];
    if (c === '"') return { value: out, end: i + 1 };
    if (c !== "\\") {
      out += c;
      i++;
      continue;
    }
    const e = s[i + 1];
    if (e === undefined) break;
    i += 2;
    switch (e) {
      case "n": out += "\n"; break;
      case "t": out += "\t"; break;
      case "r": out += "\r"; break;
      case "b": out += "\b"; break;
      case "f": out += "\f"; break;
      case "0": out += "\0"; break;
      case '"': out += '"'; break;
      case "/": out += "/"; break;
      case "\\": out += "\\"; break;
      case " ": out += " "; break;
      case "_": out += " "; break;
      case "x":
      case "u":
      case "U": {
        const len = e === "x" ? 2 : e === "u" ? 4 : 8;
        const hex = s.slice(i, i + len);
        const code = parseInt(hex, 16);
        if (hex.length !== len || !/^[0-9a-fA-F]+$/.test(hex) || code > 0x10ffff) {
          throw new YamlError("\\" + e + " needs " + len + " hex digits for a character", line);
        }
        out += String.fromCodePoint(code);
        i += len;
        break;
      }
      default:
        throw new YamlError('"\\' + e + '" is not an escape YAML knows', line);
    }
  }
  throw new YamlError("a double-quoted text is never closed", line);
}

function parseSingleQuoted(s: string, start: number, line: number): Quoted {
  let out = "";
  let i = start + 1;
  while (i < s.length) {
    const c = s[i];
    if (c === "'") {
      if (s[i + 1] === "'") {
        out += "'";
        i += 2;
        continue;
      }
      return { value: out, end: i + 1 };
    }
    out += c;
    i++;
  }
  throw new YamlError("a single-quoted text is never closed", line);
}

/** A quote opens a quoted scalar only at the start of a token, so `don't` stays plain text. */
function quoteMayStart(s: string, i: number): boolean {
  return i === 0 || /[\s[{,:]/.test(s[i - 1]);
}

function stripComment(s: string): string {
  let inDouble = false;
  let inSingle = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (inDouble) {
      if (c === "\\") i++;
      else if (c === '"') inDouble = false;
    } else if (inSingle) {
      if (c === "'") {
        if (s[i + 1] === "'") i++;
        else inSingle = false;
      }
    } else if (c === '"' && quoteMayStart(s, i)) {
      inDouble = true;
    } else if (c === "'" && quoteMayStart(s, i)) {
      inSingle = true;
    } else if (c === "#" && (i === 0 || /\s/.test(s[i - 1]))) {
      return s.slice(0, i).trimEnd();
    }
  }
  return s.trimEnd();
}

function bracketDepth(s: string): number {
  let depth = 0;
  let inDouble = false;
  let inSingle = false;
  for (let i = 0; i < s.length; i++) {
    const c = s[i];
    if (inDouble) {
      if (c === "\\") i++;
      else if (c === '"') inDouble = false;
    } else if (inSingle) {
      if (c === "'") {
        if (s[i + 1] === "'") i++;
        else inSingle = false;
      }
    } else if (c === '"' && quoteMayStart(s, i)) {
      inDouble = true;
    } else if (c === "'" && quoteMayStart(s, i)) {
      inSingle = true;
    } else if (c === "[" || c === "{") {
      depth++;
    } else if (c === "]" || c === "}") {
      depth--;
    }
  }
  return depth;
}

function plainScalar(text: string, line: number): YamlValue {
  if (text === "" || text === "~" || /^(null|Null|NULL)$/.test(text)) return null;
  if (/^(true|True|TRUE)$/.test(text)) return true;
  if (/^(false|False|FALSE)$/.test(text)) return false;
  if (/^[-+]?(0|[1-9]\d*)(\.\d+)?([eE][-+]?\d+)?$/.test(text)) {
    const n = Number(text);
    if (Number.isFinite(n)) return n;
  }
  if (/^[&*!%@`|>]/.test(text)) {
    throw new YamlError(`"${text[0]}" at the start of a value needs the value in double quotes`, line);
  }
  return text;
}

function expectNothingAfter(rest: string, line: number): void {
  if (stripComment(rest.trim()) !== "") {
    throw new YamlError("there is text after the closing quote", line);
  }
}

function parseScalarText(text: string, line: number): YamlValue {
  if (text.startsWith('"')) {
    const q = parseDoubleQuoted(text, 0, line);
    expectNothingAfter(text.slice(q.end), line);
    return q.value;
  }
  if (text.startsWith("'")) {
    const q = parseSingleQuoted(text, 0, line);
    expectNothingAfter(text.slice(q.end), line);
    return q.value;
  }
  const plain = stripComment(text);
  if (/:(\s|$)/.test(plain)) {
    throw new YamlError("a colon followed by a space needs the text in double quotes", line);
  }
  return plainScalar(plain, line);
}

class FlowReader {
  private pos = 0;
  constructor(
    private readonly s: string,
    private readonly line: number,
  ) {}

  read(): YamlValue {
    const value = this.value();
    this.ws();
    if (this.pos < this.s.length) {
      throw new YamlError("there is text after the closing bracket", this.line);
    }
    return value;
  }

  private ws(): void {
    while (this.pos < this.s.length && /\s/.test(this.s[this.pos])) this.pos++;
  }

  private value(): YamlValue {
    this.ws();
    const c = this.s[this.pos];
    if (c === "[") return this.seq();
    if (c === "{") return this.map();
    if (c === '"') {
      const q = parseDoubleQuoted(this.s, this.pos, this.line);
      this.pos = q.end;
      return q.value;
    }
    if (c === "'") {
      const q = parseSingleQuoted(this.s, this.pos, this.line);
      this.pos = q.end;
      return q.value;
    }
    return this.plain();
  }

  private seq(): YamlValue[] {
    this.pos++;
    const out: YamlValue[] = [];
    for (;;) {
      this.ws();
      if (this.pos >= this.s.length) throw new YamlError("a [ list is never closed", this.line);
      if (this.s[this.pos] === "]") {
        this.pos++;
        return out;
      }
      out.push(this.value());
      this.ws();
      const c = this.s[this.pos];
      if (c === ",") this.pos++;
      else if (c === "]") {
        this.pos++;
        return out;
      } else {
        throw new YamlError('expected "," or "]" in the list', this.line);
      }
    }
  }

  private map(): YamlMap {
    this.pos++;
    const out: YamlMap = {};
    for (;;) {
      this.ws();
      if (this.pos >= this.s.length) throw new YamlError("a { map is never closed", this.line);
      if (this.s[this.pos] === "}") {
        this.pos++;
        return out;
      }
      const key = this.key();
      this.ws();
      if (this.s[this.pos] !== ":") throw new YamlError(`expected ":" after "${key}"`, this.line);
      this.pos++;
      this.ws();
      const next = this.s[this.pos];
      const value = next === "," || next === "}" ? null : this.value();
      setKey(out, key, value, this.line);
      this.ws();
      const c = this.s[this.pos];
      if (c === ",") this.pos++;
      else if (c === "}") {
        this.pos++;
        return out;
      } else {
        throw new YamlError('expected "," or "}" in the map', this.line);
      }
    }
  }

  private key(): string {
    this.ws();
    const c = this.s[this.pos];
    if (c === '"' || c === "'") {
      const q = c === '"' ? parseDoubleQuoted(this.s, this.pos, this.line) : parseSingleQuoted(this.s, this.pos, this.line);
      this.pos = q.end;
      return q.value;
    }
    const start = this.pos;
    while (this.pos < this.s.length) {
      const ch = this.s[this.pos];
      if (ch === ":" && (this.pos + 1 >= this.s.length || /[\s,\]}]/.test(this.s[this.pos + 1]))) break;
      if (ch === "," || ch === "}" || ch === "]") break;
      this.pos++;
    }
    const key = this.s.slice(start, this.pos).trim();
    if (key === "") throw new YamlError("a map entry has no key", this.line);
    return key;
  }

  private plain(): YamlValue {
    const start = this.pos;
    while (this.pos < this.s.length) {
      const ch = this.s[this.pos];
      if (ch === "," || ch === "]" || ch === "}") break;
      if (ch === ":" && /\s/.test(this.s[this.pos + 1] ?? " ")) break;
      this.pos++;
    }
    return plainScalar(this.s.slice(start, this.pos).trim(), this.line);
  }
}

const BLOCK_HEADER = /^([|>])([+-]?)([1-9]?)([+-]?)$/;

function isSeqItem(text: string): boolean {
  return text === "-" || text.startsWith("- ");
}

/** Splits `key: rest`; `null` when the text is not a map entry (a scalar, or a flow collection). */
function splitMapEntry(text: string, line: number): { key: string; rest: string } | null {
  if (text.startsWith("[") || text.startsWith("{")) return null;
  if (text.startsWith('"') || text.startsWith("'")) {
    const q = text[0] === '"' ? parseDoubleQuoted(text, 0, line) : parseSingleQuoted(text, 0, line);
    const m = /^\s*:(?:\s+(.*))?$/.exec(text.slice(q.end));
    return m ? { key: q.value, rest: (m[1] ?? "").trim() } : null;
  }
  for (let k = 0; k < text.length; k++) {
    const c = text[k];
    if (c === ":" && (k + 1 === text.length || text[k + 1] === " ")) {
      const key = text.slice(0, k).trimEnd();
      return key === "" ? null : { key, rest: text.slice(k + 1).trim() };
    }
    if (c === "#" && k > 0 && text[k - 1] === " ") return null;
  }
  return null;
}

function fold(lines: string[]): string {
  let out = "";
  for (let k = 0; k < lines.length; k++) {
    const cur = lines[k];
    if (k === 0) {
      out = cur;
    } else if (cur === "") {
      out += "\n";
    } else if (lines[k - 1] === "") {
      out += cur;
    } else if (/^\s/.test(cur) || /^\s/.test(lines[k - 1])) {
      out += "\n" + cur;
    } else {
      out += " " + cur;
    }
  }
  return out;
}

class BlockParser {
  private readonly lines: string[];
  private pos = 0;

  constructor(text: string) {
    this.lines = text.replace(/\r\n?/g, "\n").split("\n");
    // The newline that ends the last line is not a blank line after it, or `|+` would keep one too many.
    if (this.lines[this.lines.length - 1] === "") this.lines.pop();
  }

  parseDocument(): YamlValue {
    let i = this.peek();
    if (i !== -1 && this.lines[i].trim() === "---") {
      this.pos = i + 1;
      i = this.peek();
    }
    if (i === -1) return null;
    const value = this.parseNode(indentOf(this.lines[i]));
    const rest = this.peek();
    if (rest !== -1 && this.lines[rest].trim() !== "...") {
      throw new YamlError("this line does not belong to the document above it", rest + 1);
    }
    return value;
  }

  private peek(): number {
    for (let i = this.pos; i < this.lines.length; i++) {
      const t = this.lines[i].trim();
      if (t !== "" && !t.startsWith("#")) return i;
    }
    return -1;
  }

  private assertSpaces(line: string, index: number): void {
    if (/^ *\t/.test(line)) throw new YamlError("indent with spaces, not tabs", index + 1);
  }

  private parseNode(indent: number): YamlValue {
    const i = this.peek();
    const line = this.lines[i];
    this.assertSpaces(line, i);
    const text = line.slice(indentOf(line));
    if (isSeqItem(text)) return this.parseSeq(indent);
    if (splitMapEntry(text, i + 1)) return this.parseMap(indent);
    this.pos = i + 1;
    return this.inline(text, i);
  }

  private parseMap(indent: number): YamlMap {
    const out: YamlMap = {};
    for (;;) {
      const i = this.peek();
      if (i === -1) return out;
      const line = this.lines[i];
      const ind = indentOf(line);
      if (ind < indent) return out;
      this.assertSpaces(line, i);
      if (ind > indent) throw new YamlError("this line is indented more than the lines above it", i + 1);
      const text = line.slice(ind);
      const entry = isSeqItem(text) ? null : splitMapEntry(text, i + 1);
      if (!entry) {
        throw new YamlError(`expected "key: value" here, found "${text.slice(0, 40)}"`, i + 1);
      }
      this.pos = i + 1;
      setKey(out, entry.key, this.mapValue(entry.rest, i, indent), i + 1);
    }
  }

  private mapValue(rest: string, i: number, indent: number): YamlValue {
    const bare = stripComment(rest);
    if (bare === "") {
      const j = this.peek();
      if (j === -1) return null;
      const ind = indentOf(this.lines[j]);
      if (ind > indent) return this.parseNode(ind);
      if (ind === indent && isSeqItem(this.lines[j].slice(ind))) return this.parseSeq(indent);
      return null;
    }
    const block = BLOCK_HEADER.exec(bare);
    if (block) return this.blockScalar(block, indent);
    return this.inline(rest, i);
  }

  private parseSeq(indent: number): YamlValue[] {
    const out: YamlValue[] = [];
    for (;;) {
      const i = this.peek();
      if (i === -1) return out;
      const line = this.lines[i];
      const ind = indentOf(line);
      if (ind < indent) return out;
      this.assertSpaces(line, i);
      if (ind > indent) throw new YamlError("this line is indented more than the lines above it", i + 1);
      const text = line.slice(ind);
      if (!isSeqItem(text)) return out;
      const after = text === "-" ? "" : text.slice(2);
      const remainder = after.trimStart();
      const col = ind + 2 + (after.length - remainder.length);
      this.pos = i + 1;
      out.push(this.seqItem(remainder, i, ind, col));
    }
  }

  private seqItem(remainder: string, i: number, ind: number, col: number): YamlValue {
    const bare = stripComment(remainder);
    if (bare === "") {
      const j = this.peek();
      if (j === -1) return null;
      const next = indentOf(this.lines[j]);
      return next > ind ? this.parseNode(next) : null;
    }
    // The item's own content starts at `col`: re-seat the line there so it parses as a nested node.
    if (isSeqItem(remainder)) {
      this.lines[i] = " ".repeat(col) + remainder;
      this.pos = i;
      return this.parseSeq(col);
    }
    if (splitMapEntry(remainder, i + 1)) {
      this.lines[i] = " ".repeat(col) + remainder;
      this.pos = i;
      return this.parseMap(col);
    }
    const block = BLOCK_HEADER.exec(bare);
    if (block) return this.blockScalar(block, ind);
    return this.inline(remainder, i);
  }

  private inline(raw: string, i: number): YamlValue {
    const text = raw.trim();
    if (text.startsWith("[") || text.startsWith("{")) {
      let joined = stripComment(text);
      while (bracketDepth(joined) > 0) {
        if (this.pos >= this.lines.length) throw new YamlError("a [ or { is never closed", i + 1);
        joined += " " + stripComment(this.lines[this.pos].trim());
        this.pos++;
      }
      return new FlowReader(joined, i + 1).read();
    }
    return parseScalarText(text, i + 1);
  }

  private blockScalar(header: RegExpExecArray, parentIndent: number): string {
    const folded = header[1] === ">";
    const chomp = header[2] || header[4] || "";
    let blockIndent = header[3] ? parentIndent + Number(header[3]) : -1;
    const body: string[] = [];
    while (this.pos < this.lines.length) {
      const raw = this.lines[this.pos];
      if (raw.trim() === "") {
        body.push("");
        this.pos++;
        continue;
      }
      const ind = indentOf(raw);
      if (blockIndent === -1) {
        if (ind <= parentIndent) break;
        blockIndent = ind;
      }
      if (ind < blockIndent) break;
      body.push(raw.slice(blockIndent));
      this.pos++;
    }
    let end = body.length;
    while (end > 0 && body[end - 1] === "") end--;
    const blanks = body.length - end;
    const content = body.slice(0, end);
    if (content.length === 0) return chomp === "+" ? "\n".repeat(blanks) : "";
    const text = folded ? fold(content) : content.join("\n");
    if (chomp === "-") return text;
    if (chomp === "+") return text + "\n" + "\n".repeat(blanks);
    return text + "\n";
  }
}

export function parseYaml(text: string): YamlValue {
  return new BlockParser(text).parseDocument();
}

const PLAIN_SAFE = /^[A-Za-z_][A-Za-z0-9_.-]*$/;
const RESERVED_WORDS = new Set(["true", "false", "null", "yes", "no", "on", "off", "y", "n"]);

function scalarText(value: string | number | boolean | null): string {
  if (value === null) return "null";
  if (typeof value === "boolean") return value ? "true" : "false";
  if (typeof value === "number") return Number.isFinite(value) ? String(value) : "null";
  return PLAIN_SAFE.test(value) && !RESERVED_WORDS.has(value.toLowerCase()) ? value : JSON.stringify(value);
}

function keyText(key: string): string {
  return /^[A-Za-z_][A-Za-z0-9_-]*$/.test(key) ? key : JSON.stringify(key);
}

interface BlockText {
  header: string;
  lines: string[];
}

/** Tab and newline are the only control characters a block scalar can carry. */
function hasControlChars(text: string): boolean {
  for (let i = 0; i < text.length; i++) {
    const c = text.charCodeAt(i);
    if ((c < 0x20 && c !== 0x0a && c !== 0x09) || c === 0x7f) return true;
  }
  return false;
}

/** A block scalar for `text`, or `null` when only a quoted one can hold it faithfully. */
function asBlock(text: string, force: boolean): BlockText | null {
  if (!force && !text.includes("\n")) return null;
  if (hasControlChars(text) || text.trim() === "") return null;
  let trailing = 0;
  while (text.endsWith("\n", text.length - trailing)) trailing++;
  const body = text.slice(0, text.length - trailing);
  const lines = body.split("\n");
  if (/^\s/.test(lines[0])) return null;
  for (let n = 1; n < trailing; n++) lines.push("");
  return { header: trailing === 0 ? "|-" : trailing === 1 ? "|" : "|+", lines };
}

function blockLines(block: BlockText, pad: string): string[] {
  return block.lines.map((l) => (l === "" ? "" : pad + l));
}

function emitMap(map: YamlMap, indent: number, forced: Set<string>): string[] {
  const pad = " ".repeat(indent);
  const entries = Object.entries(map);
  if (entries.length === 0) return [pad + "{}"];
  const out: string[] = [];
  for (const [key, value] of entries) {
    const k = keyText(key);
    if (isYamlMap(value)) {
      if (Object.keys(value).length === 0) {
        out.push(`${pad}${k}: {}`);
      } else {
        out.push(`${pad}${k}:`, ...emitMap(value, indent + 2, forced));
      }
    } else if (Array.isArray(value)) {
      if (value.length === 0) {
        out.push(`${pad}${k}: []`);
      } else {
        out.push(`${pad}${k}:`, ...emitSeq(value, indent + 2, forced));
      }
    } else {
      const block = typeof value === "string" ? asBlock(value, forced.has(key)) : null;
      if (block) {
        out.push(`${pad}${k}: ${block.header}`, ...blockLines(block, pad + "  "));
      } else {
        out.push(`${pad}${k}: ${scalarText(value)}`);
      }
    }
  }
  return out;
}

function emitSeq(items: YamlValue[], indent: number, forced: Set<string>): string[] {
  const pad = " ".repeat(indent);
  const out: string[] = [];
  for (const item of items) {
    if (isYamlMap(item)) {
      if (Object.keys(item).length === 0) {
        out.push(pad + "- {}");
        continue;
      }
      const inner = emitMap(item, indent + 2, forced);
      inner[0] = `${pad}- ${inner[0].slice(indent + 2)}`;
      out.push(...inner);
    } else if (Array.isArray(item)) {
      if (item.length === 0) {
        out.push(pad + "- []");
        continue;
      }
      const inner = emitSeq(item, indent + 2, forced);
      inner[0] = `${pad}- ${inner[0].slice(indent + 2)}`;
      out.push(...inner);
    } else {
      const block = typeof item === "string" ? asBlock(item, false) : null;
      if (block) {
        out.push(`${pad}- ${block.header}`, ...blockLines(block, pad + "  "));
      } else {
        out.push(`${pad}- ${scalarText(item)}`);
      }
    }
  }
  return out;
}

export interface EmitOptions {
  /** Keys whose text is written as a block scalar even when it is one line. */
  block?: readonly string[];
}

export function emitYaml(value: YamlValue, options: EmitOptions = {}): string {
  const forced = new Set(options.block ?? []);
  let lines: string[];
  if (isYamlMap(value)) lines = emitMap(value, 0, forced);
  else if (Array.isArray(value)) lines = value.length === 0 ? ["[]"] : emitSeq(value, 0, forced);
  else lines = [scalarText(value)];
  return lines.join("\n") + "\n";
}
