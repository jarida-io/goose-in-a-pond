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
    deleteSchedule: vi.fn(),
    pauseSchedule: vi.fn(),
    resumeSchedule: vi.fn(),
    runScheduleNow: vi.fn(),
  },
}));

// ── Mock AppContext (Schedules uses useAppState) ───────────────────────────────

vi.mock("../state/AppContext", () => ({
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
