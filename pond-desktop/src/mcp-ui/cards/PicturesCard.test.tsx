import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import "../index";
import { PicturesCard } from "./PicturesCard";
import { findCardByHint, findCardRenderer } from "../registry";

afterEach(() => cleanup());

const THUMB = "https://imgs.search.brave.com/p/rs:fit:500:0:0/g:ce/aHR0cHM6";

function picture(i: number, extra: Record<string, unknown> = {}) {
  return {
    title: `Lion ${i}`,
    url: `https://site${i}.example.org/lions`,
    domain: `site${i}.example.org`,
    thumbnail: `${THUMB}${i}`,
    width: 500,
    height: 333,
    ...extra,
  };
}

const DATA = { query: "lions at dawn", pictures: [picture(1), picture(2)] };

describe("PicturesCard registration", () => {
  it("answers to the hint show_pictures sends", () => {
    expect(findCardByHint("pictures")?.component).toBe(PicturesCard);
  });

  it("is chosen for show_pictures with no hint, ahead of the knowledge card", () => {
    expect(findCardRenderer("giap-knowledge__show_pictures")?.key).toBe("pictures");
    expect(findCardRenderer("giap-knowledge__search_web")?.key).toBe("search");
  });
});

describe("PicturesCard", () => {
  it("shows each picture by its title, each a link to its page outside the app", () => {
    render(<PicturesCard data={DATA} toolName="t" />);
    expect(screen.getByText("lions at dawn")).toBeTruthy();
    const grid = screen.getByRole("list", { name: "Pictures: lions at dawn" });
    expect(grid.querySelectorAll("li")).toHaveLength(2);
    const first = screen.getByRole("link", { name: /Lion 1/ });
    expect(first.getAttribute("href")).toBe("https://site1.example.org/lions");
    expect(first.getAttribute("target")).toBe("_blank");
    expect(first.getAttribute("rel")).toContain("noopener");
    expect(screen.getByAltText("Lion 1").getAttribute("src")).toBe(`${THUMB}1`);
  });

  it("does not leak a click to the chat chip around it", () => {
    const outer = vi.fn();
    render(
      <div onClick={outer}>
        <PicturesCard data={DATA} toolName="t" />
      </div>,
    );
    fireEvent.click(screen.getByRole("link", { name: /Lion 1/ }));
    expect(outer).not.toHaveBeenCalled();
  });

  it("loads pictures only from Brave's image proxy, with no referrer", () => {
    const data = {
      query: "q",
      pictures: [
        picture(1),
        picture(2, { thumbnail: "https://site2.example.org/original.jpg" }),
        picture(3, { thumbnail: "http://imgs.search.brave.com/x" }),
        picture(4, { url: "javascript:alert(1)" }),
      ],
    };
    render(<PicturesCard data={data} toolName="t" />);
    const images = Array.from(document.querySelectorAll("img"));
    expect(images.map((i) => i.getAttribute("src"))).toEqual([`${THUMB}1`]);
    expect(images[0]!.getAttribute("referrerpolicy")).toBe("no-referrer");
  });

  it("names an untitled picture by its site, so no picture is unlabelled", () => {
    render(<PicturesCard data={{ query: "q", pictures: [picture(1, { title: "" })] }} toolName="t" />);
    expect(screen.getByAltText("site1.example.org")).toBeTruthy();
  });

  it("shows fewer pictures in the chat than on the canvas", () => {
    const data = { query: "q", pictures: Array.from({ length: 12 }, (_, i) => picture(i)) };
    const count = (variant: "compact" | "normal" | "large") => {
      const { unmount } = render(<PicturesCard data={data} toolName="t" variant={variant} />);
      const n = document.querySelectorAll("img").length;
      unmount();
      return n;
    };
    expect(count("compact")).toBe(6);
    expect(count("normal")).toBe(9);
    expect(count("large")).toBe(12);
  });

  it("says it is finding pictures rather than drawing an empty box", () => {
    render(<PicturesCard data={{}} toolName="t" />);
    expect(screen.getByRole("status").textContent).toContain("Finding pictures");
  });

  it("shows a finished call's own words when none came back", () => {
    render(<PicturesCard data={{ result: "No results: pictures for \"x\". The search found none to show." }} toolName="t" />);
    expect(screen.getByText(/found none to show/)).toBeTruthy();
  });
});
