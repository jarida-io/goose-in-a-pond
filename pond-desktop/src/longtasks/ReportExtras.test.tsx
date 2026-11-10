// The parts of a report beyond its text: pictures fetched with the token, and options a member can keep or watch.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { ReportImage, ReportOption } from "../api/types";
import { ReportSheet } from "./ReportSheet";
import { listingOf } from "./optionListing";
import { TASK_ID } from "./longTaskFixtures";
import type { LongTaskReportHandle, ReportView } from "./useLongTaskReport";

const getReportImage = vi.fn();
const createSavedItem = vi.fn();
const watchSavedItem = vi.fn();

vi.mock("../api/PondApiClient", () => ({
  api: {
    getLongTaskExport: vi.fn(),
    getReportImage: (src: string) => getReportImage(src),
    createSavedItem: (card: unknown) => createSavedItem(card),
    watchSavedItem: (id: string) => watchSavedItem(id),
  },
}));

const SRC = "/api/v1/report-images/0123456789abcdef0123";

const IMAGE: ReportImage = {
  src: SRC,
  credit: "Photo: A. Person, CC BY-SA 4.0",
  caption: "Lamu Fort",
  page: "https://en.wikipedia.org/wiki/Lamu_Fort",
  file_page: "https://commons.wikimedia.org/wiki/File:Lamu_Fort.jpg",
};

const OPTION: ReportOption = {
  title: "2017 Subaru Forester",
  price: "KES 1,450,000",
  link: "https://cars.example/forester-1",
  why: "Under budget and low mileage.",
  fields: [
    ["Mileage", "98,000 km"],
    ["Location", "Nairobi"],
    ["Colour", "Blue"],
  ],
  sources: [{ title: "Cars", url: "https://cars.example/forester-1" }],
};

function show(over: Partial<ReportView> = {}) {
  const view: ReportView = {
    title: "Cars under 1.5 million",
    markdown: "## Findings\n\nText.\n",
    sections: 1,
    sources: [],
    calendarEvents: 0,
    options: [],
    images: [],
    summaryOnly: false,
    ...over,
  };
  const handle: LongTaskReportHandle = { state: { status: "ready", report: view }, load: vi.fn(async () => view) };
  render(<ReportSheet task={{ id: TASK_ID, goal: "Cars" }} report={view} handle={handle} variant="classic" onClose={vi.fn()} />);
}

let createObjectURL: ReturnType<typeof vi.fn>;
let revokeObjectURL: ReturnType<typeof vi.fn>;

beforeEach(() => {
  getReportImage.mockReset();
  createSavedItem.mockReset();
  watchSavedItem.mockReset();
  createObjectURL = vi.fn(() => "blob:local/1");
  revokeObjectURL = vi.fn();
  vi.stubGlobal("URL", Object.assign(URL, { createObjectURL, revokeObjectURL }));
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
});

describe("a report's pictures", () => {
  it("fetches each through the api client and draws it from a local address, its credit under it", async () => {
    getReportImage.mockResolvedValue(new Blob(["x"], { type: "image/jpeg" }));
    show({ images: [IMAGE] });
    await waitFor(() => expect(screen.getByRole("img", { name: "Lamu Fort" }).getAttribute("src")).toBe("blob:local/1"));
    expect(getReportImage).toHaveBeenCalledWith(SRC);
    const pictures = screen.getByRole("region", { name: "Pictures from the sources" });
    expect(pictures.textContent).toContain("Photo: A. Person, CC BY-SA 4.0");
    expect(within(pictures).getByRole("link", { name: "Licence details" }).getAttribute("href")).toBe(IMAGE.file_page);
    expect(document.querySelector(`img[src="${SRC}"]`)).toBeNull();
  });

  it("lets go of the local address when the sheet closes", async () => {
    getReportImage.mockResolvedValue(new Blob(["x"], { type: "image/png" }));
    show({ images: [IMAGE] });
    await waitFor(() => expect(createObjectURL).toHaveBeenCalled());
    cleanup();
    expect(revokeObjectURL).toHaveBeenCalledWith("blob:local/1");
  });

  it("leaves out a picture that will not load, or is not one the pond keeps", async () => {
    getReportImage.mockResolvedValueOnce(new Blob(["<svg/>"], { type: "image/svg+xml" })).mockRejectedValueOnce(new Error("404"));
    show({ images: [IMAGE, { ...IMAGE, src: "/api/v1/report-images/fedcba9876543210" }] });
    await waitFor(() => expect(document.querySelectorAll(".rimg")).toHaveLength(0));
    expect(createObjectURL).not.toHaveBeenCalled();
  });
});

describe("a report's options", () => {
  it("shows each as a card with its price, facts and why", () => {
    show({ options: [OPTION] });
    const card = screen.getByRole("article", { name: OPTION.title });
    expect(card.textContent).toContain("KES 1,450,000");
    expect(card.textContent).toContain("98,000 km");
    expect(card.textContent).toContain("Under budget and low mileage.");
    expect(within(card).getByRole("link", { name: "Open the listing" }).getAttribute("href")).toBe(OPTION.link);
  });

  it("saves one as a listing card", async () => {
    createSavedItem.mockResolvedValue({ id: "s1" });
    show({ options: [OPTION] });
    const card = screen.getByRole("article", { name: OPTION.title });
    fireEvent.click(within(card).getByRole("button", { name: "Save" }));
    await within(card).findByRole("button", { name: "Saved" });
    expect(createSavedItem).toHaveBeenCalledWith(listingOf(OPTION));
    expect(within(card).getByRole("button", { name: "Saved" }).hasAttribute("disabled")).toBe(true);
  });

  it("watches its price, saving it first, and saves it once", async () => {
    createSavedItem.mockResolvedValue({ id: "s1" });
    watchSavedItem.mockResolvedValue({ id: "w1" });
    show({ options: [OPTION] });
    const card = screen.getByRole("article", { name: OPTION.title });
    fireEvent.click(within(card).getByRole("button", { name: "Watch price" }));
    await within(card).findByRole("button", { name: "Watching the price" });
    expect(createSavedItem).toHaveBeenCalledTimes(1);
    expect(watchSavedItem).toHaveBeenCalledWith("s1");
  });

  it("says why it could not save", async () => {
    createSavedItem.mockRejectedValue(new Error("saved cards belong to a household member; this device is not one"));
    show({ options: [OPTION] });
    fireEvent.click(screen.getByRole("button", { name: "Save" }));
    expect((await screen.findByRole("alert")).textContent).toContain("this device is not one");
  });

  it("offers no watch for an option with no web link", () => {
    show({ options: [{ ...OPTION, link: undefined }] });
    expect(screen.queryByRole("button", { name: "Watch price" })).toBeNull();
  });
});

describe("listingOf", () => {
  it("puts the facts a listing has a place for in their fields, and the rest with the why in its text", () => {
    expect(listingOf(OPTION)).toEqual({
      kind: "listing",
      title: OPTION.title,
      price: "KES 1,450,000",
      link: OPTION.link,
      year: undefined,
      mileage: "98,000 km",
      location: "Nairobi",
      body: "Under budget and low mileage.\n\n- **Colour:** Blue",
      sources: OPTION.sources,
    });
  });
});
