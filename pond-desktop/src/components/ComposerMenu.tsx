// The composer's plus: what can be added to the next message. The classic and hub composers share it.

import { useCallback, useEffect, useId, useRef, useState } from "react";
import { Check, ImagePlus, Plus, Telescope } from "lucide-react";
import type { LucideIcon } from "lucide-react";
import type { DeepResearchAccess } from "../hooks/useDeepResearch";
import {
  ADD_MENU_LABEL,
  ADD_PICTURES,
  DEEP_RESEARCH,
  DEEP_RESEARCH_HINT,
  DEEP_RESEARCH_NEEDS_NO_PICTURES,
  DEEP_RESEARCH_ON_HINT,
  DEEP_RESEARCH_UNIDENTIFIED,
  PICTURES_NOT_IN_RESEARCH,
} from "./deepResearchCopy";
import "./composerMenu.css";

interface Item {
  key: "pictures" | "research";
  role: "menuitem" | "menuitemcheckbox";
  Icon: LucideIcon;
  label: string;
  /** The line under the label: what it does, or why it cannot be pressed. */
  sub: string | null;
  /** Pressing does nothing; the line under the label says why. */
  inert: boolean;
  /** Still pressable, but picture support is not ready; the composer says why on the press. */
  muted?: boolean;
  checked?: boolean;
  title?: string;
  run: () => void;
}

export interface ComposerMenuProps {
  variant: "classic" | "hub";
  /** The pond is offline, or a reply is under way. */
  disabled: boolean;
  /** The model's reason when it cannot read pictures; shown as the item's title only then. */
  picturesTitle: string;
  picturesBlocked: boolean;
  hasPictures: boolean;
  access: DeepResearchAccess;
  researchOn: boolean;
  onAddPictures: () => void;
  onToggleResearch: () => void;
}

export function ComposerMenu({
  variant,
  disabled,
  picturesTitle,
  picturesBlocked,
  hasPictures,
  access,
  researchOn,
  onAddPictures,
  onToggleResearch,
}: ComposerMenuProps) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);
  // Where focus was when the menu opened: the text field after a press, the trigger after a key.
  const returnRef = useRef<HTMLElement | null>(null);
  const landOn = useRef<"first" | "last" | null>(null);
  const viaPointer = useRef(false);
  const menuId = useId();

  const items: Item[] = [
    {
      key: "pictures",
      role: "menuitem",
      Icon: ImagePlus,
      label: ADD_PICTURES,
      sub: researchOn ? PICTURES_NOT_IN_RESEARCH : null,
      inert: researchOn,
      muted: !researchOn && picturesBlocked,
      title: researchOn || !picturesBlocked ? undefined : picturesTitle,
      run: onAddPictures,
    },
  ];
  if (access === "available" || access === "unidentified") {
    const unidentified = access === "unidentified";
    items.push({
      key: "research",
      role: "menuitemcheckbox",
      Icon: Telescope,
      label: DEEP_RESEARCH,
      sub: unidentified
        ? DEEP_RESEARCH_UNIDENTIFIED
        : researchOn
          ? DEEP_RESEARCH_ON_HINT
          : hasPictures
            ? DEEP_RESEARCH_NEEDS_NO_PICTURES
            : DEEP_RESEARCH_HINT,
      inert: unidentified || (!researchOn && hasPictures),
      checked: researchOn,
      run: onToggleResearch,
    });
  }

  const close = useCallback((restoreFocus: boolean) => {
    setOpen(false);
    if (!restoreFocus) return;
    const was = returnRef.current;
    const back = was && was !== document.body && was.isConnected ? was : triggerRef.current;
    back?.focus();
  }, []);

  function openMenu(land: "first" | "last" | null) {
    returnRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    landOn.current = land;
    setOpen(true);
  }

  const focusItem = useCallback((at: "first" | "last" | number) => {
    const list = itemRefs.current.filter((el): el is HTMLButtonElement => el !== null);
    if (list.length === 0) return;
    const index = at === "first" ? 0 : at === "last" ? list.length - 1 : (at + list.length) % list.length;
    list[index].focus();
  }, []);

  useEffect(() => {
    if (!open) return;
    if (landOn.current) focusItem(landOn.current);
    landOn.current = null;
  }, [open, focusItem]);

  useEffect(() => {
    if (disabled) setOpen(false);
  }, [disabled]);

  useEffect(() => {
    if (!open) return;
    function away(e: Event) {
      if (!wrapRef.current?.contains(e.target as Node)) close(false);
    }
    // Capture, and stop the event: the field beside the menu also reads Escape, to leave research.
    function onEscape(e: KeyboardEvent) {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      close(true);
    }
    document.addEventListener("mousedown", away);
    document.addEventListener("focusin", away);
    document.addEventListener("keydown", onEscape, true);
    return () => {
      document.removeEventListener("mousedown", away);
      document.removeEventListener("focusin", away);
      document.removeEventListener("keydown", onEscape, true);
    };
  }, [open, close]);

  function onTriggerClick() {
    const byPointer = viaPointer.current;
    viaPointer.current = false;
    if (open) {
      close(true);
      return;
    }
    // A press leaves focus in the text field: moving it would reflow the composer under the pointer.
    openMenu(byPointer ? null : "first");
  }

  function onTriggerKeyDown(e: React.KeyboardEvent<HTMLButtonElement>) {
    viaPointer.current = false;
    if (e.key !== "ArrowDown" && e.key !== "ArrowUp") return;
    e.preventDefault();
    const land = e.key === "ArrowDown" ? "first" : "last";
    if (open) focusItem(land);
    else openMenu(land);
  }

  function onPanelKeyDown(e: React.KeyboardEvent<HTMLDivElement>) {
    const at = itemRefs.current.findIndex((el) => el === document.activeElement);
    switch (e.key) {
      case "ArrowDown":
        e.preventDefault();
        focusItem(at + 1);
        break;
      case "ArrowUp":
        e.preventDefault();
        focusItem(at < 0 ? "last" : at - 1);
        break;
      case "Home":
        e.preventDefault();
        focusItem("first");
        break;
      case "End":
        e.preventDefault();
        focusItem("last");
        break;
      case "Tab":
        // Focus goes back to the trigger first, so the Tab itself moves on from there.
        close(true);
        break;
    }
  }

  function choose(item: Item) {
    if (item.inert) return;
    close(true);
    item.run();
  }

  const triggerClass = variant === "hub" ? "ch-attach" : "ch-icon-btn";

  return (
    <div ref={wrapRef} className={`cmenu cmenu--${variant}`}>
      <button
        ref={triggerRef}
        type="button"
        className={`${triggerClass} cmenu__trigger`}
        aria-label={ADD_MENU_LABEL}
        title={ADD_MENU_LABEL}
        aria-haspopup="menu"
        aria-expanded={open}
        aria-controls={open ? menuId : undefined}
        disabled={disabled}
        onPointerDown={() => {
          viaPointer.current = true;
        }}
        onMouseDown={(e) => e.preventDefault()}
        onClick={onTriggerClick}
        onKeyDown={onTriggerKeyDown}
      >
        <Plus size={variant === "hub" ? 18 : 17} aria-hidden="true" />
      </button>

      {open && (
        <div
          id={menuId}
          role="menu"
          aria-label={ADD_MENU_LABEL}
          className="cmenu__panel"
          onKeyDown={onPanelKeyDown}
          onMouseDown={(e) => e.preventDefault()}
        >
          {items.map((item, i) => {
            const labelId = `${menuId}-${item.key}`;
            const subId = `${labelId}-why`;
            return (
              <button
                key={item.key}
                ref={(el) => {
                  itemRefs.current[i] = el;
                }}
                type="button"
                role={item.role}
                className="cmenu__item"
                tabIndex={-1}
                aria-labelledby={labelId}
                aria-describedby={item.sub ? subId : undefined}
                aria-checked={item.role === "menuitemcheckbox" ? Boolean(item.checked) : undefined}
                aria-disabled={item.inert || item.muted ? true : undefined}
                data-inert={item.inert ? "true" : undefined}
                title={item.title}
                onClick={() => choose(item)}
              >
                <item.Icon size={16} aria-hidden="true" className="cmenu__icon" />
                <span className="cmenu__text">
                  <span id={labelId} className="cmenu__label">
                    {item.label}
                  </span>
                  {item.sub && (
                    <span id={subId} className="cmenu__sub">
                      {item.sub}
                    </span>
                  )}
                </span>
                {item.checked && <Check size={14} aria-hidden="true" className="cmenu__tick" />}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
