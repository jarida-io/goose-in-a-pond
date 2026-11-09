import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { WebWatch, WebWatchDetail } from "../api/types";
import { ConfirmProvider } from "../components/shared";

const appState = { requestedWatch: null as string | null };
const dispatch = vi.fn();
const api = vi.hoisted(() => ({
  listWebWatches: vi.fn(),
  getWebWatch: vi.fn(),
  editWebWatch: vi.fn(),
  deleteWebWatch: vi.fn(),
}));

vi.mock("../state/useAppContext", () => ({
  useAppState: () => appState,
  useAppDispatch: () => dispatch,
}));

vi.mock("../api/PondApiClient", () => ({ api }));

import { WatchesScreen } from "./WatchesScreen";

const NOW = new Date();
const hoursFrom = (h: number) => new Date(NOW.getTime() + h * 3_600_000).toISOString();

function watch(over: Partial<WebWatch> = {}): WebWatch {
  return {
    id: "w1",
    title: "Subaru Forester listings",
    address: "https://www.cars.example/forester",
    looking_for: "A Forester under 1.5 million",
    filters: { words: ["subaru", "forester"], max_price: 1_500_000 },
    filters_words: "subaru forester, up to 1,500,000",
    interval_hours: 6,
    paused: false,
    items_count: 12,
    matching_count: 3,
    failures: 0,
    last_checked_at: hoursFrom(-2),
    next_check_at: hoursFrom(4),
    created_at: hoursFrom(-48),
    session_id: null,
    ...over,
  };
}

function detail(over: Partial<WebWatchDetail> = {}): WebWatchDetail {
  return {
    ...watch(),
    items: [{ title: "2017 Subaru Forester", price: "1,450,000", currency: "KES", mileage: "98,000 km", link: "https://cars.example/1" }],
    changes: [
      {
        id: 2,
        kind: "price_changed",
        from: "1,500,000",
        to: "1,450,000",
        found_at: hoursFrom(-1),
        line: "Price down: 2017 Subaru Forester, KES 1,500,000 to KES 1,450,000",
        matched: true,
        item: { title: "2017 Subaru Forester", link: "https://cars.example/1" },
      },
      { id: 1, kind: "gone", found_at: hoursFrom(-30), line: "Gone: 2012 Forester", matched: false, item: { title: "2012 Forester" } },
    ],
    ...over,
  };
}

function show(onOpenChat = vi.fn()) {
  render(
    <ConfirmProvider>
      <WatchesScreen variant="classic" onOpenChat={onOpenChat} />
    </ConfirmProvider>,
  );
  return { onOpenChat };
}

beforeEach(() => {
  appState.requestedWatch = null;
  dispatch.mockReset();
  for (const fn of Object.values(api)) fn.mockReset();
});

afterEach(() => cleanup());

describe("the watches list", () => {
  it("lists each watch with what it looks for, how often, and what it found", async () => {
    api.listWebWatches.mockResolvedValue([watch(), watch({ id: "w2", title: "Flats in Kilimani", paused: true, failures: 3 })]);
    show();
    const list = await screen.findByRole("list", { name: "Watches" });
    const rows = within(list).getAllByRole("button");
    expect(rows).toHaveLength(2);
    expect(rows[0].textContent).toContain("cars.example");
    expect(rows[0].textContent).toContain("subaru forester, up to 1,500,000");
    expect(rows[0].textContent).toContain("12 items, 3 match");
    expect(rows[0].textContent).toContain("Every 6 hours");
    expect(rows[1].textContent).toContain("Paused");
    expect(rows[1].textContent).toContain("The last 3 checks could not read the page");
  });

  it("invites a first watch from a chat when there is none", async () => {
    api.listWebWatches.mockResolvedValue([]);
    const { onOpenChat } = show();
    fireEvent.click(await screen.findByRole("button", { name: "Ask in a chat" }));
    expect(onOpenChat).toHaveBeenCalled();
  });

  it("says why the list could not load, and tries again", async () => {
    api.listWebWatches.mockRejectedValueOnce(new Error("The pond is not answering")).mockResolvedValueOnce([]);
    show();
    expect((await screen.findByRole("alert")).textContent).toContain("The pond is not answering");
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText("Nothing watched yet");
  });
});

describe("one watch", () => {
  beforeEach(() => {
    api.listWebWatches.mockResolvedValue([watch()]);
    api.getWebWatch.mockResolvedValue(detail());
  });

  async function open() {
    show();
    fireEvent.click(await screen.findByRole("button", { name: /Subaru Forester listings/ }));
    return screen.findByRole("heading", { level: 1, name: "Subaru Forester listings" });
  }

  it("shows what changed, newest first, with matches marked, and what is on the page", async () => {
    await open();
    expect(api.getWebWatch).toHaveBeenCalledWith("w1");
    const changes = screen.getByRole("region", { name: "What changed" });
    const items = within(changes).getAllByRole("listitem");
    expect(items[0].textContent).toContain("Price down: 2017 Subaru Forester");
    expect(within(items[0]).getByText("Match")).toBeTruthy();
    expect(within(items[1]).queryByText("Match")).toBeNull();
    const found = screen.getByRole("region", { name: /On the page now/ });
    expect(found.textContent).toContain("KES 1,450,000 · 98,000 km");
    expect(within(found).getByRole("link", { name: "2017 Subaru Forester" }).getAttribute("target")).toBe("_blank");
  });

  it("pauses and resumes", async () => {
    await open();
    api.editWebWatch.mockResolvedValue(watch({ paused: true }));
    fireEvent.click(screen.getByRole("button", { name: "Pause" }));
    await screen.findByRole("button", { name: "Resume" });
    expect(api.editWebWatch).toHaveBeenCalledWith("w1", { paused: true });
  });

  it("saves edited filters and interval, as the pond's filter set", async () => {
    await open();
    api.editWebWatch.mockResolvedValue(watch({ interval_hours: 24, filters: { words: ["subaru"], min_year: 2015 } }));
    fireEvent.change(screen.getByLabelText("Words in the title"), { target: { value: "subaru" } });
    fireEvent.change(screen.getByLabelText("Highest price"), { target: { value: "" } });
    fireEvent.change(screen.getByLabelText("Oldest year"), { target: { value: "2015" } });
    fireEvent.change(screen.getByLabelText("Check every"), { target: { value: "24" } });
    fireEvent.click(screen.getByRole("button", { name: "Save changes" }));
    await screen.findByText("Saved.");
    expect(api.editWebWatch).toHaveBeenCalledWith("w1", {
      looking_for: "A Forester under 1.5 million",
      filters: { words: ["subaru"], min_year: 2015 },
      interval_hours: 24,
    });
    expect(screen.getByRole("button", { name: "Save changes" }).hasAttribute("disabled")).toBe(true);
  });

  it("deletes only once confirmed, then goes back to the list", async () => {
    await open();
    api.deleteWebWatch.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    const dialog = await screen.findByRole("dialog");
    expect(api.deleteWebWatch).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(api.deleteWebWatch).toHaveBeenCalledWith("w1"));
    await screen.findByRole("heading", { level: 1, name: "Watches" });
  });
});

it("opens the watch a notice asked for, once", async () => {
  appState.requestedWatch = "w9";
  api.listWebWatches.mockResolvedValue([]);
  api.getWebWatch.mockResolvedValue(detail({ id: "w9", title: "Flats in Kilimani" }));
  show();
  await screen.findByRole("heading", { level: 1, name: "Flats in Kilimani" });
  expect(api.getWebWatch).toHaveBeenCalledWith("w9");
  expect(dispatch).toHaveBeenCalledWith({ type: "TAKE_WATCH_REQUEST" });
});
