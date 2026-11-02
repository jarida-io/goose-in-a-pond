import { afterEach, describe, it, expect, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ResearchModeChip } from "./ResearchModeChip";

afterEach(cleanup);

describe("ResearchModeChip's length choice", () => {
  it("marks the chosen length and reports a new one", () => {
    const onAnswer = vi.fn();
    render(<ResearchModeChip variant="classic" waiting={false} onLeave={() => {}} answer="auto" onAnswer={onAnswer} />);
    const group = screen.getByRole("group", { name: "How much to write" });
    expect(group).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pond decides" }).getAttribute("aria-pressed")).toBe("true");
    expect(screen.getByRole("button", { name: "Short answer" }).getAttribute("aria-pressed")).toBe("false");
    fireEvent.click(screen.getByRole("button", { name: "Full report" }));
    expect(onAnswer).toHaveBeenCalledWith("report");
  });

  it("still leaves research mode on Escape from a length", () => {
    const onLeave = vi.fn();
    render(<ResearchModeChip variant="hub" waiting={false} onLeave={onLeave} answer="short" onAnswer={() => {}} />);
    fireEvent.keyDown(screen.getByRole("button", { name: "Short answer" }), { key: "Escape" });
    expect(onLeave).toHaveBeenCalledTimes(1);
  });
});
