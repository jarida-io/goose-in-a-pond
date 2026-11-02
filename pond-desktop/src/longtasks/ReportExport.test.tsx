import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { saveBlob } from "../lib/saveFile";
import { printHtml } from "../lib/printHtml";
import { ReportExport } from "./ReportExport";
import { TASK_ID } from "./longTaskFixtures";
import type { LongTaskReportHandle, ReportState, ReportView } from "./useLongTaskReport";

vi.mock("../api/PondApiClient", () => ({
  api: { getLongTaskExport: vi.fn() },
}));

vi.mock("../lib/saveFile", async (importOriginal) => ({ ...(await importOriginal<typeof import("../lib/saveFile")>()), saveBlob: vi.fn() }));
vi.mock("../lib/printHtml", () => ({ printHtml: vi.fn() }));

const exportFile = () => vi.mocked(api.getLongTaskExport);

beforeEach(() => {
  vi.resetAllMocks();
});

afterEach(() => {
  cleanup();
});

function report(over: Partial<ReportView> = {}): ReportView {
  return {
    title: "Kenya tax rules",
    markdown: "# Kenya tax rules\n\nSummary.\n",
    sections: 2,
    sources: [],
    calendarEvents: 0,
    summaryOnly: false,
    ...over,
  };
}

function show(state: ReportState, load: LongTaskReportHandle["load"] = vi.fn(async () => null), over: { onKeyDown?: () => void } = {}) {
  const handle: LongTaskReportHandle = { state, load };
  render(
    <div onKeyDown={over.onKeyDown}>
      <ReportExport task={{ id: TASK_ID, goal: "Kenya tax rules" }} handle={handle} variant="classic" placement="popover" />
    </div>,
  );
  return { load };
}

const ready = (over: Partial<ReportView> = {}): ReportState => ({ status: "ready", report: report(over) });
const trigger = () => screen.getByRole("button", { name: "Save or print" });
const menu = () => screen.queryByRole("menu", { name: "Save or print" });
const item = (name: string) => screen.getByRole("menuitem", { name });
const openMenu = () => fireEvent.click(trigger());

/** Presses an item and lets the request it starts settle; a pending one stays pending. */
async function choose(name: string) {
  openMenu();
  await act(async () => {
    fireEvent.click(item(name));
    await new Promise((r) => setTimeout(r, 0));
  });
}

describe("the save menu", () => {
  it("is a menu button, closed until pressed", () => {
    show(ready());
    expect(trigger().getAttribute("aria-haspopup")).toBe("menu");
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    expect(menu()).toBeNull();
    openMenu();
    expect(trigger().getAttribute("aria-expanded")).toBe("true");
    expect(trigger().getAttribute("aria-controls")).toBe(menu()?.id);
  });

  it("asks for the report when it is opened and has not been asked for, not before", () => {
    const { load } = show({ status: "idle" });
    expect(load).not.toHaveBeenCalled();
    openMenu();
    expect(load).toHaveBeenCalledTimes(1);
  });

  it("says it is checking while the report is on its way, and offers nothing yet", () => {
    const { load } = show({ status: "loading" });
    openMenu();
    expect(load).not.toHaveBeenCalled();
    const checking = item("Checking what can be saved...");
    expect(checking.getAttribute("aria-disabled")).toBe("true");
    expect(screen.getAllByRole("menuitem")).toHaveLength(1);
  });

  it("offers Markdown, a web page and printing, in that order, for a report", () => {
    show(ready());
    openMenu();
    expect(screen.getAllByRole("menuitem").map((el) => el.textContent)).toEqual([
      "Save as MarkdownPlain text you can open in any editor.",
      "Save as a web pageOne page you can open in a browser.",
      "Print or save as PDFOpens the print window.",
    ]);
  });

  it("offers adding to a calendar only when the report holds dated events", () => {
    show(ready({ calendarEvents: 0 }));
    openMenu();
    expect(screen.queryByRole("menuitem", { name: "Add to calendar" })).toBeNull();
    cleanup();

    show(ready({ calendarEvents: 3 }));
    openMenu();
    expect(item("Add to calendar")).toBeTruthy();
    expect(screen.getAllByRole("menuitem")).toHaveLength(4);
  });

  it("uses plain words, with no mention of artifacts or formats", () => {
    show(ready({ calendarEvents: 1 }));
    openMenu();
    expect(menu()?.textContent).not.toMatch(/artifact|format|export/i);
    expect(trigger().textContent).toBe("Save or print");
  });
});

describe("saving a file from the pond", () => {
  it("saves Markdown under the name the pond gave it, and says it is ready", async () => {
    const blob = new Blob(["# Plan"]);
    exportFile().mockResolvedValue({ blob, filename: "kenya-tax-rules.md" });
    show(ready());
    await choose("Save as Markdown");

    expect(exportFile()).toHaveBeenCalledWith(TASK_ID, "markdown");
    expect(saveBlob).toHaveBeenCalledWith(blob, "kenya-tax-rules.md");
    expect(screen.getByRole("status").textContent).toBe("kenya-tax-rules.md is ready.");
    expect(menu()).toBeNull();
  });

  it("saves a web page, naming it from the title when the page cannot read the pond's header", async () => {
    const blob = new Blob(["<p>x</p>"]);
    exportFile().mockResolvedValue({ blob, filename: null });
    show(ready());
    await choose("Save as a web page");

    expect(exportFile()).toHaveBeenCalledWith(TASK_ID, "html");
    expect(saveBlob).toHaveBeenCalledWith(blob, "kenya-tax-rules.html");
  });

  it("saves the dated events as a calendar file", async () => {
    const blob = new Blob(["BEGIN:VCALENDAR"]);
    exportFile().mockResolvedValue({ blob, filename: null });
    show(ready({ calendarEvents: 2 }));
    await choose("Add to calendar");

    expect(exportFile()).toHaveBeenCalledWith(TASK_ID, "ics");
    expect(saveBlob).toHaveBeenCalledWith(blob, "kenya-tax-rules.ics");
  });

  it("says there are no dated events when the pond finds none after all", async () => {
    exportFile().mockRejectedValue(new ApiError(404, "this report has no dated events for a calendar"));
    show(ready({ calendarEvents: 2 }));
    await choose("Add to calendar");

    expect(screen.getByRole("alert").textContent).toBe("This report has no dated events to add.");
    expect(saveBlob).not.toHaveBeenCalled();
  });

  it("says what went wrong, in the pond's words, when a file cannot be had", async () => {
    exportFile().mockRejectedValue(new ApiError(403, "nobody identified"));
    show(ready());
    await choose("Save as Markdown");
    expect(screen.getByRole("alert").textContent).toBe("Couldn't get the file. nobody identified");
    expect(saveBlob).not.toHaveBeenCalled();
  });

  it("clears an earlier note or problem when the next one starts", async () => {
    exportFile().mockRejectedValueOnce(new Error("boom")).mockResolvedValue({ blob: new Blob(["x"]), filename: "a.md" });
    show(ready());
    await choose("Save as Markdown");
    expect(screen.getByRole("alert")).toBeTruthy();
    await choose("Save as Markdown");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.getByRole("status").textContent).toBe("a.md is ready.");
  });

  it("says it is getting the file while it waits, and ignores a second press meanwhile", async () => {
    let arrive: (v: { blob: Blob; filename: string | null }) => void = () => {};
    exportFile().mockReturnValue(new Promise<{ blob: Blob; filename: string | null }>((resolve) => (arrive = resolve)));
    show(ready());
    await choose("Save as Markdown");
    expect(screen.getByRole("status").textContent).toBe("Getting the file...");
    expect(trigger().getAttribute("aria-busy")).toBe("true");

    // Opened on its own first: inside one act the open menu has not rendered yet.
    openMenu();
    await act(async () => {
      fireEvent.click(item("Save as a web page"));
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(exportFile()).toHaveBeenCalledTimes(1);

    await act(async () => {
      arrive({ blob: new Blob(["x"]), filename: "a.md" });
      await new Promise((r) => setTimeout(r, 0));
    });
    expect(screen.getByRole("status").textContent).toBe("a.md is ready.");
    expect(trigger().getAttribute("aria-busy")).toBeNull();
  });
});

describe("printing", () => {
  it("prints the pond's web page from a hidden frame, and saves nothing", async () => {
    exportFile().mockResolvedValue({ blob: new Blob(["<p>the page</p>"]), filename: null });
    vi.mocked(printHtml).mockResolvedValue(undefined);
    show(ready());
    await choose("Print or save as PDF");

    expect(exportFile()).toHaveBeenCalledWith(TASK_ID, "html");
    expect(printHtml).toHaveBeenCalledWith("<p>the page</p>");
    expect(saveBlob).not.toHaveBeenCalled();
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("gives focus back to the button once the print window is done", async () => {
    exportFile().mockResolvedValue({ blob: new Blob(["<p>x</p>"]), filename: null });
    vi.mocked(printHtml).mockResolvedValue(undefined);
    show(ready());
    trigger().focus();
    await choose("Print or save as PDF");
    expect(document.activeElement).toBe(trigger());
  });

  it("says when the print window could not be opened, and what to do instead", async () => {
    exportFile().mockResolvedValue({ blob: new Blob(["<p>x</p>"]), filename: null });
    vi.mocked(printHtml).mockRejectedValue(new Error("no printer"));
    show(ready());
    await choose("Print or save as PDF");
    expect(screen.getByRole("alert").textContent).toBe("Couldn't open the print window. no printer");
  });

  it("says when the page to print could not be had", async () => {
    exportFile().mockRejectedValue(new Error("offline"));
    show(ready());
    await choose("Print or save as PDF");
    expect(screen.getByRole("alert").textContent).toBe("Couldn't open the print window. offline");
    expect(printHtml).not.toHaveBeenCalled();
  });
});

describe("on a pond that can only give the Result", () => {
  it("offers Markdown alone, with the one line saying why there is no more", () => {
    show(ready({ summaryOnly: true, calendarEvents: 5 }));
    openMenu();
    expect(screen.getAllByRole("menuitem")).toHaveLength(2);
    expect(item("Save as Markdown")).toBeTruthy();
    expect(item("This pond can only save the result as Markdown for now.").getAttribute("aria-disabled")).toBe("true");
    expect(screen.queryByRole("menuitem", { name: "Add to calendar" })).toBeNull();
    expect(screen.queryByRole("menuitem", { name: "Print or save as PDF" })).toBeNull();
  });

  it("saves the text it already holds, without asking the pond for a file", async () => {
    show(ready({ summaryOnly: true, markdown: "- ferry at **7:00**" }));
    await choose("Save as Markdown");

    expect(exportFile()).not.toHaveBeenCalled();
    const [blob, name] = vi.mocked(saveBlob).mock.calls[0];
    expect(await blob.text()).toBe("- ferry at **7:00**");
    expect(name).toBe("kenya-tax-rules.md");
    expect(screen.getByRole("status").textContent).toBe("kenya-tax-rules.md is ready.");
  });
});

describe("when what can be saved could not be checked", () => {
  it("offers to try again, says why, and keeps the menu open to show what it finds", () => {
    const { load } = show({ status: "failed", message: "offline" });
    openMenu();
    expect(item("Try again").textContent).toContain("Couldn't check what can be saved. offline");
    fireEvent.click(item("Try again"));
    expect(load).toHaveBeenCalledTimes(1);
    expect(menu()).not.toBeNull();
  });
});

describe("the keyboard", () => {
  it("opens on ArrowDown to the first item, and on ArrowUp to the last", () => {
    show(ready({ calendarEvents: 1 }));
    trigger().focus();
    fireEvent.keyDown(trigger(), { key: "ArrowDown" });
    expect(document.activeElement).toBe(item("Save as Markdown"));
    fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });

    trigger().focus();
    fireEvent.keyDown(trigger(), { key: "ArrowUp" });
    expect(document.activeElement).toBe(item("Add to calendar"));
  });

  it("moves through the items with the arrows, wrapping, and to the ends with Home and End", () => {
    show(ready());
    trigger().focus();
    openMenu();
    expect(document.activeElement).toBe(item("Save as Markdown"));
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowDown" });
    expect(document.activeElement).toBe(item("Save as a web page"));
    fireEvent.keyDown(document.activeElement as Element, { key: "End" });
    expect(document.activeElement).toBe(item("Print or save as PDF"));
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowDown" });
    expect(document.activeElement).toBe(item("Save as Markdown"));
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowUp" });
    expect(document.activeElement).toBe(item("Print or save as PDF"));
    fireEvent.keyDown(document.activeElement as Element, { key: "Home" });
    expect(document.activeElement).toBe(item("Save as Markdown"));
  });

  it("closes on Escape with focus back on the button, and leaves the Escape from anything around it", () => {
    const around = vi.fn();
    show(ready(), undefined, { onKeyDown: around });
    trigger().focus();
    openMenu();

    const notCancelled = fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
    expect(notCancelled).toBe(false);
    expect(around).not.toHaveBeenCalled();
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(trigger());
  });

  it("closes on Tab, with focus on the button so the Tab moves on from there", () => {
    show(ready());
    trigger().focus();
    openMenu();
    fireEvent.keyDown(document.activeElement as Element, { key: "Tab" });
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(trigger());
  });

  it("closes on a press anywhere else", () => {
    show(ready());
    openMenu();
    fireEvent.mouseDown(document.body);
    expect(menu()).toBeNull();
  });

  it("closes when the button is pressed again", () => {
    show(ready());
    openMenu();
    openMenu();
    expect(menu()).toBeNull();
  });

  it("does nothing for a press on the line that only says something", () => {
    show({ status: "loading" });
    openMenu();
    fireEvent.click(item("Checking what can be saved..."));
    expect(menu()).not.toBeNull();
    expect(exportFile()).not.toHaveBeenCalled();
  });
});
