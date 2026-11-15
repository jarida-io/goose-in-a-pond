import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { WebSearchPanel } from "./WebSearchPanel";
import { api } from "../api/PondApiClient";
import { ApiError, type WebSearchState, type WebSearchStatus } from "../api/types";
import { DispatchCtx } from "../state/useAppContext";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getWebSearch: vi.fn(),
    setWebSearchKey: vi.fn(),
    deleteWebSearchKey: vi.fn(),
    updateSettings: vi.fn(),
  },
}));

const m = vi.mocked(api);

const KEY = "BSA-test-key-0123456789";

const LINES: Record<WebSearchStatus, string> = {
  ok: "On, searching with Brave",
  not_configured: "Off",
  rejected: "Brave did not accept the key",
  quota: "This month's free searches are used up",
  unverified: "Saved, not yet checked",
};

function reads(status: WebSearchStatus) {
  m.getWebSearch.mockResolvedValue({
    provider: "brave",
    configured: status !== "not_configured",
    status,
  });
}

async function open(status: WebSearchStatus) {
  reads(status);
  render(<WebSearchPanel />);
  await screen.findByText(LINES[status]);
}

const field = () => screen.getByLabelText("Brave Search key") as HTMLInputElement;
const save = () => screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;

function typeKey(value = KEY) {
  fireEvent.change(field(), { target: { value } });
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
  localStorage.clear();
  sessionStorage.clear();
});

describe("WebSearchPanel: the status line", () => {
  it.each(Object.keys(LINES) as WebSearchStatus[])("says %s in the words of the spec", async (status) => {
    await open(status);
    expect(screen.getByText(LINES[status])).toBeTruthy();
  });

  it("offers Remove only while a key is stored", async () => {
    await open("not_configured");
    expect(screen.queryByRole("button", { name: /remove/i })).toBeNull();
    cleanup();

    for (const status of ["ok", "rejected", "quota", "unverified"] as const) {
      await open(status);
      expect(screen.getByRole("button", { name: "Remove the Brave key" })).toBeTruthy();
      cleanup();
    }
  });

  it("says it is reading rather than showing a state it does not know", async () => {
    m.getWebSearch.mockReturnValue(new Promise(() => {}));
    render(<WebSearchPanel />);
    expect(screen.getByText("Reading the setting…")).toBeTruthy();
    expect(screen.queryByText("Off")).toBeNull();
    expect(field().disabled).toBe(true);
  });
});

describe("WebSearchPanel: what it always says", () => {
  it("keeps the privacy line visible before, during and after a read", async () => {
    const privacy = /your question is sent to Brave, and the top pages are read from their sites/;
    m.getWebSearch.mockRejectedValue(new ApiError(500, "boom"));
    render(<WebSearchPanel />);
    expect(screen.getByText(privacy)).toBeTruthy();
    await screen.findByRole("alert");
    expect(screen.getByText(privacy)).toBeTruthy();
  });

  it("says where to get a key, with a real link that opens outside the app", async () => {
    await open("not_configured");
    const link = screen.getByRole("link", { name: "api-dashboard.search.brave.com" }) as HTMLAnchorElement;
    expect(link.href).toBe("https://api-dashboard.search.brave.com/");
    expect(link.target).toBe("_blank");
    expect(link.rel).toContain("noopener");
    expect(
      screen.getByText(/The free monthly credit covers about 500 web searches, since each one asks Brave twice/),
    ).toBeTruthy();
    expect(screen.getByText(/Brave asks for a card to confirm who you are\./)).toBeTruthy();
  });

  it("masks the key and keeps password managers and spellcheck off it", async () => {
    await open("not_configured");
    expect(field().type).toBe("password");
    expect(field().autocomplete).toBe("off");
    expect(field().getAttribute("spellcheck")).toBe("false");
  });
});

describe("WebSearchPanel: saving a key", () => {
  it("does not offer Save for an empty key", async () => {
    await open("not_configured");
    expect(save().disabled).toBe(true);
    typeKey("   ");
    expect(save().disabled).toBe(true);
    typeKey();
    expect(save().disabled).toBe(false);
  });

  it("shows progress while Brave is asked, then the new state, and clears the field", async () => {
    await open("not_configured");
    let release!: (v: { configured: true; status: "ok" }) => void;
    m.setWebSearchKey.mockReturnValue(new Promise((resolve) => (release = resolve)));

    typeKey(`  ${KEY}\n`);
    fireEvent.click(save());

    expect(m.setWebSearchKey).toHaveBeenCalledWith(KEY);
    expect(screen.getByRole("button", { name: "Saving…" })).toBeTruthy();
    expect((screen.getByRole("button", { name: "Saving…" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("Checking the key with Brave…")).toBeTruthy();
    expect(field().disabled).toBe(true);

    release({ configured: true, status: "ok" });
    await screen.findByText(LINES.ok);

    expect(field().value).toBe("");
    expect(field().disabled).toBe(false);
    expect(screen.getByRole("button", { name: "Remove the Brave key" })).toBeTruthy();
    expect(screen.queryByText("Checking the key with Brave…")).toBeNull();
  });

  it.each([
    ["quota", LINES.quota],
    ["unverified", LINES.unverified],
  ] as const)("keeps a key that is stored but %s, and says so", async (status, line) => {
    await open("not_configured");
    m.setWebSearchKey.mockResolvedValue({ configured: true, status });
    typeKey();
    fireEvent.click(save());
    await screen.findByText(line);
    expect(field().value).toBe("");
    expect(screen.getByRole("button", { name: "Remove the Brave key" })).toBeTruthy();
  });

  it("moves focus to the new state, so a keyboard user is not left on a disabled button", async () => {
    await open("not_configured");
    m.setWebSearchKey.mockResolvedValue({ configured: true, status: "ok" });
    typeKey();
    fireEvent.click(save());
    const line = await screen.findByText(LINES.ok);
    await waitFor(() => expect(document.activeElement).toBe(line));
  });

  it("says Brave did not accept a refused key, beside the field, and changes nothing", async () => {
    await open("not_configured");
    m.setWebSearchKey.mockRejectedValue(new ApiError(422, "Brave did not accept this key.", undefined, {
      error: "Brave did not accept this key.",
    }));
    typeKey();
    fireEvent.click(save());

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("Brave did not accept this key.");
    expect(field().getAttribute("aria-invalid")).toBe("true");
    expect(field().getAttribute("aria-describedby")).toBe(alert.id);
    expect(screen.getByText(LINES.not_configured)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /remove/i })).toBeNull();
    // Kept, so a mistyped character can be fixed rather than the whole key pasted again.
    expect(field().value).toBe(KEY);
  });

  it("drops the refusal as soon as the key is edited", async () => {
    await open("not_configured");
    m.setWebSearchKey.mockRejectedValue(new ApiError(422, "Brave did not accept this key."));
    typeKey();
    fireEvent.click(save());
    await screen.findByRole("alert");
    typeKey(`${KEY}x`);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("does not blame the key when the pond could not be reached", async () => {
    await open("not_configured");
    m.setWebSearchKey.mockRejectedValue(new TypeError("Failed to fetch"));
    typeKey();
    fireEvent.click(save());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("Could not save the key. Try again in a moment.");
    expect(alert.textContent).not.toMatch(/Failed to fetch/);
  });

  it("does not claim a save an older pond never made", async () => {
    await open("not_configured");
    // `request` hands back undefined for the web page an older pond answers with.
    m.setWebSearchKey.mockResolvedValue(undefined as never);
    typeKey();
    fireEvent.click(save());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/does not know about web search yet/);
    expect(screen.getByText(LINES.not_configured)).toBeTruthy();
    expect(field().value).toBe(KEY);
  });

  it("replaces a stored key, and says so in the field", async () => {
    await open("rejected");
    expect(field().placeholder).toBe("Paste a new key to replace it");
    m.setWebSearchKey.mockResolvedValue({ configured: true, status: "ok" });
    typeKey();
    fireEvent.click(save());
    await screen.findByText(LINES.ok);
  });
});

describe("WebSearchPanel: the key is write-only", () => {
  it("never puts the key on screen, in storage, or back in a request, once saved", async () => {
    await open("not_configured");
    m.setWebSearchKey.mockResolvedValue({ configured: true, status: "ok" });
    const container = document.body;
    typeKey();
    fireEvent.click(save());
    await screen.findByText(LINES.ok);

    expect(field().value).toBe("");
    expect(container.textContent).not.toContain(KEY);
    expect(container.innerHTML).not.toContain(KEY);
    expect(JSON.stringify({ ...localStorage })).not.toContain(KEY);
    expect(JSON.stringify({ ...sessionStorage })).not.toContain(KEY);
    // Sent once, to the route that stores it, and read from nowhere.
    expect(m.setWebSearchKey).toHaveBeenCalledTimes(1);
    expect(m.getWebSearch).toHaveBeenCalledTimes(1);
  });

  it("does not repeat the key in a refusal", async () => {
    await open("not_configured");
    m.setWebSearchKey.mockRejectedValue(new ApiError(422, `Brave did not accept this key.`));
    typeKey();
    fireEvent.click(save());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).not.toContain(KEY);
  });
});

describe("WebSearchPanel: removing the key", () => {
  it("asks first, and a Keep leaves the key stored", async () => {
    await open("ok");
    fireEvent.click(screen.getByRole("button", { name: "Remove the Brave key" }));

    expect(screen.getByText("Remove the key?")).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Keep" })));
    fireEvent.click(screen.getByRole("button", { name: "Keep" }));

    expect(m.deleteWebSearchKey).not.toHaveBeenCalled();
    expect(screen.getByText(LINES.ok)).toBeTruthy();
    await waitFor(() =>
      expect(document.activeElement).toBe(screen.getByRole("button", { name: "Remove the Brave key" })),
    );
  });

  it("Escape is a Keep", async () => {
    await open("ok");
    fireEvent.click(screen.getByRole("button", { name: "Remove the Brave key" }));
    fireEvent.keyDown(screen.getByRole("button", { name: "Keep" }), { key: "Escape" });
    expect(screen.queryByText("Remove the key?")).toBeNull();
    expect(m.deleteWebSearchKey).not.toHaveBeenCalled();
  });

  it("removes the key, goes Off, and drops Remove", async () => {
    await open("quota");
    m.deleteWebSearchKey.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: "Remove the Brave key" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));

    const off = await screen.findByText(LINES.not_configured);
    expect(m.deleteWebSearchKey).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole("button", { name: /remove/i })).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(off));
    expect(field().placeholder).toBe("Paste your key");
  });

  it("says so, and keeps the key, when the pond could not remove it", async () => {
    await open("ok");
    m.deleteWebSearchKey.mockRejectedValue(new ApiError(500, "boom"));
    fireEvent.click(screen.getByRole("button", { name: "Remove the Brave key" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("Could not remove the key. Try again in a moment.");
    expect(screen.getByText(LINES.ok)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Remove the Brave key" })).toBeTruthy();
  });
});

describe("WebSearchPanel: when the pond cannot say", () => {
  it("explains, offers another try, and keeps the form from guessing", async () => {
    m.getWebSearch.mockRejectedValueOnce(new ApiError(500, "boom"));
    render(<WebSearchPanel />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("Could not read the web search setting. Try again in a moment.");
    expect(field().disabled).toBe(true);
    expect(save().disabled).toBe(true);

    reads("ok");
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByText(LINES.ok);
    expect(field().disabled).toBe(false);
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("says an older pond is older, rather than showing Off", async () => {
    m.getWebSearch.mockResolvedValue(undefined as never);
    render(<WebSearchPanel />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/does not know about web search yet/);
    expect(screen.queryByText("Off")).toBeNull();
  });
});

type Local = NonNullable<WebSearchState["local_results"]>;

const NAIROBI: Local = { enabled: false, town: "Nairobi" };
const SENDS_NAIROBI =
  "Sends your town (Nairobi) to Brave with each search, so it can find places near you. Your exact location is never sent.";
const TOWN_LINE = "Each web search sends Nairobi to Brave, so the results can include places near you.";

function readsWith(local: Local | undefined, status: WebSearchStatus = "ok") {
  m.getWebSearch.mockResolvedValue({
    provider: "brave",
    configured: status !== "not_configured",
    status,
    ...(local ? { local_results: local } : {}),
  });
}

async function openWith(local: Local | undefined, status: WebSearchStatus = "ok") {
  readsWith(local, status);
  render(<WebSearchPanel />);
  await screen.findByText(LINES[status]);
}

const nearby = () => screen.getByRole("switch", { name: "Nearby places" }) as HTMLButtonElement;
const checked = () => nearby().getAttribute("aria-checked");

describe("WebSearchPanel: nearby places", () => {
  it("is off until the household turns it on, and says exactly what would leave the house", async () => {
    await openWith(NAIROBI);
    expect(checked()).toBe("false");
    expect(screen.getByText(SENDS_NAIROBI)).toBeTruthy();
    expect(nearby().getAttribute("aria-describedby")).toContain("web-search-nearby-hint");
    expect(document.getElementById("web-search-nearby-hint")?.textContent).toBe(SENDS_NAIROBI);
    expect(m.updateSettings).not.toHaveBeenCalled();
  });

  it("draws the stored state once the pond has said it, not the one it mounted with", async () => {
    // The hub's own Toggle seeds from its prop once and would draw Off here.
    await openWith({ enabled: true, town: "Nairobi" });
    expect(checked()).toBe("true");
  });

  it("shows the switch beside a key that is not set yet, so the household sees what would be sent first", async () => {
    await openWith(NAIROBI, "not_configured");
    expect(nearby()).toBeTruthy();
  });

  it("turns on at once, saves only that one setting, and keeps it", async () => {
    await openWith(NAIROBI);
    let release!: (v: unknown) => void;
    m.updateSettings.mockReturnValue(new Promise((resolve) => (release = resolve)) as never);

    fireEvent.click(nearby());
    expect(checked()).toBe("true");
    expect(nearby().getAttribute("aria-busy")).toBe("true");
    expect(m.updateSettings).toHaveBeenCalledTimes(1);
    expect(m.updateSettings).toHaveBeenCalledWith({ web_search_local_results: true });

    release({ web_search_local_results: true });
    await waitFor(() => expect(nearby().getAttribute("aria-busy")).toBeNull());
    expect(checked()).toBe("true");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("is not saved twice by a second press while the first is still going", async () => {
    await openWith(NAIROBI);
    m.updateSettings.mockReturnValue(new Promise(() => {}) as never);
    fireEvent.click(nearby());
    fireEvent.click(nearby());
    expect(m.updateSettings).toHaveBeenCalledTimes(1);
    expect(checked()).toBe("true");
  });

  it("turns off again with a save of its own", async () => {
    await openWith({ enabled: true, town: "Nairobi" });
    m.updateSettings.mockResolvedValue({ web_search_local_results: false } as never);
    fireEvent.click(nearby());
    expect(checked()).toBe("false");
    expect(m.updateSettings).toHaveBeenCalledWith({ web_search_local_results: false });
    await waitFor(() => expect(nearby().getAttribute("aria-busy")).toBeNull());
    expect(checked()).toBe("false");
  });

  it("goes back and says so, in plain words, when the pond could not save it", async () => {
    await openWith(NAIROBI);
    m.updateSettings.mockRejectedValue(new ApiError(500, "disk_full", "disk_full"));
    fireEvent.click(nearby());
    expect(checked()).toBe("true");

    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toBe("Could not save this setting. Try again in a moment.");
    expect(alert.textContent).not.toMatch(/disk_full|500/);
    expect(checked()).toBe("false");
    expect(nearby().getAttribute("aria-describedby")).toContain(alert.id);
    expect(screen.queryByText(TOWN_LINE, { exact: false })).toBeNull();

    // A second try is open to them, and clears the complaint when it works.
    m.updateSettings.mockResolvedValue({ web_search_local_results: true } as never);
    fireEvent.click(nearby());
    await waitFor(() => expect(screen.queryByRole("alert")).toBeNull());
    expect(checked()).toBe("true");
  });

  it("does not blame the pond when the network is down", async () => {
    await openWith(NAIROBI);
    m.updateSettings.mockRejectedValue(new TypeError("Failed to fetch"));
    fireEvent.click(nearby());
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).not.toMatch(/Failed to fetch/);
  });

  it("believes the pond over its own guess when the pond answers with the other value", async () => {
    await openWith(NAIROBI);
    m.updateSettings.mockResolvedValue({ web_search_local_results: false } as never);
    fireEvent.click(nearby());
    await waitFor(() => expect(nearby().getAttribute("aria-busy")).toBeNull());
    expect(checked()).toBe("false");
  });

  it("keeps the switch where it was through saving and removing the key", async () => {
    await openWith({ enabled: true, town: "Nairobi" }, "not_configured");
    m.setWebSearchKey.mockResolvedValue({ configured: true, status: "ok" });
    typeKey();
    fireEvent.click(save());
    await screen.findByText(LINES.ok);
    expect(checked()).toBe("true");

    m.deleteWebSearchKey.mockResolvedValue(undefined);
    fireEvent.click(screen.getByRole("button", { name: "Remove the Brave key" }));
    fireEvent.click(screen.getByRole("button", { name: "Remove" }));
    await screen.findByText(LINES.not_configured);
    expect(checked()).toBe("true");
    expect(screen.getByText(TOWN_LINE, { exact: false })).toBeTruthy();
  });
});

describe("WebSearchPanel: where the question goes, with a town", () => {
  const privacy = /your question is sent to Brave, and the top pages are read from their sites/;

  it("names the town once nearby places are on, in the same line", async () => {
    await openWith({ enabled: true, town: "Nairobi" });
    const line = screen.getByText(privacy);
    expect(line.textContent).toContain(TOWN_LINE);
  });

  it("names it the moment the switch is turned on, and drops it if the save fails", async () => {
    await openWith(NAIROBI);
    expect(screen.getByText(privacy).textContent).not.toContain("Nairobi");

    m.updateSettings.mockRejectedValue(new ApiError(500, "boom"));
    fireEvent.click(nearby());
    expect(screen.getByText(privacy).textContent).toContain(TOWN_LINE);
    await screen.findByRole("alert");
    expect(screen.getByText(privacy).textContent).not.toContain("Nairobi");
  });

  it("says nothing of a town while the switch is off", async () => {
    await openWith(NAIROBI);
    expect(screen.getByText(privacy).textContent).toBe(
      "When the pond searches the web, your question is sent to Brave, and the top pages are read from their sites. Pictures in the results load from Brave's image server.",
    );
  });

  it("says nothing of a town that is not there to send", async () => {
    await openWith({ enabled: true, town: null });
    expect(screen.getByText(privacy).textContent).not.toMatch(/Each web search sends/);
  });
});

describe("WebSearchPanel: nearby places with no town to send", () => {
  it("cannot be turned on, and says to set the location first", async () => {
    await openWith({ enabled: false, town: null });
    expect(nearby().getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText(/Needs your location first/)).toBeTruthy();

    fireEvent.click(nearby());
    expect(m.updateSettings).not.toHaveBeenCalled();
    expect(checked()).toBe("false");
    // Still focusable, so a keyboard user can reach the reason beside it.
    nearby().focus();
    expect(document.activeElement).toBe(nearby());
  });

  it("links to the place a location is set, when the app can go there", async () => {
    const dispatch = vi.fn();
    readsWith({ enabled: false, town: null });
    render(
      <DispatchCtx.Provider value={dispatch}>
        <WebSearchPanel />
      </DispatchCtx.Provider>,
    );
    await screen.findByText(LINES.ok);
    fireEvent.click(screen.getByRole("button", { name: "Set your location" }));
    expect(dispatch).toHaveBeenCalledWith({ type: "SET_SECTION", payload: "settings" });
  });

  it("offers no link where the panel cannot navigate, and still gives the words", async () => {
    await openWith({ enabled: false, town: null });
    expect(screen.queryByRole("button", { name: "Set your location" })).toBeNull();
    expect(screen.getByText(/Account and Home, then Location/)).toBeTruthy();
  });

  it("can still be turned off when it was left on and the town has since gone", async () => {
    await openWith({ enabled: true, town: null });
    expect(checked()).toBe("true");
    expect(nearby().getAttribute("aria-disabled")).toBeNull();
    expect(screen.getByText(/so no town is sent/)).toBeTruthy();

    m.updateSettings.mockResolvedValue({ web_search_local_results: false } as never);
    fireEvent.click(nearby());
    expect(m.updateSettings).toHaveBeenCalledWith({ web_search_local_results: false });
    expect(checked()).toBe("false");
  });

  it("treats a blank town as no town", async () => {
    await openWith({ enabled: false, town: "   " });
    expect(nearby().getAttribute("aria-disabled")).toBe("true");
  });
});

describe("WebSearchPanel: a pond that predates nearby places", () => {
  it("says so, and offers nothing that would be saved into the void", async () => {
    await openWith(undefined);
    expect(screen.getByText(/cannot look for nearby places yet/)).toBeTruthy();
    expect(nearby().getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(nearby());
    expect(m.updateSettings).not.toHaveBeenCalled();
  });

  it("is not drawn while the pond has not said, or could not", async () => {
    m.getWebSearch.mockReturnValue(new Promise(() => {}));
    const { unmount } = render(<WebSearchPanel />);
    expect(screen.queryByRole("switch")).toBeNull();
    unmount();

    m.getWebSearch.mockRejectedValue(new ApiError(500, "boom"));
    render(<WebSearchPanel />);
    await screen.findByRole("alert");
    expect(screen.queryByRole("switch")).toBeNull();
  });
});
