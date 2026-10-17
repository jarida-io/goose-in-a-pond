import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import "../index";
import { SearchCard } from "./SearchCard";
import { findCardByHint, findCardRenderer } from "../registry";

afterEach(() => cleanup());

const RESULTS = [
  {
    title: "Opening hours - Example Town Library",
    url: "https://library.example.org/visit/hours",
    domain: "library.example.org",
    snippet: "Saturday 9:00 to 13:00. Closed on public holidays.",
  },
  {
    title: "Visiting the library",
    url: "https://www.example.com/council/library",
    domain: "example.com",
    snippet: "The reading room keeps the same hours on weekends.",
  },
];

const DATA = { query: "when does the library open on saturdays", results: RESULTS };

function many(n: number) {
  return Array.from({ length: n }, (_, i) => ({
    title: `Page ${i + 1}`,
    url: `https://site${i + 1}.example.org/`,
    domain: `site${i + 1}.example.org`,
    snippet: `Snippet ${i + 1}`,
  }));
}

describe("SearchCard registration", () => {
  it("answers to the hint the server sends for a web search", () => {
    // Nothing else catches a mismatch with the Rust hint: the card just silently stops rendering.
    expect(findCardByHint("search")?.component).toBe(SearchCard);
  });

  it("is chosen for search_web even with no hint, ahead of the knowledge card", () => {
    expect(findCardRenderer("giap-knowledge__search_web")?.key).toBe("search");
    expect(findCardRenderer("search_web")?.key).toBe("search");
  });

  it("leaves the encyclopedia to its own card", () => {
    expect(findCardRenderer("giap-knowledge__get_wikipedia_article")?.key).toBe("knowledge");
  });
});

describe("SearchCard", () => {
  it("shows the question and each result's title, site and snippet", () => {
    render(<SearchCard data={DATA} toolName="giap-knowledge__search_web" />);
    expect(screen.getByText("when does the library open on saturdays")).toBeTruthy();
    expect(screen.getByText("Opening hours - Example Town Library")).toBeTruthy();
    expect(screen.getByText("library.example.org")).toBeTruthy();
    expect(screen.getByText("Saturday 9:00 to 13:00. Closed on public holidays.")).toBeTruthy();
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("makes each title a link that opens outside the app, and does not leak the click", () => {
    const outer = vi.fn();
    render(
      <div onClick={outer}>
        <SearchCard data={DATA} toolName="t" />
      </div>,
    );
    const links = screen.getAllByRole("link") as HTMLAnchorElement[];
    expect(links.map((l) => l.href)).toEqual([
      "https://library.example.org/visit/hours",
      "https://www.example.com/council/library",
    ]);
    for (const link of links) {
      expect(link.target).toBe("_blank");
      expect(link.rel).toContain("noopener");
      expect(link.rel).toContain("noreferrer");
    }
    // The chat chip toggles on any click inside it.
    fireEvent.click(links[0]);
    expect(outer).not.toHaveBeenCalled();
  });

  it("tells a screen reader that a link leaves the app", () => {
    render(<SearchCard data={DATA} toolName="t" />);
    expect(screen.getAllByText("(opens in your browser)")).toHaveLength(2);
  });

  it("never turns a page's own address into a script or a file link", () => {
    const hostile = {
      query: "q",
      results: [
        { title: "Script", url: "javascript:alert(1)", domain: "evil.example", snippet: "s" },
        { title: "File", url: "file:///etc/passwd", domain: "", snippet: "s" },
        { title: "Data", url: "data:text/html,<b>x</b>", domain: "", snippet: "s" },
        { title: "Not a URL", url: "not a url", domain: "", snippet: "s" },
      ],
    };
    render(<SearchCard data={hostile} toolName="t" />);
    expect(screen.queryAllByRole("link")).toHaveLength(0);
    expect(screen.getByText("Script")).toBeTruthy();
    expect(screen.getByText("Not a URL")).toBeTruthy();
  });

  it("shows fewer results in the chat, more on the canvas", () => {
    const data = { query: "q", results: many(8) };
    const count = (variant: "compact" | "normal" | "large") => {
      const { unmount } = render(<SearchCard data={data} toolName="t" variant={variant} />);
      const n = screen.getAllByRole("listitem").length;
      unmount();
      return n;
    };
    expect(count("compact")).toBe(3);
    expect(count("normal")).toBe(5);
    expect(count("large")).toBe(8);
  });

  it("says it is searching rather than drawing an empty box", () => {
    render(<SearchCard data={{}} toolName="t" />);
    expect(screen.getByText("Searching the web…")).toBeTruthy();
    expect(screen.queryByRole("list")).toBeNull();
  });

  it("shows a finished call's own words when it came back without a card", () => {
    // Canvas and voice carry no hint, so a miss arrives as plain text and must not read as still searching.
    render(<SearchCard data={{ result: "No web results for that." }} toolName="giap-knowledge__search_web" />);
    expect(screen.getByText("No web results for that.")).toBeTruthy();
    expect(screen.queryByText("Searching the web…")).toBeNull();
  });

  it("says nothing came back when the search found no pages", () => {
    render(<SearchCard data={{ query: "zzzz", results: [] }} toolName="t" />);
    expect(screen.getByText("zzzz")).toBeTruthy();
    expect(screen.getByText("No pages came back for this search.")).toBeTruthy();
  });

  it("renders a partial result, working out the site from its address", () => {
    render(
      <SearchCard
        data={{ query: "q", results: [{ url: "https://www.example.org/a/b" }, { title: "Only a title" }] }}
        toolName="t"
      />,
    );
    expect(screen.getByText("example.org")).toBeTruthy();
    expect(screen.getByText("Only a title")).toBeTruthy();
  });

  it("ignores a results field that is not a list", () => {
    render(<SearchCard data={{ query: "q", results: "nope" }} toolName="t" />);
    expect(screen.getByText("No pages came back for this search.")).toBeTruthy();
  });

  it("shows no half of an attribution: the terms ask for the logo with the words, or neither", () => {
    const { container } = render(<SearchCard data={DATA} toolName="t" />);
    expect(container.querySelector("img, svg[role='img']")).toBeNull();
    expect(container.textContent?.toLowerCase()).not.toContain("powered by");
  });
});
