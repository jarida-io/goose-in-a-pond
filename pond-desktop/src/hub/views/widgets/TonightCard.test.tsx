import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { TonightCard } from "./TonightCard";
import { placeOnEvening, untilText } from "../../state/evening";
import { api } from "../../../api/PondApiClient";
import type { Schedule } from "../../../api/types";

vi.mock("../../../api/PondApiClient", () => ({ api: { listSchedules: vi.fn() } }));

describe("untilText", () => {
  it("counts minutes, then hours, and drops the minutes a few hours out", () => {
    expect(untilText(20_000)).toBe("now");
    expect(untilText(25 * 60_000)).toBe("in 25 min");
    expect(untilText(135 * 60_000)).toBe("in 2 h 15 min");
    expect(untilText(120 * 60_000)).toBe("in 2 h");
    expect(untilText(305 * 60_000)).toBe("in 5 h");
  });
});

describe("placeOnEvening", () => {
  it("places a moment between now and midnight", () => {
    const now = new Date(2026, 10, 11, 18, 0).getTime();
    expect(placeOnEvening(now, now)).toBe(0);
    expect(placeOnEvening(new Date(2026, 10, 11, 21, 0).getTime(), now)).toBe(50);
    expect(placeOnEvening(new Date(2026, 10, 12, 0, 0).getTime(), now)).toBe(100);
  });
});

describe("TonightCard", () => {
  afterEach(() => {
    cleanup();
    vi.useRealTimers();
    vi.clearAllMocks();
  });

  it("leads with how soon the next routine runs, and marks each on the evening", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    vi.setSystemTime(new Date(2026, 10, 11, 16, 0));
    const at = (h: number) => new Date(2026, 10, 11, h, 0).toISOString();
    const schedule = (id: string, name: string, next: string) => ({ id, name, enabled: true, next_run: next }) as unknown as Schedule;
    vi.mocked(api.listSchedules).mockResolvedValue([schedule("a", "News", at(19)), schedule("b", "Lights off", at(23))]);

    const { container } = render(<TonightCard onNavigate={() => {}} />);
    expect(await screen.findByText("in 3 h")).toBeTruthy();
    expect(screen.getByText("News")).toBeTruthy();
    expect(screen.getByText("1 more after that.")).toBeTruthy();
    expect(container.querySelectorAll(".tonight__mark")).toHaveLength(2);
    expect(container.querySelector(".tonight__mark[data-next]")).toBeTruthy();
  });

  it("calls an empty evening quiet rather than showing a count of nothing", async () => {
    vi.mocked(api.listSchedules).mockResolvedValue([]);
    render(<TonightCard onNavigate={() => {}} />);
    expect(await screen.findByText("A quiet night")).toBeTruthy();
  });
});
