/** Whether Enter sends a chat message or starts a new line, kept per device like the theme. */
import { useSyncExternalStore } from "react";
import type { KeyboardEvent } from "react";

const KEY_ENTER_SENDS = "goosehub_enter_sends";

function readFromStorage(): boolean {
  try {
    return localStorage.getItem(KEY_ENTER_SENDS) !== "false";
  } catch {
    return true;
  }
}

let _enterSends = readFromStorage();
const _listeners = new Set<() => void>();

function setEnterSends(on: boolean): void {
  try { localStorage.setItem(KEY_ENTER_SENDS, String(on)); } catch { /* ignore */ }
  _enterSends = on;
  _listeners.forEach((fn) => fn());
}

function _subscribe(listener: () => void): () => void {
  _listeners.add(listener);
  return () => _listeners.delete(listener);
}

function _snapshot(): boolean {
  return _enterSends;
}

export function useEnterSends(): { enterSends: boolean; setEnterSends: (on: boolean) => void } {
  const enterSends = useSyncExternalStore(_subscribe, _snapshot, _snapshot);
  return { enterSends, setEnterSends };
}

/**
 * Whether this key press sends the message. Ctrl/Cmd+Enter always sends; plain Enter sends only
 * when `enterSends` is on, and Shift+Enter is always a new line. Enter that confirms an IME
 * composition never sends.
 */
export function sendsMessage(e: KeyboardEvent, enterSends: boolean): boolean {
  if (e.key !== "Enter" || e.nativeEvent.isComposing) return false;
  if (e.metaKey || e.ctrlKey) return true;
  return enterSends && !e.shiftKey;
}
