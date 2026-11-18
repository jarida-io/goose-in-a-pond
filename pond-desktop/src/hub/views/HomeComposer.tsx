// The full-width "ask or run" bar. Desktop-only (see home-composer.css): the kiosk panels'
// height budget is already fully spent on .dash__aside/.dash__dock (see dashboard-grid.css's own
// account of how fragile that math is), so this doesn't touch that column at all -- it's a second,
// independent entry point that simply doesn't render below the width those panels use.

import { useState, type FormEvent, type KeyboardEvent } from "react";
import { HubIco } from "../primitives/HubIco";
import { micEl } from "../primitives/icons";
import { sendsMessage, useEnterSends } from "../state/enterSendsStore";
import "./home-composer.css";

export interface HomeComposerProps {
  /** Sends a prompt to chat. Without it Send has nothing to do. */
  onAsk?: (prompt: string) => void;
  /** Starts a voice turn. */
  onTalk: () => void;
}

export function HomeComposer({ onAsk, onTalk }: HomeComposerProps) {
  const [text, setText] = useState("");
  const { enterSends } = useEnterSends();

  function onKeyDown(e: KeyboardEvent<HTMLTextAreaElement>) {
    if (!sendsMessage(e, enterSends)) return;
    e.preventDefault();
    e.currentTarget.form?.requestSubmit();
  }

  function submit(e: FormEvent) {
    e.preventDefault();
    const prompt = text.trim();
    if (!prompt || !onAsk) return;
    onAsk(prompt);
    setText("");
  }

  return (
    <form className="hcomposer" onSubmit={submit}>
      <textarea
        className="hcomposer__input"
        placeholder="Ask me, or tell me what to run"
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={onKeyDown}
        rows={1}
        aria-label="Ask me, or tell me what to run"
      />
      <button type="button" className="hcomposer__talk" onClick={onTalk}>
        <HubIco d={micEl} size={16} color="var(--color-text-on-accent)" sw={2} />
        Hold to talk
      </button>
      <button type="submit" className="hcomposer__send" disabled={!text.trim()}>
        Send
      </button>
    </form>
  );
}
