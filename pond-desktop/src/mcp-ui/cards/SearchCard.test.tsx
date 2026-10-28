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

const PLACES = [
  {
    name: "Java House Junction",
    url: "https://www.javahouseafrica.com/junction",
    snippet: "day: Monday; hours: 7 am - 10 pm",
  },
  { name: "Artcaffe Junction", url: "", snippet: "" },
];

function manyPlaces(n: number) {
  return Array.from({ length: n }, (_, i) => ({
    name: `Place ${i + 1}`,
    url: `https://place${i + 1}.example.org/`,
    snippet: `Snippet ${i + 1}`,
  }));
}

describe("SearchCard, nearby places", () => {
  const withPlaces = { ...DATA, places: PLACES };

  it("shows each place's name and what is known of it, above the pages", () => {
    render(<SearchCard data={withPlaces} toolName="t" />);
    const group = screen.getByRole("list", { name: "Nearby places" });
    expect(group.querySelectorAll("li")).toHaveLength(2);
    expect(screen.getByText("Nearby places")).toBeTruthy();
    expect(screen.getByText("day: Monday; hours: 7 am - 10 pm")).toBeTruthy();
    expect(screen.getByText("javahouseafrica.com")).toBeTruthy();

    const everything = document.querySelector(".ui-search")!.textContent!;
    expect(everything.indexOf("Java House Junction")).toBeLessThan(
      everything.indexOf("Opening hours - Example Town Library"),
    );
    // The pages keep their own label, so they are not read as more places.
    expect(screen.getByText("Pages")).toBeTruthy();
    expect(screen.getAllByRole("listitem")).toHaveLength(4);
  });

  it("links a place only when its address is a web address", () => {
    render(
      <SearchCard
        data={{
          query: "q",
          places: [
            { name: "Web", url: "https://a.example.org/x", snippet: "" },
            { name: "Plain http", url: "http://b.example.org/", snippet: "" },
            { name: "No address", url: "", snippet: "" },
            { name: "Script", url: "javascript:alert(1)", snippet: "" },
            { name: "File", url: "file:///etc/passwd", snippet: "" },
          ],
        }}
        toolName="t"
        variant="large"
      />,
    );
    const links = screen.getAllByRole("link") as HTMLAnchorElement[];
    expect(links.map((l) => l.href)).toEqual(["https://a.example.org/x", "http://b.example.org/"]);
    for (const link of links) {
      expect(link.target).toBe("_blank");
      expect(link.rel).toContain("noopener");
      expect(link.rel).toContain("noreferrer");
    }
    // Every place is still named, linked or not.
    for (const name of ["No address", "Script", "File"]) expect(screen.getByText(name)).toBeTruthy();
  });

  it("does not leak a place link's click to the chat chip around it", () => {
    const outer = vi.fn();
    render(
      <div onClick={outer}>
        <SearchCard data={withPlaces} toolName="t" />
      </div>,
    );
    fireEvent.click(screen.getByRole("link", { name: /Java House Junction/ }));
    expect(outer).not.toHaveBeenCalled();
  });

  it("tells a screen reader that a place link leaves the app", () => {
    render(<SearchCard data={{ query: "q", places: [PLACES[0]] }} toolName="t" />);
    expect(screen.getAllByText("(opens in your browser)")).toHaveLength(1);
  });

  it("draws the place name as text, never as markup", () => {
    render(
      <SearchCard
        data={{ query: "q", places: [{ name: "<img src=x onerror=alert(1)> Cafe", url: "", snippet: "<b>x</b>" }] }}
        toolName="t"
      />,
    );
    expect(document.querySelector(".ui-search img, .ui-search b")).toBeNull();
    expect(screen.getByText("<img src=x onerror=alert(1)> Cafe")).toBeTruthy();
  });

  it("is unchanged without places", () => {
    render(<SearchCard data={DATA} toolName="t" />);
    expect(screen.queryByText("Nearby places")).toBeNull();
    expect(screen.queryByText("Pages")).toBeNull();
    expect(screen.queryByRole("list", { name: "Nearby places" })).toBeNull();
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("shows places alone when no page came back, and does not say nothing did", () => {
    render(<SearchCard data={{ query: "q", results: [], places: PLACES }} toolName="t" />);
    expect(screen.getByText("Java House Junction")).toBeTruthy();
    expect(screen.queryByText("No pages came back for this search.")).toBeNull();
    expect(screen.queryByText("Pages")).toBeNull();
  });

  it("is a card, not a loading line, when only places arrived", () => {
    render(<SearchCard data={{ places: PLACES }} toolName="t" />);
    expect(screen.queryByText("Searching the web…")).toBeNull();
    expect(screen.getByText("Web search")).toBeTruthy();
    expect(screen.getByText("Artcaffe Junction")).toBeTruthy();
  });

  it("skips a place with no name, and a field of the wrong kind", () => {
    render(
      <SearchCard
        data={{
          query: "q",
          places: [{ name: "", url: "https://a.example.org/", snippet: "s" }, { name: 3 }, null, "x", { name: "Kept", url: 7, snippet: 9 }],
        }}
        toolName="t"
      />,
    );
    const group = screen.getByRole("list", { name: "Nearby places" });
    expect(group.querySelectorAll("li")).toHaveLength(1);
    expect(screen.getByText("Kept")).toBeTruthy();
    expect(screen.queryAllByRole("link")).toHaveLength(0);
  });

  it("ignores a places field that is not a list", () => {
    render(<SearchCard data={{ query: "q", results: RESULTS, places: "nope" }} toolName="t" />);
    expect(screen.queryByText("Nearby places")).toBeNull();
    expect(screen.getAllByRole("listitem")).toHaveLength(2);
  });

  it("shows fewer places in the chat than on the canvas, and never more than the pond sends", () => {
    const data = { query: "q", places: manyPlaces(5) };
    const count = (variant: "compact" | "normal" | "large") => {
      const { unmount } = render(<SearchCard data={data} toolName="t" variant={variant} />);
      const n = screen.getByRole("list", { name: "Nearby places" }).querySelectorAll("li").length;
      unmount();
      return n;
    };
    expect(count("compact")).toBe(3);
    expect(count("normal")).toBe(5);
    expect(count("large")).toBe(5);
  });
});

const THUMB = "https://imgs.search.brave.com/t/rs:fit:200:200:1/g:ce/aHR0cHM6";

const NEWS = [
  {
    title: "The library opens a new reading room",
    url: "https://news.example.org/reading-room",
    domain: "news.example.org",
    publisher: "Example News",
    date: "2026-10-26",
    thumbnail: THUMB,
    breaking: true,
  },
];

const VIDEOS = [
  {
    title: "A tour of the library",
    url: "https://video.example.org/watch?v=tour",
    domain: "video.example.org",
    duration: "4:12",
    creator: "Example Channel",
    date: null,
    thumbnail: THUMB,
  },
];

describe("SearchCard, what Brave adds beside the pages", () => {
  const withEverything = { ...DATA, news: NEWS, videos: VIDEOS };

  it("shows news and videos under their own labels, after the pages", () => {
    render(<SearchCard data={withEverything} toolName="t" />);
    expect(screen.getByText("Pages")).toBeTruthy();
    const news = screen.getByRole("list", { name: "News" });
    expect(news.textContent).toContain("The library opens a new reading room");
    expect(news.textContent).toContain("Example News · 2026-10-26");
    expect(news.textContent).toContain("Breaking");
    const videos = screen.getByRole("list", { name: "Videos" });
    expect(videos.textContent).toContain("A tour of the library");
    expect(videos.textContent).toContain("Example Channel · video.example.org");

    const everything = document.querySelector(".ui-search")!.textContent!;
    expect(everything.indexOf("Opening hours - Example Town Library")).toBeLessThan(
      everything.indexOf("The library opens a new reading room"),
    );
  });

  it("names a video's length on its picture, and in words for a screen reader", () => {
    render(<SearchCard data={withEverything} toolName="t" />);
    const badge = document.querySelector(".ui-search__duration")!;
    expect(badge.textContent).toBe("4:12");
    expect(badge.getAttribute("aria-hidden")).toBe("true");
    expect(screen.getByText(", 4:12 long")).toBeTruthy();
  });

  it("loads a thumbnail only from Brave's image proxy, with no referrer and no words of its own", () => {
    const elsewhere = [{ ...NEWS[0], thumbnail: "https://news.example.org/photo.jpg" }];
    render(<SearchCard data={{ ...DATA, news: elsewhere, videos: VIDEOS }} toolName="t" />);
    const images = Array.from(document.querySelectorAll("img"));
    expect(images.map((i) => i.getAttribute("src"))).toEqual([THUMB]);
    expect(images[0]!.getAttribute("referrerpolicy")).toBe("no-referrer");
    expect(images[0]!.getAttribute("alt")).toBe("");
  });

  it("opens news and videos outside the app, and only web addresses", () => {
    const script = [{ ...NEWS[0], url: "javascript:alert(1)" }];
    render(<SearchCard data={{ ...DATA, news: script, videos: VIDEOS }} toolName="t" />);
    expect(screen.queryByRole("list", { name: "News" })).toBeNull();
    const video = screen.getByRole("link", { name: /A tour of the library/ });
    expect(video.getAttribute("href")).toBe("https://video.example.org/watch?v=tour");
    expect(video.getAttribute("target")).toBe("_blank");
  });

  it("is a card when only news came back, and labels nothing when one group is all there is", () => {
    render(<SearchCard data={{ query: "q", news: NEWS }} toolName="t" />);
    expect(screen.getByRole("list", { name: "News" })).toBeTruthy();
    expect(screen.queryByText("News")).toBeNull();
    expect(screen.queryByText("No pages came back for this search.")).toBeNull();
  });

  it("ignores news and videos fields that are not lists", () => {
    render(<SearchCard data={{ ...DATA, news: "x", videos: { a: 1 } }} toolName="t" />);
    expect(screen.queryByRole("list", { name: "News" })).toBeNull();
    expect(screen.queryByRole("list", { name: "Videos" })).toBeNull();
    expect(screen.queryByText("Pages")).toBeNull();
  });

  it("shows a place's details in place of its snippet when the search knew them", () => {
    const place = {
      name: "Example Town Library",
      url: "https://library.example.org/",
      snippet: "phone: +254 700 000 000",
      hours: "Saturday 09:00 to 13:00",
      address: "1 Library Road",
      phone: "+254 700 000 000",
      rating: "4.5 of 5 from 12 reviews",
    };
    render(<SearchCard data={{ query: "q", places: [place] }} toolName="t" />);
    expect(screen.getByText("Open Saturday 09:00 to 13:00")).toBeTruthy();
    expect(screen.getByText("1 Library Road")).toBeTruthy();
    expect(screen.getByText("+254 700 000 000")).toBeTruthy();
    expect(screen.getByText("4.5 of 5 from 12 reviews")).toBeTruthy();
    expect(screen.queryByText("phone: +254 700 000 000")).toBeNull();
  });
});
