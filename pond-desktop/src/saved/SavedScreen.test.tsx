import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { SavedCollection, SavedItem } from "../api/types";
import { ConfirmProvider } from "../components/shared";

const api = vi.hoisted(() => ({
  listSavedItems: vi.fn(),
  listSavedCollections: vi.fn(),
  editSavedItem: vi.fn(),
  deleteSavedItem: vi.fn(),
  watchSavedItem: vi.fn(),
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { SavedScreen } from "./SavedScreen";

const BASE = { owner: "jerry", body: "", shared: false, created_at: "2026-11-01T10:00:00Z", updated_at: "2026-11-01T10:00:00Z", mine: true };

const RECIPE: SavedItem = {
  ...BASE,
  id: "r1",
  kind: "recipe",
  title: "Pilau",
  servings: "4",
  time: "45 min",
  ingredients: ["2 cups rice", "500 g beef", "pilau masala"],
  steps: ["Brown the beef.", "Add the rice and spice.", "Simmer."],
  body: "Serve with kachumbari.",
  collection: "Dinners",
};

const LISTING: SavedItem = {
  ...BASE,
  id: "l1",
  kind: "listing",
  title: "2017 Subaru Forester",
  price: "1,450,000",
  currency: "KES",
  year: "2017",
  mileage: "98,000 km",
  location: "Nairobi",
  link: "https://cars.example/1",
  photos: ["https://cars.example/1.jpg"],
  collection: "Cars",
  shared: true,
};

const PLACE: SavedItem = {
  ...BASE,
  id: "p1",
  kind: "place",
  title: "Lamu Fort",
  address: "Lamu Island",
  hours: "8:00 to 18:00",
  link: "https://museums.example/lamu",
  mine: false,
  owner: "liz",
  shared: true,
};

const COLLECTIONS: SavedCollection[] = [
  { name: "Cars", count: 1 },
  { name: "Dinners", count: 1 },
];

function show(items: SavedItem[] = [RECIPE, LISTING, PLACE], collections = COLLECTIONS) {
  api.listSavedItems.mockResolvedValue(items);
  api.listSavedCollections.mockResolvedValue(collections);
  render(
    <ConfirmProvider>
      <SavedScreen variant="classic" />
    </ConfirmProvider>,
  );
}

const grid = () => screen.findByRole("list", { name: "Saved" });

beforeEach(() => {
  for (const fn of Object.values(api)) fn.mockReset();
});

afterEach(() => cleanup());

describe("the saved cards", () => {
  it("gives a recipe, a listing and a place each their own face", async () => {
    show();
    const cards = within(await grid()).getAllByRole("button");
    expect(cards.map((c) => c.className.match(/scard--(\w+)/)?.[1])).toEqual(["recipe", "listing", "place"]);
    expect(cards[0].textContent).toContain("Serves 4 · 45 min · 3 ingredients · 3 steps");
    expect(cards[1].querySelector(".scard__price")?.textContent).toBe("KES 1,450,000");
    expect(cards[1].textContent).toContain("2017 · 98,000 km · Nairobi");
    expect(cards[1].textContent).toContain("Shared");
    expect(cards[2].textContent).toContain("Lamu Island · 8:00 to 18:00");
  });

  it("never loads a listing's photos, which would tell the site who is looking", async () => {
    show();
    await grid();
    expect(document.querySelector("img")).toBeNull();
  });

  it("filters by collection, with a chip for the cards in none", async () => {
    show();
    const nav = await screen.findByRole("navigation", { name: "Collections" });
    expect(within(nav).getAllByRole("button").map((b) => b.textContent)).toEqual(["All3", "Cars1", "Dinners1", "Not in a collection1"]);
    fireEvent.click(within(nav).getByRole("button", { name: /Dinners/ }));
    expect(within(nav).getByRole("button", { name: /Dinners/ }).getAttribute("aria-pressed")).toBe("true");
    expect(within(await grid()).getAllByRole("button").map((c) => c.textContent)).toEqual([expect.stringContaining("Pilau")]);
    fireEvent.click(within(nav).getByRole("button", { name: /Not in a collection/ }));
    expect(within(await grid()).getAllByRole("button").map((c) => c.textContent)).toEqual([expect.stringContaining("Lamu Fort")]);
  });

  it("invites a first save when there is none", async () => {
    show([], []);
    await screen.findByText("Nothing saved yet");
  });
});

describe("one card", () => {
  async function open(title: RegExp) {
    fireEvent.click(within(await grid()).getByRole("button", { name: title }));
  }

  it("shows a recipe's ingredients and numbered steps, then its notes", async () => {
    show();
    await open(/Pilau/);
    expect(screen.getByRole("heading", { level: 1, name: "Pilau" })).toBeTruthy();
    expect(screen.getByText("2 cups rice").tagName).toBe("LI");
    expect(screen.getByText("Simmer.").closest("ol")).toBeTruthy();
    expect(screen.getByText("Serve with kachumbari.")).toBeTruthy();
  });

  it("offers a listing's link and Watch price", async () => {
    api.watchSavedItem.mockResolvedValue({ id: "w1" });
    show();
    await open(/Subaru/);
    expect(screen.getByRole("link", { name: "Open the listing" }).getAttribute("href")).toBe("https://cars.example/1");
    fireEvent.click(screen.getByRole("button", { name: "Watch price" }));
    await screen.findByText(/Watching the price/);
    expect(api.watchSavedItem).toHaveBeenCalledWith("l1");
  });

  it("lets the owner share it with the household", async () => {
    api.editSavedItem.mockResolvedValue({ ...RECIPE, shared: true });
    api.listSavedCollections.mockResolvedValue(COLLECTIONS);
    show();
    await open(/Pilau/);
    const share = screen.getByRole("switch", { name: "Share with the household" });
    expect(share.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(share);
    await waitFor(() => expect(screen.getByRole("switch", { name: "Share with the household" }).getAttribute("aria-checked")).toBe("true"));
    expect(api.editSavedItem).toHaveBeenCalledWith("r1", { shared: true });
  });

  it("moves it out of its collection with null, and into a new one by name", async () => {
    api.editSavedItem.mockResolvedValueOnce({ ...RECIPE, collection: undefined }).mockResolvedValueOnce({ ...RECIPE, collection: "Weekend" });
    show();
    await open(/Pilau/);
    fireEvent.change(screen.getByLabelText("Collection"), { target: { value: "loose" } });
    await waitFor(() => expect(api.editSavedItem).toHaveBeenCalledWith("r1", { collection: null }));
    fireEvent.change(screen.getByLabelText("Collection"), { target: { value: "new" } });
    fireEvent.change(screen.getByLabelText("New collection name"), { target: { value: "Weekend" } });
    fireEvent.click(screen.getByRole("button", { name: "Move" }));
    await waitFor(() => expect(api.editSavedItem).toHaveBeenCalledWith("r1", { collection: "Weekend" }));
  });

  it("renames it", async () => {
    api.editSavedItem.mockResolvedValue({ ...RECIPE, title: "Beef pilau" });
    show();
    await open(/Pilau/);
    fireEvent.click(screen.getByRole("button", { name: "Rename" }));
    fireEvent.change(screen.getByLabelText("Name"), { target: { value: "Beef pilau" } });
    fireEvent.click(screen.getByRole("button", { name: "Save name" }));
    await screen.findByRole("heading", { level: 1, name: "Beef pilau" });
    expect(api.editSavedItem).toHaveBeenCalledWith("r1", { title: "Beef pilau" });
  });

  it("deletes it once confirmed", async () => {
    api.deleteSavedItem.mockResolvedValue(undefined);
    show();
    await open(/Pilau/);
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("dialog");
    expect(api.deleteSavedItem).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(api.deleteSavedItem).toHaveBeenCalledWith("r1"));
    await screen.findByRole("heading", { level: 1, name: "Saved" });
  });

  it("keeps another member's shared card read-only", async () => {
    show();
    await open(/Lamu Fort/);
    expect(screen.getByText(/Only they can change it/)).toBeTruthy();
    expect(screen.queryByRole("switch")).toBeNull();
    expect(screen.queryByRole("button", { name: "Delete" })).toBeNull();
  });
});
