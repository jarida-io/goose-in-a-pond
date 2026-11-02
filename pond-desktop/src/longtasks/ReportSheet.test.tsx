import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { useState } from "react";
import { cleanup, fireEvent, render, screen, within } from "@testing-library/react";
import { ReportSheet } from "./ReportSheet";
import { TASK_ID } from "./longTaskFixtures";
import type { LongTaskReportHandle, ReportView } from "./useLongTaskReport";

vi.mock("../api/PondApiClient", () => ({
  api: { getLongTaskExport: vi.fn() },
}));

const FOCUSABLE = 'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])';

const MARKDOWN = [
  "# Kenya tax rules",
  "",
  "A short summary.",
  "",
  "## Income tax",
  "",
  "Text.",
  "",
  "### Bands",
  "",
  "| Band | Rate |",
  "|---|---|",
  "| First | 10% |",
  "",
  "#### Detail",
  "",
  "## Packing",
  "",
  "- [ ] passport",
  "",
  "## Sources",
  "",
  "- [KRA](https://www.kra.go.ke/)",
  "",
].join("\n");

function report(over: Partial<ReportView> = {}): ReportView {
  return {
    title: "Kenya tax rules",
    markdown: MARKDOWN,
    sections: 3,
    sources: [{ title: "KRA", url: "https://www.kra.go.ke/" }],
    calendarEvents: 0,
    summaryOnly: false,
    ...over,
  };
}

function sheet(over: Partial<ReportView> = {}, onClose = vi.fn(), variant: "classic" | "hub" = "classic") {
  const view = report(over);
  const handle: LongTaskReportHandle = { state: { status: "ready", report: view }, load: vi.fn(async () => view) };
  const { container } = render(
    <div>
      <ReportSheet task={{ id: TASK_ID, goal: "Kenya tax rules" }} report={view} handle={handle} variant={variant} onClose={onClose} />
    </div>,
  );
  return { onClose, container };
}

const dialog = () => screen.getByRole("dialog", { name: "Kenya tax rules" });
const text = () => screen.getByRole("region", { name: "The report" });
const nav = () => screen.queryByRole("navigation", { name: "Sections" });

let scrollTo: ReturnType<typeof vi.fn>;

beforeEach(() => {
  scrollTo = vi.fn();
  Object.defineProperty(HTMLElement.prototype, "scrollIntoView", { value: scrollTo, configurable: true, writable: true });
});

afterEach(() => {
  cleanup();
});

describe("the reading sheet", () => {
  it("is a modal dialog named by the report's title, drawn over the page rather than inside it", () => {
    const { container } = sheet();
    expect(dialog().getAttribute("aria-modal")).toBe("true");
    expect(within(dialog()).getByRole("heading", { level: 2, name: "Kenya tax rules" })).toBeTruthy();
    expect(container.querySelector("[role=dialog]")).toBeNull();
    expect(dialog().closest("body")).toBe(document.body);
  });

  it("says how long the report is, leaving out what it has none of", () => {
    sheet({ sections: 12, sources: Array.from({ length: 8 }, (_, i) => ({ title: `s${i}`, url: `https://e.co/${i}` })) });
    expect(screen.getByText("12 sections, 8 sources")).toBeTruthy();
    cleanup();

    sheet({ sections: 1, sources: [] });
    expect(screen.getByText("1 section")).toBeTruthy();
    cleanup();

    sheet({ sections: 0, sources: [] });
    expect(document.querySelector(".rsheet__counts")).toBeNull();
  });

  it("does not say the title twice: the report's own title line is taken off the text", () => {
    sheet();
    expect(screen.getAllByRole("heading", { name: "Kenya tax rules" })).toHaveLength(1);
    expect(within(text()).getByText("A short summary.")).toBeTruthy();
  });

  it("keeps a first heading that is not the title", () => {
    sheet({ markdown: "# Something else\n\ntext\n" });
    expect(within(text()).getByRole("heading", { name: "Something else" })).toBeTruthy();
  });

  it("draws the report as rich text, headings below its own title", () => {
    sheet();
    expect(within(text()).getByRole("heading", { name: "Income tax" }).tagName).toBe("H3");
    expect(within(text()).getByRole("heading", { name: "Bands" }).tagName).toBe("H4");
    expect(within(text()).getByRole("table")).toBeTruthy();
    expect(within(text()).getByRole("checkbox", { name: "passport" }).getAttribute("aria-checked")).toBe("false");
  });

  it("is sized for touch on the hub", () => {
    sheet({}, vi.fn(), "hub");
    expect(dialog().className).toContain("rsheet--hub");
  });
});

describe("the sections outline", () => {
  it("lists the sections and their parts, down to the third level, as buttons", () => {
    sheet();
    const names = within(nav() as HTMLElement)
      .getAllByRole("button")
      .map((b) => b.textContent);
    expect(names).toEqual(["Income tax", "Bands", "Packing", "Sources"]);
  });

  it("shows a part one step in from its section", () => {
    sheet();
    const buttons = within(nav() as HTMLElement).getAllByRole("button");
    expect(buttons.map((b) => b.getAttribute("data-depth"))).toEqual(["0", "1", "0", "0"]);
  });

  it("is left out of a report with too few headings to need it", () => {
    sheet({ markdown: "Just a paragraph.\n\n## Only one\n\ntext\n" });
    expect(nav()).toBeNull();
  });

  it("takes the reader to a section, and puts focus on its heading", () => {
    sheet();
    fireEvent.click(within(nav() as HTMLElement).getByRole("button", { name: "Packing" }));
    const heading = within(text()).getByRole("heading", { name: "Packing" });
    expect(scrollTo).toHaveBeenCalledTimes(1);
    expect(document.activeElement).toBe(heading);
    expect(heading.getAttribute("tabindex")).toBe("-1");
  });
});

describe("the keyboard and focus", () => {
  it("puts focus on the text first, so the arrow and page keys read it at once", () => {
    sheet();
    expect(document.activeElement).toBe(text());
    expect(text().getAttribute("tabindex")).toBe("0");
  });

  it("closes on Escape", () => {
    const { onClose } = sheet();
    fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("closes from its Close button", () => {
    const { onClose } = sheet();
    fireEvent.click(screen.getByRole("button", { name: "Close the report" }));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("closes on a press on the dimmed page behind it, but not on one inside", () => {
    const { onClose } = sheet();
    fireEvent.mouseDown(within(dialog()).getByText("A short summary."));
    fireEvent.mouseDown(dialog());
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.mouseDown(dialog().parentElement as Element);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it("keeps Tab inside: past the last control it comes back to the first, and Shift+Tab the other way", () => {
    sheet();
    const nodes = Array.from(dialog().querySelectorAll<HTMLElement>(FOCUSABLE));
    expect(nodes.length).toBeGreaterThan(3);

    nodes[nodes.length - 1].focus();
    fireEvent.keyDown(nodes[nodes.length - 1], { key: "Tab" });
    expect(document.activeElement).toBe(nodes[0]);

    fireEvent.keyDown(nodes[0], { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(nodes[nodes.length - 1]);
  });

  it("gives focus back to what opened it", () => {
    function Harness() {
      const [open, setOpen] = useState(false);
      const view = report();
      const handle: LongTaskReportHandle = { state: { status: "ready", report: view }, load: vi.fn(async () => view) };
      return (
        <>
          <button onClick={() => setOpen(true)}>Open</button>
          {open ? <ReportSheet task={{ id: TASK_ID, goal: "g" }} report={view} handle={handle} variant="classic" onClose={() => setOpen(false)} /> : null}
        </>
      );
    }
    render(<Harness />);
    const opener = screen.getByRole("button", { name: "Open" });
    opener.focus();
    fireEvent.click(opener);
    expect(screen.getByRole("dialog")).toBeTruthy();

    fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(opener);
  });

  it("lets an open save menu take the first Escape, and the sheet the second", () => {
    const { onClose } = sheet();
    const save = screen.getByRole("button", { name: "Save or print" });
    fireEvent.click(save);
    expect(screen.getByRole("menu", { name: "Save or print" })).toBeTruthy();

    fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
    expect(screen.queryByRole("menu")).toBeNull();
    expect(onClose).not.toHaveBeenCalled();

    fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
    expect(onClose).toHaveBeenCalledTimes(1);
  });
});

describe("saving and printing from the sheet", () => {
  it("offers the same menu as the card, in the sheet's header", () => {
    sheet({ calendarEvents: 2 });
    fireEvent.click(screen.getByRole("button", { name: "Save or print" }));
    expect(screen.getAllByRole("menuitem").map((el) => el.querySelector(".rexp__label")?.textContent)).toEqual([
      "Save as Markdown",
      "Save as a web page",
      "Print or save as PDF",
      "Add to calendar",
    ]);
    expect(screen.getByRole("menu").className).toContain("rexp__panel--popover");
  });
});

describe("what a report cites", () => {
  it("shows each source the text lists as a link that opens in the browser, once", () => {
    sheet();
    const links = within(text()).getAllByRole("link", { name: "KRA" });
    expect(links).toHaveLength(1);
    expect(links[0].getAttribute("href")).toBe("https://www.kra.go.ke/");
    expect(links[0].getAttribute("target")).toBe("_blank");
    expect(links[0].getAttribute("rel")).toBe("noopener noreferrer");
    expect(document.querySelector(".rsheet__sources")).toBeNull();
  });

  it("lists the sources itself when the text does not, and only the ones a browser may open", () => {
    sheet({
      markdown: "## Findings\n\nText.\n",
      sources: [
        { title: "KRA", url: "https://www.kra.go.ke/" },
        { title: "Bad", url: "javascript:alert(1)" },
        { title: "Mail", url: "mailto:a@b.co" },
      ],
    });
    const aside = screen.getByRole("region", { name: "Sources" });
    expect(within(aside).getAllByRole("link").map((a) => a.getAttribute("href"))).toEqual(["https://www.kra.go.ke/"]);
    expect(within(aside).getByRole("link", { name: "KRA" }).getAttribute("target")).toBe("_blank");
  });

  it("makes a link of an address written out in the text", () => {
    sheet({ markdown: "## Maps\n\nSee https://www.openstreetmap.org/search?query=Lamu%20Fort.\n\n## Other\n\nx\n", sources: [] });
    const link = within(text()).getByRole("link");
    expect(link.getAttribute("href")).toBe("https://www.openstreetmap.org/search?query=Lamu%20Fort");
    expect(link.getAttribute("rel")).toBe("noopener noreferrer");
  });
});

describe("a report that tries to run something", () => {
  const HOSTILE = [
    "## Findings",
    "",
    '<img src=x onerror="alert(1)"><script>alert(1)</script><iframe src="https://evil.example"></iframe>',
    "",
    "![tracker](https://evil.example/pixel.png)",
    "",
    "[click](javascript:alert(1)) and [data](data:text/html,hi) and [file](file:///etc/passwd)",
    "",
    "| Cell |",
    "|---|",
    "| <img src=x onerror=alert(1)> |",
    "| ![t](https://evil.example/t.png) |",
    "| [go](javascript:alert(1)) |",
    "",
    "- [ ] <b onmouseover=alert(1)>bold</b>",
    "",
    "## Second",
    "",
    "text",
    "",
  ].join("\n");

  it("draws all of it as text: no element, no picture that loads, no link that runs", () => {
    sheet({ markdown: HOSTILE, sources: [] });
    const body = text();
    expect(body.querySelector("img, script, iframe, object, embed, b")).toBeNull();
    expect(Array.from(body.querySelectorAll("a")).filter((a) => !/^https?:/i.test(a.getAttribute("href") ?? ""))).toEqual([]);
    expect(body.querySelector("a[href^='javascript:'], a[href^='data:'], a[href^='file:']")).toBeNull();
    expect(body.textContent).toContain("<script>alert(1)</script>");
    expect(body.textContent).toContain("tracker");
    expect(body.querySelector("[onerror], [onmouseover]")).toBeNull();
  });
});
