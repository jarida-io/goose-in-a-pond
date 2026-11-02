import { describe, it, expect, afterEach } from "vitest";
import { cleanup, render, within } from "@testing-library/react";
import { MarkdownText } from "./MarkdownText";

afterEach(cleanup);

function drawn(source: string): HTMLElement {
  const { container } = render(<MarkdownText source={source} />);
  return container.firstElementChild as HTMLElement;
}

describe("MarkdownText", () => {
  it("draws headings small enough to sit inside a chat", () => {
    const root = drawn("# One\n\n## Two\n\n### Three\n\n#### Four");
    expect(root.querySelectorAll("h1, h2, h3")).toHaveLength(0);
    expect(root.querySelector("h4")?.textContent).toBe("One");
    expect(root.querySelectorAll("h4")).toHaveLength(2);
    expect(root.querySelector("h5")?.textContent).toBe("Three");
    expect(root.querySelector("h6")?.textContent).toBe("Four");
  });

  it("draws emphasis, code and a paragraph", () => {
    const root = drawn("Some **bold**, *slanted* and `code`.");
    expect(root.querySelector("strong")?.textContent).toBe("bold");
    expect(root.querySelector("em")?.textContent).toBe("slanted");
    expect(root.querySelector("code")?.textContent).toBe("code");
    expect(root.querySelector("p")?.textContent).toBe("Some bold, slanted and code.");
  });

  it("opens a web link in a new window that cannot reach back", () => {
    const link = drawn("[ferry times](https://example.com/ferry)").querySelector("a");
    expect(link?.getAttribute("href")).toBe("https://example.com/ferry");
    expect(link?.getAttribute("target")).toBe("_blank");
    expect(link?.getAttribute("rel")).toBe("noopener noreferrer");
  });

  it("draws a link that is not to the web as plain text", () => {
    const root = drawn("[click](javascript:alert(1))");
    expect(root.querySelector("a")).toBeNull();
    expect(root.textContent).toContain("click");
  });

  it("never turns the pond's text into elements", () => {
    const root = drawn('<img src=x onerror="alert(1)"><script>alert(1)</script>');
    expect(root.querySelector("img, script")).toBeNull();
    expect(root.textContent).toContain("<script>");
  });

  it("draws a list with no paragraph inside a one-line item, and a nested list inside another", () => {
    const root = drawn("- one\n- two\n  - inner");
    const items = Array.from(root.querySelector("ul")?.children ?? []);
    expect(items).toHaveLength(2);
    expect(items[0].querySelector("p")).toBeNull();
    expect(items[0].textContent).toBe("one");
    expect(items[1].querySelector("ul li")?.textContent).toBe("inner");
  });

  it("starts a numbered list where the text says", () => {
    expect(drawn("3. a\n4. b").querySelector("ol")?.getAttribute("start")).toBe("3");
  });

  it("draws a code block in a pre, with its marks left alone", () => {
    const pre = drawn("```\n# not a heading\n**nor bold**\n```").querySelector("pre");
    expect(pre?.textContent).toBe("# not a heading\n**nor bold**");
    expect(pre?.querySelector("strong, h4")).toBeNull();
  });

  it("draws a quote and a rule", () => {
    const root = drawn("> said\n\n---");
    expect(root.querySelector("blockquote")?.textContent).toBe("said");
    expect(root.querySelector("hr")).not.toBeNull();
  });

  it("draws nothing for an empty source", () => {
    expect(drawn("").textContent).toBe("");
  });
});

describe("MarkdownText, strikethrough and addresses", () => {
  it("draws struck-out text as del", () => {
    expect(drawn("a ~~gone~~ b").querySelector("del")?.textContent).toBe("gone");
  });

  it("opens a bare web address in a window that cannot reach back", () => {
    const link = drawn("Read https://example.com/ferry. Then go.").querySelector("a");
    expect(link?.getAttribute("href")).toBe("https://example.com/ferry");
    expect(link?.textContent).toBe("https://example.com/ferry");
    expect(link?.getAttribute("target")).toBe("_blank");
    expect(link?.getAttribute("rel")).toBe("noopener noreferrer");
  });

  it("opens a map link the way the composer writes one", () => {
    const link = drawn("[Lamu Fort](https://www.openstreetmap.org/search?query=Lamu%20Fort)").querySelector("a");
    expect(link?.getAttribute("href")).toBe("https://www.openstreetmap.org/search?query=Lamu%20Fort");
    expect(link?.getAttribute("rel")).toBe("noopener noreferrer");
  });
});

describe("MarkdownText, tables", () => {
  const SOURCE = "| Day | Meal | Cost |\n| :-- | :-: | --: |\n| Mon | Fish | 5 |\n| Tue | Rice | 3 |";

  it("draws a table with a header row and a row per line", () => {
    const root = drawn(SOURCE);
    expect(Array.from(root.querySelectorAll("thead th")).map((th) => th.textContent)).toEqual(["Day", "Meal", "Cost"]);
    expect(root.querySelectorAll("thead th")[0].getAttribute("scope")).toBe("col");
    const rows = Array.from(root.querySelectorAll("tbody tr")).map((tr) => Array.from(tr.children).map((td) => td.textContent));
    expect(rows).toEqual([
      ["Mon", "Fish", "5"],
      ["Tue", "Rice", "3"],
    ]);
  });

  it("aligns a column as the delimiter row says", () => {
    const cells = drawn(SOURCE).querySelectorAll("tbody tr:first-child td");
    expect(cells[0].className).toContain("md__cell--left");
    expect(cells[1].className).toContain("md__cell--center");
    expect(cells[2].className).toContain("md__cell--right");
  });

  it("puts a wide table in a region that scrolls by keyboard, named for what it is", () => {
    const wrap = drawn(SOURCE).querySelector(".md__tablewrap");
    expect(wrap?.getAttribute("role")).toBe("region");
    expect(wrap?.getAttribute("aria-label")).toBe("Table");
    expect(wrap?.getAttribute("tabindex")).toBe("0");
    expect(wrap?.querySelector("table")).not.toBeNull();
  });

  it("draws marks and safe links in a cell", () => {
    const root = drawn("| Where |\n|---|\n| **Lamu** [map](https://example.com/m) |");
    expect(root.querySelector("td strong")?.textContent).toBe("Lamu");
    expect(root.querySelector("td a")?.getAttribute("href")).toBe("https://example.com/m");
  });

  it("never turns a cell into html, a picture that loads, or a link that runs", () => {
    const root = drawn(
      "| x |\n|---|\n| <img src=x onerror=alert(1)><script>alert(1)</script> |\n| ![tracker](https://evil.example/t.png) |\n| [go](javascript:alert(1)) |\n| [d](data:text/html,hi) |",
    );
    expect(root.querySelector("img, script, iframe, object, embed")).toBeNull();
    expect(root.querySelector("a")).toBeNull();
    expect(root.querySelector("td")?.textContent).toContain("<script>");
    expect(root.textContent).toContain("tracker");
  });
});

describe("MarkdownText, task lists", () => {
  it("draws a read-only box per item, ticked or not, named by the item's words", () => {
    const root = drawn("- [ ] tent\n- [x] stove\n- plain");
    const boxes = within(root).getAllByRole("checkbox");
    expect(boxes).toHaveLength(2);
    const tent = within(root).getByRole("checkbox", { name: "tent" });
    const stove = within(root).getByRole("checkbox", { name: "stove" });
    expect(tent.getAttribute("aria-checked")).toBe("false");
    expect(stove.getAttribute("aria-checked")).toBe("true");
    expect(tent.getAttribute("aria-readonly")).toBe("true");
    // A plain item keeps its bullet: there is nothing to tick.
    expect(Array.from(root.querySelectorAll("li")).at(-1)?.className).toBe("");
  });

  it("draws no form control, so nothing can be toggled", () => {
    const root = drawn("- [ ] tent\n- [x] stove");
    expect(root.querySelector("input, button")).toBeNull();
    expect(root.querySelector(".md__task")).not.toBeNull();
  });

  it("keeps a task's marks and nested list", () => {
    const root = drawn("- [ ] **tent**\n  - poles");
    expect(root.querySelector(".md__taskText strong")?.textContent).toBe("tent");
    expect(root.querySelector(".md__taskText ul li")?.textContent).toBe("poles");
  });

  it("never draws a task's words as html", () => {
    const root = drawn("- [ ] <img src=x onerror=alert(1)>");
    expect(root.querySelector("img")).toBeNull();
    expect(root.textContent).toContain("<img");
  });
});

describe("MarkdownText, a report's headings", () => {
  const SOURCE = "# Title\n\n## One\n\n### Two\n\n- ## inside\n\n#### Four";

  it("sits its headings below the report's own title, not below a chat's headings", () => {
    const { container } = render(<MarkdownText source={SOURCE} headings="report" />);
    const report = container.firstElementChild as HTMLElement;
    expect(report.querySelectorAll("h1, h2")).toHaveLength(0);
    expect(report.querySelector(".md__h1")?.tagName).toBe("H3");
    expect(report.querySelector(".md__h2")?.tagName).toBe("H3");
    expect(report.querySelector(".md__h3")?.tagName).toBe("H4");
    expect(report.querySelector(".md__h4")?.tagName).toBe("H5");
  });

  it("names only the top-level headings by id, and makes them focusable for an outline", () => {
    const { container } = render(<MarkdownText source={SOURCE} headings="report" idPrefix="rpt" />);
    const ids = Array.from(container.querySelectorAll("[id]")).map((el) => el.id);
    expect(ids).toEqual(["rpt-b0", "rpt-b1", "rpt-b2", "rpt-b4"]);
    expect(container.querySelector("#rpt-b1")?.getAttribute("tabindex")).toBe("-1");
    expect(container.querySelector("ul h3, ul h4, ul h5, ul h6")?.id).toBe("");
  });

  it("gives a chat's headings no id", () => {
    expect(drawn(SOURCE).querySelector("[id]")).toBeNull();
  });
});
