import { describe, it, expect, vi, afterEach } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { ComposerMenu } from "./ComposerMenu";
import type { ComposerMenuProps } from "./ComposerMenu";
import {
  DEEP_RESEARCH_HINT,
  DEEP_RESEARCH_NEEDS_NO_PICTURES,
  DEEP_RESEARCH_UNIDENTIFIED,
  PICTURES_NOT_IN_RESEARCH,
} from "./deepResearchCopy";

afterEach(() => {
  cleanup();
});

function props(over: Partial<ComposerMenuProps> = {}): ComposerMenuProps {
  return {
    variant: "classic",
    disabled: false,
    picturesTitle: "Attach image",
    picturesBlocked: false,
    hasPictures: false,
    access: "available",
    researchOn: false,
    onAddPictures: vi.fn(),
    onToggleResearch: vi.fn(),
    ...over,
  };
}

const trigger = () => screen.getByRole("button", { name: "Add to message" });
const menu = () => screen.queryByRole("menu", { name: "Add to message" });
const pictures = () => screen.getByRole("menuitem", { name: "Add pictures" });
const research = () => screen.getByRole("menuitemcheckbox", { name: "Deep research" });

/** Opens as a keypress on the focused trigger does: focus moves into the menu. */
function openByKeyboard() {
  trigger().focus();
  fireEvent.click(trigger());
}

describe("the plus button", () => {
  it("is a menu button, closed until it is pressed", () => {
    render(<ComposerMenu {...props()} />);
    expect(trigger().getAttribute("aria-haspopup")).toBe("menu");
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
    expect(menu()).toBeNull();
  });

  it("opens a labelled menu, and says so", () => {
    render(<ComposerMenu {...props()} />);
    fireEvent.click(trigger());
    expect(menu()).not.toBeNull();
    expect(trigger().getAttribute("aria-expanded")).toBe("true");
    expect(trigger().getAttribute("aria-controls")).toBe(menu()?.id);
  });

  it("closes when it is pressed again", () => {
    render(<ComposerMenu {...props()} />);
    fireEvent.click(trigger());
    fireEvent.click(trigger());
    expect(menu()).toBeNull();
    expect(trigger().getAttribute("aria-expanded")).toBe("false");
  });

  it("cannot be opened while the composer cannot take anything", () => {
    render(<ComposerMenu {...props({ disabled: true })} />);
    expect((trigger() as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(trigger());
    expect(menu()).toBeNull();
  });

  it("closes if the composer stops taking things while it is open", () => {
    const { rerender } = render(<ComposerMenu {...props()} />);
    fireEvent.click(trigger());
    rerender(<ComposerMenu {...props({ disabled: true })} />);
    expect(menu()).toBeNull();
  });

  it("is sized by the hub's touch targets and the classic icon button, by variant", () => {
    const { rerender } = render(<ComposerMenu {...props({ variant: "hub" })} />);
    expect(trigger().className).toContain("ch-attach");
    rerender(<ComposerMenu {...props({ variant: "classic" })} />);
    expect(trigger().className).toContain("ch-icon-btn");
  });
});

describe("what the menu holds", () => {
  it("offers Add pictures and Deep research when the pond offers research", () => {
    render(<ComposerMenu {...props()} />);
    fireEvent.click(trigger());
    expect(pictures()).toBeTruthy();
    expect(research().getAttribute("aria-checked")).toBe("false");
    expect(research().getAttribute("aria-disabled")).toBeNull();
    expect(screen.getByText(DEEP_RESEARCH_HINT)).toBeTruthy();
  });

  it.each(["hidden", "unknown"] as const)("leaves Deep research out while access is %s", (access) => {
    render(<ComposerMenu {...props({ access })} />);
    fireEvent.click(trigger());
    expect(pictures()).toBeTruthy();
    expect(screen.queryByRole("menuitemcheckbox")).toBeNull();
    expect(screen.queryByText("Deep research")).toBeNull();
  });

  it("shows Deep research disabled, with one line saying why, for a caller the pond cannot identify", () => {
    const onToggleResearch = vi.fn();
    render(<ComposerMenu {...props({ access: "unidentified", onToggleResearch })} />);
    fireEvent.click(trigger());

    expect(research().getAttribute("aria-disabled")).toBe("true");
    expect(research().getAttribute("aria-describedby")).toBeTruthy();
    expect(screen.getByText(DEEP_RESEARCH_UNIDENTIFIED)).toBeTruthy();
    fireEvent.click(research());
    expect(onToggleResearch).not.toHaveBeenCalled();
    // A refused press leaves the menu open, so the reason stays on screen.
    expect(menu()).not.toBeNull();
  });

  it("names the reason by the item's description, not its name", () => {
    render(<ComposerMenu {...props({ access: "unidentified" })} />);
    fireEvent.click(trigger());
    expect(research().getAttribute("aria-labelledby")).not.toBe(research().getAttribute("aria-describedby"));
    expect(screen.getByRole("menuitemcheckbox", { name: "Deep research" })).toBeTruthy();
  });
});

describe("Add pictures", () => {
  it("runs the picker and closes", () => {
    const onAddPictures = vi.fn();
    render(<ComposerMenu {...props({ onAddPictures })} />);
    fireEvent.click(trigger());
    fireEvent.click(pictures());
    expect(onAddPictures).toHaveBeenCalledTimes(1);
    expect(menu()).toBeNull();
  });

  it("carries the model's reason as its title and stays pressable when picture support is not ready", () => {
    const onAddPictures = vi.fn();
    render(<ComposerMenu {...props({ picturesBlocked: true, picturesTitle: "This model cannot look at pictures.", onAddPictures })} />);
    fireEvent.click(trigger());
    expect(pictures().getAttribute("title")).toBe("This model cannot look at pictures.");
    expect(pictures().getAttribute("aria-disabled")).toBe("true");
    expect(pictures().getAttribute("data-inert")).toBeNull();
    fireEvent.click(pictures());
    expect(onAddPictures).toHaveBeenCalledTimes(1);
  });

  it("carries no title when picture support is ready, so a tooltip never repeats its name", () => {
    render(<ComposerMenu {...props({ picturesTitle: "Add pictures" })} />);
    fireEvent.click(trigger());
    expect(pictures().getAttribute("title")).toBeNull();
    expect(pictures().getAttribute("aria-disabled")).toBeNull();
  });

  it("is off while research is on, with its reason in view", () => {
    const onAddPictures = vi.fn();
    render(<ComposerMenu {...props({ researchOn: true, onAddPictures })} />);
    fireEvent.click(trigger());
    expect(pictures().getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText(PICTURES_NOT_IN_RESEARCH)).toBeTruthy();
    fireEvent.click(pictures());
    expect(onAddPictures).not.toHaveBeenCalled();
    expect(menu()).not.toBeNull();
  });
});

describe("Deep research", () => {
  it("turns the mode on and closes", () => {
    const onToggleResearch = vi.fn();
    render(<ComposerMenu {...props({ onToggleResearch })} />);
    fireEvent.click(trigger());
    fireEvent.click(research());
    expect(onToggleResearch).toHaveBeenCalledTimes(1);
    expect(menu()).toBeNull();
  });

  it("shows a tick while it is on, and turns it off from the same place", () => {
    const onToggleResearch = vi.fn();
    render(<ComposerMenu {...props({ researchOn: true, onToggleResearch })} />);
    fireEvent.click(trigger());
    expect(research().getAttribute("aria-checked")).toBe("true");
    expect(research().getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(research());
    expect(onToggleResearch).toHaveBeenCalledTimes(1);
  });

  it("is off while pictures wait in the tray, with its reason in view", () => {
    const onToggleResearch = vi.fn();
    render(<ComposerMenu {...props({ hasPictures: true, onToggleResearch })} />);
    fireEvent.click(trigger());
    expect(research().getAttribute("aria-disabled")).toBe("true");
    expect(screen.getByText(DEEP_RESEARCH_NEEDS_NO_PICTURES)).toBeTruthy();
    fireEvent.click(research());
    expect(onToggleResearch).not.toHaveBeenCalled();
  });
});

describe("the keyboard", () => {
  it("moves focus to the first item when a key opens the menu", () => {
    render(<ComposerMenu {...props()} />);
    openByKeyboard();
    expect(document.activeElement).toBe(pictures());
  });

  it("opens on ArrowDown, to the first item, and on ArrowUp, to the last", () => {
    render(<ComposerMenu {...props()} />);
    trigger().focus();
    fireEvent.keyDown(trigger(), { key: "ArrowDown" });
    expect(document.activeElement).toBe(pictures());
    fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });

    trigger().focus();
    fireEvent.keyDown(trigger(), { key: "ArrowUp" });
    expect(document.activeElement).toBe(research());
  });

  it("steps through the items with the arrows, wrapping at both ends", () => {
    render(<ComposerMenu {...props()} />);
    openByKeyboard();
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowDown" });
    expect(document.activeElement).toBe(research());
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowDown" });
    expect(document.activeElement).toBe(pictures());
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowUp" });
    expect(document.activeElement).toBe(research());
  });

  it("jumps to the ends with Home and End", () => {
    render(<ComposerMenu {...props()} />);
    openByKeyboard();
    fireEvent.keyDown(document.activeElement as Element, { key: "End" });
    expect(document.activeElement).toBe(research());
    fireEvent.keyDown(document.activeElement as Element, { key: "Home" });
    expect(document.activeElement).toBe(pictures());
  });

  it("reaches a disabled item, so its reason can be read", () => {
    render(<ComposerMenu {...props({ access: "unidentified" })} />);
    openByKeyboard();
    fireEvent.keyDown(document.activeElement as Element, { key: "ArrowDown" });
    expect(document.activeElement).toBe(research());
  });

  it("closes on Escape and gives focus back to the plus", () => {
    render(<ComposerMenu {...props()} />);
    openByKeyboard();
    fireEvent.keyDown(document.activeElement as Element, { key: "Escape" });
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(trigger());
  });

  it("closes on Tab, with focus on the plus, so the Tab moves on from there", () => {
    render(<ComposerMenu {...props()} />);
    openByKeyboard();
    fireEvent.keyDown(document.activeElement as Element, { key: "Tab" });
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(trigger());
  });

  it("gives focus back to the plus after a choice made by key", () => {
    render(<ComposerMenu {...props()} />);
    openByKeyboard();
    fireEvent.click(pictures());
    expect(document.activeElement).toBe(trigger());
  });
});

describe("the pointer", () => {
  it("leaves focus in the text field, so the composer does not reflow under the press", () => {
    render(
      <div>
        <textarea aria-label="Message input" />
        <ComposerMenu {...props()} />
      </div>,
    );
    const field = screen.getByLabelText("Message input");
    field.focus();
    fireEvent.pointerDown(trigger());
    fireEvent.click(trigger());
    expect(menu()).not.toBeNull();
    expect(document.activeElement).toBe(field);
  });

  it("gives focus back to the text field when a press closes it", () => {
    render(
      <div>
        <textarea aria-label="Message input" />
        <ComposerMenu {...props()} />
      </div>,
    );
    const field = screen.getByLabelText("Message input");
    field.focus();
    fireEvent.pointerDown(trigger());
    fireEvent.click(trigger());
    fireEvent.click(research());
    expect(document.activeElement).toBe(field);
  });

  it("closes on a press anywhere else, without taking focus", () => {
    render(
      <div>
        <textarea aria-label="Message input" />
        <ComposerMenu {...props()} />
      </div>,
    );
    const field = screen.getByLabelText("Message input");
    field.focus();
    fireEvent.pointerDown(trigger());
    fireEvent.click(trigger());
    fireEvent.mouseDown(document.body);
    expect(menu()).toBeNull();
    expect(document.activeElement).toBe(field);
  });

  it("keeps a press inside the menu from pulling focus off the field", () => {
    render(<ComposerMenu {...props()} />);
    fireEvent.click(trigger());
    expect(fireEvent.mouseDown(menu() as Element)).toBe(false);
    expect(fireEvent.mouseDown(trigger())).toBe(false);
  });
});

describe("Escape beside a field that also reads it", () => {
  it("is taken by the open menu, and marked handled, so the field leaves research only on the next one", () => {
    const outside = vi.fn();
    render(
      <div onKeyDown={outside}>
        <textarea aria-label="Message input" />
        <ComposerMenu {...props({ researchOn: true })} />
      </div>,
    );
    const field = screen.getByLabelText("Message input");
    field.focus();
    fireEvent.pointerDown(trigger());
    fireEvent.click(trigger());

    const notCancelled = fireEvent.keyDown(field, { key: "Escape" });
    expect(notCancelled).toBe(false);
    expect(menu()).toBeNull();
    expect(outside).not.toHaveBeenCalled();

    // With the menu shut, the same key reaches the field untouched.
    expect(fireEvent.keyDown(field, { key: "Escape" })).toBe(true);
    expect(outside).toHaveBeenCalledTimes(1);
  });
});
