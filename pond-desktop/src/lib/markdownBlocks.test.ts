import { describe, it, expect } from "vitest";
import { blockId, inlineText, isWebLink, outlineOf, parseInline, parseMarkdown, type Block, type Inline } from "./markdownBlocks";

const text = (v: string): Inline => ({ t: "text", v });

describe("inline markdown", () => {
  it("reads bold, italics and code spans", () => {
    expect(parseInline("a **bold** and *em* and `code`")).toEqual([
      text("a "),
      { t: "strong", c: [text("bold")] },
      text(" and "),
      { t: "em", c: [text("em")] },
      text(" and "),
      { t: "code", v: "code" },
    ]);
  });

  it("reads underscores at the start of a word but not inside one", () => {
    expect(parseInline("__bold__")).toEqual([{ t: "strong", c: [text("bold")] }]);
    expect(parseInline("snake_case_name")).toEqual([text("snake_case_name")]);
  });

  it("leaves a lone star as text", () => {
    expect(parseInline("2 * 3 = 6")).toEqual([text("2 * 3 = 6")]);
  });

  it("keeps an escaped mark as the character", () => {
    expect(parseInline("\\*not em\\*")).toEqual([text("*not em*")]);
  });

  it("makes a link of a web or mail address", () => {
    expect(parseInline("[site](https://example.com/a)")).toEqual([
      { t: "link", href: "https://example.com/a", c: [text("site")] },
    ]);
    expect(parseInline("[me](mailto:a@b.co)")).toEqual([{ t: "link", href: "mailto:a@b.co", c: [text("me")] }]);
  });

  it("shows any other kind of link as its text, so nothing can run from it", () => {
    for (const source of ["[x](javascript:alert(1))", "[x](data:text/html,hi)", "[x](/relative)", "[x](file:///etc/passwd)"]) {
      expect(JSON.stringify(parseInline(source)), source).not.toContain('"link"');
    }
  });

  it("shows an image as its description", () => {
    expect(parseInline("![a map](https://example.com/map.png)")).toEqual([text("a map")]);
  });
});

describe("blocks", () => {
  it("reads a heading and a paragraph that runs over two lines", () => {
    const blocks = parseMarkdown("# Title\n\none\ntwo");
    expect(blocks).toEqual([
      { t: "h", level: 1, c: [text("Title")] },
      { t: "p", c: [text("one two")] },
    ]);
  });

  it("drops the closing hashes of a heading", () => {
    expect(parseMarkdown("## Plan ##")).toEqual([{ t: "h", level: 2, c: [text("Plan")] }]);
  });

  it("reads a bulleted list and an ordered one that starts where it says", () => {
    const [bullets, numbers] = parseMarkdown("- one\n- two\n\n3. a\n4. b");
    expect(bullets).toMatchObject({ t: "ul" });
    expect((bullets as Extract<Block, { t: "ul" }>).items).toHaveLength(2);
    expect(numbers).toMatchObject({ t: "ol", start: 3 });
    expect((numbers as Extract<Block, { t: "ol" }>).items).toHaveLength(2);
  });

  it("nests a list under an item", () => {
    const [list] = parseMarkdown("- a\n  - b\n  - c\n- d") as [Extract<Block, { t: "ul" }>];
    expect(list.items).toHaveLength(2);
    const first = list.items[0];
    expect(first[0]).toMatchObject({ t: "p" });
    expect(first[1]).toMatchObject({ t: "ul" });
    expect((first[1] as Extract<Block, { t: "ul" }>).items).toHaveLength(2);
  });

  it("reads a fenced code block as it is written, marks and all", () => {
    expect(parseMarkdown("```js\nconst a = **1**;\n\nb();\n```")).toEqual([
      { t: "code", lang: "js", v: "const a = **1**;\n\nb();" },
    ]);
  });

  it("takes an unclosed fence to the end rather than losing the rest", () => {
    expect(parseMarkdown("```\nstill code")).toEqual([{ t: "code", lang: "", v: "still code" }]);
  });

  it("reads a quote and a rule", () => {
    const blocks = parseMarkdown("> quoted\n> more\n\n---\n\nafter");
    expect(blocks[0]).toEqual({ t: "quote", c: [{ t: "p", c: [text("quoted more")] }] });
    expect(blocks[1]).toEqual({ t: "hr" });
    expect(blocks[2]).toMatchObject({ t: "p" });
  });

  it("starts a list in the middle of a paragraph only at a bullet or a 1", () => {
    expect(parseMarkdown("intro\n- a")).toHaveLength(2);
    expect(parseMarkdown("born in\n1998. a good year")).toHaveLength(1);
  });

  it("reads Windows line endings and an empty source", () => {
    expect(parseMarkdown("a\r\n\r\nb")).toHaveLength(2);
    expect(parseMarkdown("")).toEqual([]);
  });

  it("does not choke on deep nesting", () => {
    expect(() => parseMarkdown(`${">".repeat(60)} deep`)).not.toThrow();
    expect(() => parseMarkdown(`${"  ".repeat(30)}- deep`)).not.toThrow();
  });

  it("leaves html as text", () => {
    expect(parseMarkdown("<script>alert(1)</script>")).toEqual([{ t: "p", c: [text("<script>alert(1)</script>")] }]);
  });
});

describe("strikethrough", () => {
  it("reads text between double tildes as struck out", () => {
    expect(parseInline("a ~~gone~~ b")).toEqual([text("a "), { t: "del", c: [text("gone")] }, text(" b")]);
  });

  it("keeps marks inside it, and leaves an unclosed or spaced pair as text", () => {
    expect(parseInline("~~**old**~~")).toEqual([{ t: "del", c: [{ t: "strong", c: [text("old")] }] }]);
    expect(parseInline("~~ not struck ~~")).toEqual([text("~~ not struck ~~")]);
    expect(parseInline("~~never closed")).toEqual([text("~~never closed")]);
    expect(parseInline("a ~ b ~ c")).toEqual([text("a ~ b ~ c")]);
  });
});

describe("addresses written out", () => {
  const link = (href: string): Inline => ({ t: "link", href, c: [text(href)] });

  it("makes a link of a bare web address, without the punctuation after it", () => {
    expect(parseInline("see https://example.com/a. Next")).toEqual([text("see "), link("https://example.com/a"), text(". Next")]);
    expect(parseInline("(https://example.com/a)")).toEqual([text("("), link("https://example.com/a"), text(")")]);
    expect(parseInline("go to http://example.com/a, then")).toEqual([text("go to "), link("http://example.com/a"), text(", then")]);
  });

  it("keeps a bracket the address opened itself", () => {
    expect(parseInline("https://en.wikipedia.org/wiki/Lamu_(town)")).toEqual([link("https://en.wikipedia.org/wiki/Lamu_(town)")]);
  });

  it("makes a link of an address in angle brackets", () => {
    expect(parseInline("<https://example.com/a>")).toEqual([link("https://example.com/a")]);
  });

  it("leaves anything that is not a web address as text", () => {
    for (const source of ["javascript:alert(1)", "<javascript:alert(1)>", "ftp://example.com/a", "xhttps://example.com", "https://", "<script>x</script>"]) {
      expect(JSON.stringify(parseInline(source)), source).not.toContain('"link"');
    }
  });

  it("does not make a link inside a code span, or a second link inside a link's own text", () => {
    expect(parseInline("`https://example.com`")).toEqual([{ t: "code", v: "https://example.com" }]);
    expect(parseInline("[https://example.com](https://example.com)")).toEqual([
      { t: "link", href: "https://example.com", c: [text("https://example.com")] },
    ]);
  });

  it("reads a map link whose address is encoded", () => {
    const url = "https://www.openstreetmap.org/search?query=Lamu%20Fort&zoom=15";
    expect(parseInline(`[Lamu Fort](${url})`)).toEqual([{ t: "link", href: url, c: [text("Lamu Fort")] }]);
  });

  it("says which addresses may be opened", () => {
    expect(isWebLink("https://a.co")).toBe(true);
    expect(isWebLink("mailto:a@b.co")).toBe(true);
    expect(isWebLink("javascript:alert(1)")).toBe(false);
    expect(isWebLink("/relative")).toBe(false);
  });
});

describe("tables", () => {
  const table = (source: string) => parseMarkdown(source)[0] as Extract<Block, { t: "table" }>;

  it("reads a header, its alignments and the rows", () => {
    expect(table("| Day | Meal | Cost |\n| :-- | :-: | --: |\n| Mon | Fish | 5 |\n| Tue | Rice | 3 |")).toEqual({
      t: "table",
      align: ["left", "center", "right"],
      head: [[text("Day")], [text("Meal")], [text("Cost")]],
      rows: [
        [[text("Mon")], [text("Fish")], [text("5")]],
        [[text("Tue")], [text("Rice")], [text("3")]],
      ],
    });
  });

  it("needs no outer pipes, and reads an unaligned column as none", () => {
    const t = table("Day | Meal\n--- | ---\nMon | Fish");
    expect(t.align).toEqual([null, null]);
    expect(t.rows).toEqual([[[text("Mon")], [text("Fish")]]]);
  });

  it("keeps an escaped pipe inside its cell", () => {
    expect(table("| a | b |\n|---|---|\n| x \\| y | z |").rows[0]).toEqual([[text("x | y")], [text("z")]]);
  });

  it("pads a short row and cuts a long one to the header's columns", () => {
    const t = table("| a | b |\n|---|---|\n| 1 |\n| 1 | 2 | 3 |");
    expect(t.rows[0]).toEqual([[text("1")], []]);
    expect(t.rows[1]).toEqual([[text("1")], [text("2")]]);
  });

  it("reads marks and links in a cell", () => {
    const t = table("| Where |\n|---|\n| **Lamu** [map](https://example.com/m) |");
    expect(t.rows[0][0]).toEqual([
      { t: "strong", c: [text("Lamu")] },
      text(" "),
      { t: "link", href: "https://example.com/m", c: [text("map")] },
    ]);
  });

  it("ends at a blank line, and what follows is read as it would be anywhere", () => {
    const blocks = parseMarkdown("| a |\n|---|\n| 1 |\n\nafter");
    expect(blocks.map((b) => b.t)).toEqual(["table", "p"]);
  });

  it("is a paragraph when there is no delimiter row, or the columns do not match", () => {
    expect(parseMarkdown("a | b\nc | d").map((b) => b.t)).toEqual(["p"]);
    expect(parseMarkdown("| a | b |\n|---|\n| 1 | 2 |").map((b) => b.t)).toEqual(["p"]);
    expect(parseMarkdown("| a |\n---").map((b) => b.t)).not.toContain("table");
  });

  it("reads a table inside a list item", () => {
    const [list] = parseMarkdown("- Plan:\n\n  | a |\n  |---|\n  | 1 |") as [Extract<Block, { t: "ul" }>];
    expect(list.items[0].map((b) => b.t)).toEqual(["p", "table"]);
  });

  it("never reads a cell as html, a picture, or a link that runs", () => {
    const t = table(
      "| x |\n|---|\n| <img src=x onerror=alert(1)> |\n| ![tracker](https://evil.example/t.png) |\n| [go](javascript:alert(1)) |",
    );
    expect(t.rows[0][0]).toEqual([text("<img src=x onerror=alert(1)>")]);
    expect(t.rows[1][0]).toEqual([text("tracker")]);
    expect(JSON.stringify(t.rows[2])).not.toContain('"link"');
  });
});

describe("task lists", () => {
  const list = (source: string) => parseMarkdown(source)[0] as Extract<Block, { t: "ul" | "ol" }>;

  it("reads a ticked and an unticked item, and takes the marker off the words", () => {
    expect(list("- [ ] tent\n- [x] stove\n- [X] pot")).toEqual({
      t: "ul",
      items: [[{ t: "p", c: [text("tent")] }], [{ t: "p", c: [text("stove")] }], [{ t: "p", c: [text("pot")] }]],
      tasks: [false, true, true],
    });
  });

  it("leaves an ordinary item in the same list as none", () => {
    expect(list("- [ ] tent\n- plain").tasks).toEqual([false, null]);
  });

  it("adds nothing to a list that has no task", () => {
    expect("tasks" in list("- a\n- [link](https://example.com)")).toBe(false);
    expect("tasks" in list("- [] not a task\n- [xx] nor this")).toBe(false);
  });

  it("reads them in a numbered list and under another item, and keeps the marks after the marker", () => {
    expect(list("1. [x] first").tasks).toEqual([true]);
    const outer = list("- Pack\n  - [ ] **tent**");
    const inner = outer.items[0][1] as Extract<Block, { t: "ul" }>;
    expect(inner.tasks).toEqual([false]);
    expect(inner.items[0][0]).toEqual({ t: "p", c: [{ t: "strong", c: [text("tent")] }] });
  });

  it("reads a marker with nothing after it as an empty task", () => {
    expect(list("- [x]").tasks).toEqual([true]);
  });

  it("never reads an item's words as html", () => {
    expect(list("- [ ] <script>alert(1)</script>").items[0]).toEqual([{ t: "p", c: [text("<script>alert(1)</script>")] }]);
  });
});

describe("an outline", () => {
  it("lists the top-level headings with where they are, and the words without their marks", () => {
    const blocks = parseMarkdown("# Title\n\n## One\n\n- ## inside a list\n\n### Two **bold**");
    expect(outlineOf(blocks)).toEqual([
      { index: 0, level: 1, text: "Title" },
      { index: 1, level: 2, text: "One" },
      { index: 3, level: 3, text: "Two bold" },
    ]);
  });

  it("names a block by its prefix and place", () => {
    expect(blockId("rpt", 3)).toBe("rpt-b3");
  });

  it("reads the words of nested marks", () => {
    expect(inlineText(parseInline("a *b* `c` ~~d~~ [e](https://x.co)"))).toBe("a b c d e");
  });
});
