import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import "../i18n";

vi.mock("./WebSearchPanel", () => ({ WebSearchPanel: () => <div data-testid="web-search-panel" /> }));

import { BuiltInSearchNote } from "./BuiltInSearchNote";

afterEach(() => cleanup());

describe("BuiltInSearchNote", () => {
  it("says the built-in search already uses the key, and offers nothing to turn on", () => {
    render(<BuiltInSearchNote state="on" className="note" />);
    expect(screen.getByText("Web search is built in and already uses your Brave key.")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("steers to the built-in search when no key is stored", () => {
    render(<BuiltInSearchNote state="needs_key" className="note" />);
    expect(screen.getByText(/Add your Brave key to it instead/)).toBeTruthy();
    expect(screen.queryByTestId("web-search-panel")).toBeNull();
    fireEvent.click(screen.getByRole("button", { name: "Turn on web search" }));
    expect(screen.getByTestId("web-search-panel")).toBeTruthy();
  });

  it("points at the Knowledge switch when that is what is off", () => {
    render(<BuiltInSearchNote state="knowledge_off" className="note" />);
    expect(screen.getByText(/Turn on Knowledge in Settings/)).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });
});
