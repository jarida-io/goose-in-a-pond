import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, act } from "@testing-library/react";
import { sendsMessage, useEnterSends } from "./enterSendsStore";

function Composer({ onSend }: { onSend: () => void }) {
  const { enterSends } = useEnterSends();
  return (
    <textarea
      aria-label="Message input"
      onKeyDown={(e) => {
        if (sendsMessage(e, enterSends)) onSend();
      }}
    />
  );
}

function Setter() {
  const { setEnterSends } = useEnterSends();
  return (
    <>
      <button onClick={() => setEnterSends(false)}>off</button>
      <button onClick={() => setEnterSends(true)}>on</button>
    </>
  );
}

function setup() {
  const onSend = vi.fn();
  render(<><Composer onSend={onSend} /><Setter /></>);
  return { onSend, field: screen.getByLabelText("Message input") };
}

afterEach(() => {
  act(() => screen.queryByText("on")?.click());
  cleanup();
  localStorage.clear();
});

describe("Enter in the chat composer", () => {
  it("sends on Enter by default and keeps Shift+Enter for a new line", () => {
    const { onSend, field } = setup();
    fireEvent.keyDown(field, { key: "Enter", shiftKey: true });
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.keyDown(field, { key: "Enter" });
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("starts a new line on Enter when switched off, and still sends on Ctrl+Enter", () => {
    const { onSend, field } = setup();
    act(() => screen.getByText("off").click());
    expect(localStorage.getItem("goosehub_enter_sends")).toBe("false");

    fireEvent.keyDown(field, { key: "Enter" });
    expect(onSend).not.toHaveBeenCalled();
    fireEvent.keyDown(field, { key: "Enter", ctrlKey: true });
    expect(onSend).toHaveBeenCalledTimes(1);
  });

  it("never sends on the Enter that confirms an IME composition", () => {
    const { onSend, field } = setup();
    fireEvent.keyDown(field, { key: "Enter", isComposing: true });
    expect(onSend).not.toHaveBeenCalled();
  });
});
