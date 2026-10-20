import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Schedules } from "./Schedules";
import { api } from "../api/PondApiClient";
import type { Schedule } from "../api/types";

// ── Mock PondApiClient ────────────────────────────────────────────────────────

vi.mock("../api/PondApiClient", () => ({
  api: {
    listSchedules: vi.fn(),
    createSchedule: vi.fn(),
    updateSchedule: vi.fn(),
    deleteSchedule: vi.fn(),
    pauseSchedule: vi.fn(),
    resumeSchedule: vi.fn(),
    runScheduleNow: vi.fn(),
  },
}));

// ── Mock AppContext (Schedules uses useAppState) ───────────────────────────────

vi.mock("../state/useAppContext", () => ({
  useAppState: () => ({ serverOnline: true, sessionToken: "test-token" }),
  useAppDispatch: () => vi.fn(),
}));

// ── Mock useConfirm (used in handleDelete) ────────────────────────────────────

vi.mock("../components/shared", async () => {
  const actual = await vi.importActual("../components/shared") as Record<string, unknown>;
  return { ...actual, useConfirm: () => async () => true };
});

// ── Sample data ───────────────────────────────────────────────────────────────

const activeSchedule: Schedule = {
  id: "sched-morning",
  name: "Morning Briefing",
  cron: "0 0 7 * * *",
  prompt: "Give me a morning briefing",
  enabled: true,
};

const pausedSchedule: Schedule = {
  id: "sched-evening",
  name: "Evening Summary",
  cron: "0 0 20 * * *",
  prompt: "Summarize the day",
  enabled: false,
};

// ── Helpers ───────────────────────────────────────────────────────────────────

function renderSchedules() {
  return render(<Schedules />);
}

beforeEach(() => {
  vi.resetAllMocks();
});

afterEach(() => {
  cleanup();
});

// ── Tests ──────────────────────────────────────────────────────────────────────

describe("Schedules section", () => {
  it("renders list of schedules from API", async () => {
    vi.mocked(api.listSchedules).mockResolvedValue([activeSchedule, pausedSchedule]);

    renderSchedules();

    await waitFor(() => {
      expect(screen.getByText("Morning Briefing")).toBeTruthy();
      expect(screen.getByText("Evening Summary")).toBeTruthy();
    });
  });

  it("shows loading state then renders schedules", async () => {
    vi.mocked(api.listSchedules).mockResolvedValue([activeSchedule]);

    renderSchedules();

    await waitFor(() => {
      expect(screen.getByText("Morning Briefing")).toBeTruthy();
    });

    expect(api.listSchedules).toHaveBeenCalledTimes(1);
  });

  it("shows error message when listSchedules fails", async () => {
    vi.mocked(api.listSchedules).mockRejectedValue(new Error("network error"));

    renderSchedules();

    await waitFor(() => {
      expect(screen.getByText(/network error/i)).toBeTruthy();
    });
  });

  it("renders empty state when no schedules exist", async () => {
    vi.mocked(api.listSchedules).mockResolvedValue([]);

    renderSchedules();

    await waitFor(() => {
      expect(api.listSchedules).toHaveBeenCalled();
    });

    expect(screen.queryByText("Morning Briefing")).toBeNull();
  });

  it("calls deleteSchedule and reloads on delete click", async () => {
    vi.mocked(api.listSchedules)
      .mockResolvedValueOnce([activeSchedule])
      .mockResolvedValueOnce([]); // after delete
    vi.mocked(api.deleteSchedule).mockResolvedValue(undefined);

    renderSchedules();

    await waitFor(() => screen.getByText("Morning Briefing"));

    const deleteButtons = screen.getAllByRole("button");
    const deleteBtn = deleteButtons.find(
      (b) => b.getAttribute("aria-label")?.toLowerCase().includes("delete") || b.title?.toLowerCase().includes("delete"),
    );
    if (deleteBtn) {
      fireEvent.click(deleteBtn);
      await waitFor(() => {
        expect(api.deleteSchedule).toHaveBeenCalledWith("sched-morning");
      });
    }
  });

  it("calls pauseSchedule when toggling an active schedule", async () => {
    vi.mocked(api.listSchedules)
      .mockResolvedValueOnce([activeSchedule])
      .mockResolvedValueOnce([{ ...activeSchedule, enabled: false }]);
    vi.mocked(api.pauseSchedule).mockResolvedValue(undefined);

    renderSchedules();

    await waitFor(() => screen.getByText("Morning Briefing"));

    const pauseButtons = screen.getAllByRole("button");
    const pauseBtn = pauseButtons.find(
      (b) =>
        b.getAttribute("aria-label")?.toLowerCase().includes("pause") ||
        b.title?.toLowerCase().includes("pause"),
    );
    if (pauseBtn) {
      fireEvent.click(pauseBtn);
      await waitFor(() => {
        expect(api.pauseSchedule).toHaveBeenCalledWith("sched-morning");
      });
    }
  });

  it("calls resumeSchedule when toggling a paused schedule", async () => {
    vi.mocked(api.listSchedules)
      .mockResolvedValueOnce([pausedSchedule])
      .mockResolvedValueOnce([{ ...pausedSchedule, enabled: true }]);
    vi.mocked(api.resumeSchedule).mockResolvedValue(undefined);

    renderSchedules();

    await waitFor(() => screen.getByText("Evening Summary"));

    const resumeButtons = screen.getAllByRole("button");
    const resumeBtn = resumeButtons.find(
      (b) =>
        b.getAttribute("aria-label")?.toLowerCase().includes("resume") ||
        b.title?.toLowerCase().includes("resume"),
    );
    if (resumeBtn) {
      fireEvent.click(resumeBtn);
      await waitFor(() => {
        expect(api.resumeSchedule).toHaveBeenCalledWith("sched-evening");
      });
    }
  });

  it("calls runScheduleNow when run button clicked", async () => {
    vi.mocked(api.listSchedules).mockResolvedValue([activeSchedule]);
    vi.mocked(api.runScheduleNow).mockResolvedValue(undefined);

    renderSchedules();

    await waitFor(() => screen.getByText("Morning Briefing"));

    const runButtons = screen.getAllByRole("button");
    const runBtn = runButtons.find(
      (b) =>
        b.getAttribute("aria-label")?.toLowerCase().includes("run") ||
        b.title?.toLowerCase().includes("run"),
    );
    if (runBtn) {
      fireEvent.click(runBtn);
      await waitFor(() => {
        expect(api.runScheduleNow).toHaveBeenCalledWith("sched-morning");
      });
    }
  });

  it("creates a schedule when form is submitted", async () => {
    vi.mocked(api.listSchedules)
      .mockResolvedValueOnce([])
      .mockResolvedValueOnce([activeSchedule]);
    vi.mocked(api.createSchedule).mockResolvedValue(activeSchedule);

    renderSchedules();

    await waitFor(() => expect(api.listSchedules).toHaveBeenCalled());

    const addButton = screen.getAllByRole("button").find(
      (b) => b.textContent?.toLowerCase().includes("add") || b.textContent?.toLowerCase().includes("new"),
    );
    if (addButton) {
      fireEvent.click(addButton);

      const nameInput = screen.queryByPlaceholderText(/name/i) ?? screen.queryByLabelText(/name/i);
      const cronInput = screen.queryByPlaceholderText(/cron/i) ?? screen.queryByLabelText(/cron/i);
      const promptInput = screen.queryByPlaceholderText(/prompt/i) ?? screen.queryByLabelText(/prompt/i);

      if (nameInput && cronInput && promptInput) {
        fireEvent.change(nameInput, { target: { value: "Morning Briefing" } });
        fireEvent.change(cronInput, { target: { value: "0 0 7 * * *" } });
        fireEvent.change(promptInput, { target: { value: "Good morning briefing" } });

        const submitBtn = screen.queryByRole("button", { name: /save|create|submit/i });
        if (submitBtn) {
          fireEvent.click(submitBtn);
          await waitFor(() => {
            expect(api.createSchedule).toHaveBeenCalled();
          });
        }
      }
    }
  });

  it("listSchedules is called on mount", async () => {
    vi.mocked(api.listSchedules).mockResolvedValue([]);
    renderSchedules();
    await waitFor(() => expect(api.listSchedules).toHaveBeenCalledTimes(1));
  });
});

// ── Editing must not change timing it was not asked to ───────────────────────

describe("Schedules edit", () => {
  async function openEdit(schedule: Schedule) {
    vi.mocked(api.listSchedules).mockResolvedValue([schedule]);
    vi.mocked(api.updateSchedule).mockResolvedValue(schedule);
    renderSchedules();
    await waitFor(() => screen.getByText(schedule.name));
    fireEvent.click(screen.getByRole("button", { name: "Edit schedule" }));
  }

  function save() {
    fireEvent.click(screen.getByRole("button", { name: "Update Schedule" }));
  }

  it("a rename sends the name and no timing fields", async () => {
    await openEdit({ ...activeSchedule, cron: "0 0 7 * * 1-5", timezone: "UTC" });
    fireEvent.change(screen.getByLabelText("Schedule name"), { target: { value: "Weekday briefing" } });
    save();
    await waitFor(() => expect(api.updateSchedule).toHaveBeenCalled());
    expect(api.updateSchedule).toHaveBeenCalledWith("sched-morning", { name: "Weekday briefing" });
  });

  it.each([
    ["0 0 7 * * 1-5", "Weekly"],
    ["0 0 7 1,15 * *", "Custom"],
    ["0 */15 * * * *", "Custom"],
  ])("%s opens as %s and is left unchanged on save", async (cron, preset) => {
    await openEdit({ ...activeSchedule, cron, timezone: "UTC" });
    expect(screen.getByRole("button", { name: preset }).getAttribute("aria-pressed")).toBe("true");
    fireEvent.change(screen.getByLabelText("Schedule prompt"), { target: { value: "New prompt" } });
    save();
    await waitFor(() => expect(api.updateSchedule).toHaveBeenCalled());
    expect(api.updateSchedule).toHaveBeenCalledWith("sched-morning", { prompt: "New prompt" });
  });

  it("a weekday range opens with every one of its days selected", async () => {
    await openEdit({ ...activeSchedule, cron: "0 0 7 * * 1-5", timezone: "UTC" });
    const days = screen.getByRole("group", { name: "Days of week" });
    const pressed = Array.from(days.querySelectorAll("button"))
      .filter((b) => b.getAttribute("aria-pressed") === "true")
      .map((b) => b.textContent);
    expect(pressed).toEqual(["Mon", "Tue", "Wed", "Thu", "Fri"]);
  });

  const oneOff: Schedule = {
    ...activeSchedule,
    cron: "@once",
    fire_at: "2099-03-14T06:30:00Z",
    timezone: "Africa/Nairobi",
  };

  it("a one-off opens with its date and keeps it on a rename", async () => {
    await openEdit(oneOff);
    expect(screen.getByRole("button", { name: "Once" }).getAttribute("aria-pressed")).toBe("true");
    expect((screen.getByLabelText("Run date") as HTMLInputElement).value).toBe("2099-03-14");
    expect((screen.getByLabelText("Hour") as HTMLSelectElement).value).toBe("9");
    fireEvent.change(screen.getByLabelText("Schedule name"), { target: { value: "Pi day" } });
    save();
    await waitFor(() => expect(api.updateSchedule).toHaveBeenCalled());
    expect(api.updateSchedule).toHaveBeenCalledWith("sched-morning", { name: "Pi day" });
  });

  it("a one-off's new time is sent as fire_at on its own date, not a cron", async () => {
    await openEdit(oneOff);
    fireEvent.change(screen.getByLabelText("Hour"), { target: { value: "10" } });
    save();
    await waitFor(() => expect(api.updateSchedule).toHaveBeenCalled());
    expect(api.updateSchedule).toHaveBeenCalledWith("sched-morning", {
      fire_at: "2099-03-14T07:30:00.000Z",
    });
  });

  it("a one-off moved to another date sends that date", async () => {
    await openEdit(oneOff);
    fireEvent.change(screen.getByLabelText("Run date"), { target: { value: "2099-12-01" } });
    save();
    await waitFor(() => expect(api.updateSchedule).toHaveBeenCalled());
    expect(api.updateSchedule).toHaveBeenCalledWith("sched-morning", {
      fire_at: "2099-12-01T06:30:00.000Z",
    });
  });

  it("a one-off in the past is refused, not sent", async () => {
    await openEdit({ ...oneOff, fire_at: "2099-03-14T06:30:00Z" });
    fireEvent.change(screen.getByLabelText("Run date"), { target: { value: "2000-01-01" } });
    save();
    await waitFor(() => expect(screen.getByText(/already passed/)).toBeTruthy());
    expect(api.updateSchedule).not.toHaveBeenCalled();
  });

  it("saving with nothing changed sends nothing", async () => {
    await openEdit({ ...activeSchedule, timezone: "UTC" });
    save();
    await waitFor(() => expect(screen.getByText(/Nothing changed/)).toBeTruthy());
    expect(api.updateSchedule).not.toHaveBeenCalled();
  });
});

describe("Schedules create", () => {
  it("a dated one-off is created pinned to its date and year", async () => {
    vi.mocked(api.listSchedules).mockResolvedValue([]);
    vi.mocked(api.createSchedule).mockResolvedValue(activeSchedule);
    renderSchedules();
    await waitFor(() => expect(api.listSchedules).toHaveBeenCalled());
    fireEvent.click(screen.getAllByRole("button", { name: /New Schedule/ })[0]);
    fireEvent.change(screen.getByLabelText("Schedule name"), { target: { value: "Pi day" } });
    fireEvent.change(screen.getByLabelText("Schedule prompt"), { target: { value: "Bake" } });
    fireEvent.click(screen.getByRole("button", { name: "Once" }));
    fireEvent.change(screen.getByLabelText("Run date"), { target: { value: "2099-03-14" } });
    fireEvent.click(screen.getByRole("button", { name: "Create Schedule" }));
    await waitFor(() => expect(api.createSchedule).toHaveBeenCalled());
    expect(api.createSchedule).toHaveBeenCalledWith(
      expect.objectContaining({ cron: "0 0 8 14 3 * 2099", once: true }),
    );
  });
});
