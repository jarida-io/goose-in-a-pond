import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import { ToolCallChip } from "./ToolCallChip";
import type { ContextCard } from "../state/reducer";

afterEach(() => cleanup());

/** A tool-call card as Chat builds one, after `tool_result` has filled it in. */
function card(over: Partial<ContextCard> = {}): ContextCard {
  return {
    id: "c1",
    tool: "giap-knowledge__compute_answer",
    data: { query: "3 miles in km", primary: "4.828 km", primary_title: "Result" },
    timestamp_ms: 0,
    ...over,
  } as ContextCard;
}

/** Open the chip, which is what reveals the body. */
function expand() {
  fireEvent.click(screen.getByRole("button", { name: /click to expand/i }));
}

describe("ToolCallChip — MCP-UI cards in the conversation", () => {
  it("renders the registered card when the server hinted one", () => {
    render(<ToolCallChip card={card({ renderHint: "wolfram" })} />);
    expand();

    expect(screen.getByText("4.828 km")).toBeTruthy();
    expect(screen.queryByText(/"primary"/)).toBeNull();
  });

  it("falls back to the raw text when no card matches the hint", () => {
    const c = card({ tool: "giap-system__run_shell_command", renderHint: undefined, data: { result: "exit 0" } });
    render(<ToolCallChip card={c} />);
    expand();

    expect(screen.getByText("exit 0")).toBeTruthy();
  });

  it("shows the text rather than an empty card when the hint has no data yet", () => {
    // `renderHint` only arrives with tool_result; an empty card would draw its loading state.
    const c = card({ renderHint: undefined, data: { result: "partial output" } });
    render(<ToolCallChip card={c} />);
    expand();

    expect(screen.getByText("partial output")).toBeTruthy();
  });

  it("collapsed by default, so a long result never takes over the bubble", () => {
    render(<ToolCallChip card={card({ renderHint: "wolfram" })} />);
    expect(screen.queryByText("4.828 km")).toBeNull();
  });

  it("a click on the card's own content does not collapse it", () => {
    const c = card({
      renderHint: "wolfram",
      data: {
        query: "mercury",
        primary: "Mercury (planet)",
        pods: [{ title: "Orbital period", text: "87.97 days" }],
      },
    });
    render(<ToolCallChip card={c} />);
    expand();

    // Plain content: a suggestion chip stops propagation itself, so it would pass anyway.
    fireEvent.click(screen.getByText("87.97 days"));

    expect(screen.getByText("Mercury (planet)")).toBeTruthy();
  });

  it("passes the follow-up up to the composer rather than calling a tool", () => {
    const onAction = vi.fn();
    const c = card({
      renderHint: "wolfram",
      data: {
        query: "mercury",
        primary: "Mercury (planet)",
        explore: [{ id: "w2", label: "a chemical element", kind: "assumption", verb: "interpret as" }],
      },
    });
    render(<ToolCallChip card={c} onAction={onAction} />);
    expand();
    fireEvent.click(screen.getByRole("button", { name: /a chemical element/i }));

    // Nothing resolves ids, so the prompt must carry the whole request and no id to quote back.
    const sent = onAction.mock.calls[0][0] as string;
    expect(sent).toContain("a chemical element");
    expect(sent).not.toContain("w2");
  });
});
