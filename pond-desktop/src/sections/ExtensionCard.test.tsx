import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import "../i18n";
import type { Extension } from "../api/types";

vi.mock("../shell", () => ({ isDesktopShell: () => true, invoke: vi.fn() }));

import { ExtensionCard } from "./Extensions";

afterEach(() => cleanup());

const card = (ext: Extension) =>
  render(
    <ExtensionCard ext={ext} onToggle={() => {}} onDelete={() => {}} disabled={false} />,
  );

/** Music on a Pond that is not a Mac: running, with no tools on purpose. */
const music: Extension = {
  name: "music",
  kind: "stdio",
  description: "",
  tools: [],
  enabled: true,
  status: "no_tools",
  last_error: null,
  status_reason: "No music service can be controlled by the assistant on this computer: Apple Music needs a Mac.",
};

describe("ExtensionCard status", () => {
  it("shows a running extension without tools as neutral, with its reason", () => {
    const { container } = card(music);
    expect(screen.getByText("Running, no assistant tools here")).toBeTruthy();
    expect(screen.getByText(/Apple Music needs a Mac/)).toBeTruthy();
    expect(container.querySelector(".ext-card__status-dot--no_tools")).toBeTruthy();
    expect(container.querySelector(".ext-card__status-dot--error")).toBeNull();
    expect(container.querySelector(".ext-card__error-line")).toBeNull();
    expect(screen.queryByText(/failed to load/i)).toBeNull();
  });

  it("falls back to a generic reason when the extension gave none", () => {
    card({ ...music, status_reason: null });
    expect(screen.getByText(/gives the assistant nothing to use on this Pond/)).toBeTruthy();
  });

  it("still shows a failed start as an error", () => {
    const { container } = card({
      ...music,
      status: "error",
      status_reason: null,
      last_error: "Extension failed to load",
    });
    expect(container.querySelector(".ext-card__status-dot--error")).toBeTruthy();
    expect(screen.getByText("Extension failed to load")).toBeTruthy();
    expect(screen.queryByText("Running, no assistant tools here")).toBeNull();
  });
});

describe("ExtensionCard redundancy", () => {
  const brave: Extension = { ...music, name: "brave-search", status: "connected", status_reason: null };

  it("marks an extension the built-in search makes redundant and offers its removal", () => {
    const onDelete = vi.fn();
    render(
      <ExtensionCard ext={brave} onToggle={() => {}} onDelete={onDelete} redundant disabled={false} />,
    );
    expect(screen.getByText("Not needed")).toBeTruthy();
    expect(screen.getByText(/already uses your Brave key/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Remove brave-search" }));
    expect(onDelete).toHaveBeenCalledWith("brave-search");
  });

  it("says nothing about redundancy otherwise", () => {
    card(brave);
    expect(screen.queryByText("Not needed")).toBeNull();
  });
});
