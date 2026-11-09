// A member's web watches: the list, and one watch with what is on its page, what changed, and its
// filters, check interval, pause and delete. Both shells draw this; `variant` sets the density.

import { useCallback, useEffect, useId, useRef, useState } from "react";
import {
  ArrowLeft,
  ChevronRight,
  ExternalLink,
  Eye,
  MessageSquare,
  Minus,
  Pause,
  Play,
  Plus,
  Trash2,
  TrendingDown,
  TrendingUp,
  TriangleAlert,
} from "lucide-react";
import "./watches.css";
import { api } from "../api/PondApiClient";
import type { WebWatch, WebWatchChange, WebWatchDetail, WebWatchItem } from "../api/types";
import { useConfirm } from "../components/shared";
import { useAppDispatch, useAppState } from "../state/useAppContext";
import {
  ALL_WATCHES,
  CHANGES_HEADING,
  CHECK_EVERY,
  CHECK_EVERY_HOURS,
  DELETE,
  DELETE_TITLE,
  EDIT_HEADING,
  EMPTY_PAGE,
  FOUND_HEADING,
  LOADING_WATCH,
  LOADING_WATCHES,
  LOOKING_FOR,
  MATCH,
  MATCH_HINT,
  MAX_MILEAGE,
  MAX_PRICE,
  MIN_YEAR,
  NOTHING_FOUND,
  NO_CHANGES,
  NO_WATCHES_BODY,
  NO_WATCHES_TITLE,
  OPEN_PAGE,
  PAUSE,
  RESUME,
  RETRY,
  SAVED,
  SAVE_CHANGES,
  SAVING,
  START_IN_CHAT,
  WATCHES_INTRO,
  WATCHES_TITLE,
  WORDS,
  WORDS_HINT,
  checkWords,
  countWords,
  deleteQuestion,
  everyWords,
  failureWords,
  filtersFrom,
  formFrom,
  foundAgo,
  itemFacts,
  siteOf,
  type WatchForm,
} from "./watchWords";

type Variant = "classic" | "hub";

type Loaded<T> = { status: "loading" } | { status: "failed"; message: string } | { status: "ready"; value: T };

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const isWeb = (url: string | undefined): url is string => !!url && /^https?:\/\//i.test(url);

interface Props {
  variant: Variant;
  /** Watches start in a conversation, where the page is read first. */
  onOpenChat?: () => void;
}

export function WatchesScreen({ variant, onOpenChat }: Props) {
  const dispatch = useAppDispatch();
  const { requestedWatch } = useAppState();
  const [list, setList] = useState<Loaded<WebWatch[]>>({ status: "loading" });
  const [openId, setOpenId] = useState<string | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const wasOpen = useRef(false);

  const reload = useCallback(async () => {
    try {
      const watches = await api.listWebWatches();
      setList({ status: "ready", value: Array.isArray(watches) ? watches : [] });
    } catch (e) {
      setList({ status: "failed", message: reasonOf(e) });
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  // A watch a notice named, taken once so going back to the list stays on the list.
  useEffect(() => {
    if (!requestedWatch) return;
    dispatch({ type: "TAKE_WATCH_REQUEST" });
    setOpenId(requestedWatch);
  }, [requestedWatch, dispatch]);

  useEffect(() => {
    if (wasOpen.current && openId === null) headingRef.current?.focus();
    wasOpen.current = openId !== null;
  }, [openId]);

  function back() {
    setOpenId(null);
    void reload();
  }

  if (openId) {
    return (
      <div className={`wpage wpage--${variant}`}>
        <WatchDetail id={openId} variant={variant} onBack={back} />
      </div>
    );
  }

  return (
    <div className={`wpage wpage--${variant}`}>
      <header className="wpage__head">
        <h1 ref={headingRef} className="wpage__title" tabIndex={-1}>
          {WATCHES_TITLE}
        </h1>
        <p className="wpage__intro">{WATCHES_INTRO}</p>
      </header>

      {list.status === "loading" ? <p className="wpage__note" role="status">{LOADING_WATCHES}</p> : null}

      {list.status === "failed" ? (
        <div className="wpage__problem" role="alert">
          <span>{list.message}</span>
          <button type="button" className="ink-btn ink-btn--quiet wpage__btn" onClick={() => void reload()}>
            {RETRY}
          </button>
        </div>
      ) : null}

      {list.status === "ready" && list.value.length === 0 ? (
        <div className="wpage__empty">
          <span className="wpage__empty-icon" aria-hidden="true">
            <Eye size={24} />
          </span>
          <h2 className="wpage__empty-title">{NO_WATCHES_TITLE}</h2>
          <p className="wpage__empty-body">{NO_WATCHES_BODY}</p>
          {onOpenChat ? (
            <button type="button" className="ink-btn ink-btn--primary wpage__btn" onClick={onOpenChat}>
              <MessageSquare size={16} aria-hidden="true" />
              {START_IN_CHAT}
            </button>
          ) : null}
        </div>
      ) : null}

      {list.status === "ready" && list.value.length > 0 ? (
        <ul className="wwatch-list" aria-label={WATCHES_TITLE}>
          {list.value.map((watch) => (
            <li key={watch.id}>
              <WatchRow watch={watch} onOpen={() => setOpenId(watch.id)} />
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function WatchRow({ watch, onOpen }: { watch: WebWatch; onOpen: () => void }) {
  const failing = failureWords(watch);
  return (
    <button type="button" className="ink-edge ink-card ink-pressable wwatch-row" data-paused={watch.paused} onClick={onOpen}>
      <span className="wwatch-row__main">
        <span className="wwatch-row__title">{watch.title}</span>
        <span className="wwatch-row__site">{siteOf(watch.address)}</span>
        <span className="wwatch-row__filters">{watch.filters_words}</span>
        <span className="wwatch-row__meta">
          {countWords(watch)}. {checkWords(watch)}
        </span>
        {failing ? (
          <span className="wwatch-row__failing">
            <TriangleAlert size={14} aria-hidden="true" />
            {failing}
          </span>
        ) : null}
      </span>
      <ChevronRight size={18} aria-hidden="true" className="wwatch-row__go" />
    </button>
  );
}

function WatchDetail({ id, variant, onBack }: { id: string; variant: Variant; onBack: () => void }) {
  const confirm = useConfirm();
  const [watch, setWatch] = useState<Loaded<WebWatchDetail>>({ status: "loading" });
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);
  const titleRef = useRef<HTMLHeadingElement>(null);

  const load = useCallback(async () => {
    setWatch({ status: "loading" });
    try {
      setWatch({ status: "ready", value: await api.getWebWatch(id) });
    } catch (e) {
      setWatch({ status: "failed", message: reasonOf(e) });
    }
  }, [id]);

  useEffect(() => {
    void load();
  }, [load]);

  // Only the first draw moves focus; an edit keeps it where the member is.
  const focused = useRef(false);
  useEffect(() => {
    if (watch.status !== "ready" || focused.current) return;
    focused.current = true;
    titleRef.current?.focus();
  }, [watch.status]);

  /** The edit answers the watch without its items and changes, which an edit does not touch. */
  function merge(next: WebWatch) {
    setWatch((current) => (current.status === "ready" ? { status: "ready", value: { ...current.value, ...next } } : current));
  }

  async function togglePause(current: WebWatchDetail) {
    setBusy(true);
    setProblem(null);
    try {
      merge(await api.editWebWatch(current.id, { paused: !current.paused }));
    } catch (e) {
      setProblem(reasonOf(e));
    } finally {
      setBusy(false);
    }
  }

  async function remove(current: WebWatchDetail) {
    const ok = await confirm(deleteQuestion(current.title), { title: DELETE_TITLE, confirmLabel: DELETE, destructive: true });
    if (!ok) return;
    setBusy(true);
    setProblem(null);
    try {
      await api.deleteWebWatch(current.id);
      onBack();
    } catch (e) {
      setProblem(reasonOf(e));
      setBusy(false);
    }
  }

  return (
    <>
      <div className="wpage__bar">
        <button type="button" className="wpage__back" onClick={onBack}>
          <ArrowLeft size={16} aria-hidden="true" />
          {ALL_WATCHES}
        </button>
      </div>

      {watch.status === "loading" ? <p className="wpage__note" role="status">{LOADING_WATCH}</p> : null}

      {watch.status === "failed" ? (
        <div className="wpage__problem" role="alert">
          <span>{watch.message}</span>
          <button type="button" className="ink-btn ink-btn--quiet wpage__btn" onClick={() => void load()}>
            {RETRY}
          </button>
        </div>
      ) : null}

      {watch.status === "ready" ? (
        <article className="wwatch" aria-labelledby={`${id}-title`}>
          <header className="wpage__head">
            <h1 id={`${id}-title`} ref={titleRef} className="wpage__title" tabIndex={-1}>
              {watch.value.title}
            </h1>
            <p className="wpage__intro">{checkWords(watch.value)}</p>
            {failureWords(watch.value) ? (
              <p className="wwatch__failing">
                <TriangleAlert size={16} aria-hidden="true" />
                {failureWords(watch.value)}
              </p>
            ) : null}
            <div className="wpage__actions">
              {isWeb(watch.value.address) ? (
                <a className="wpage__link" href={watch.value.address} target="_blank" rel="noopener noreferrer">
                  <ExternalLink size={16} aria-hidden="true" />
                  {OPEN_PAGE}
                </a>
              ) : null}
              <button
                type="button"
                className="ink-btn ink-btn--quiet wpage__btn"
                disabled={busy}
                onClick={() => void togglePause(watch.value)}
              >
                {watch.value.paused ? <Play size={16} aria-hidden="true" /> : <Pause size={16} aria-hidden="true" />}
                {watch.value.paused ? RESUME : PAUSE}
              </button>
              <button
                type="button"
                className="ink-btn ink-btn--quiet wpage__btn wpage__btn--danger"
                disabled={busy}
                onClick={() => void remove(watch.value)}
              >
                <Trash2 size={16} aria-hidden="true" />
                {DELETE}
              </button>
            </div>
            {problem ? (
              <p className="wpage__problem" role="alert">
                {problem}
              </p>
            ) : null}
          </header>

          <WatchEditor key={watch.value.id} watch={watch.value} variant={variant} onSaved={merge} />
          <ChangeList changes={watch.value.changes ?? []} />
          <ItemList items={watch.value.items ?? []} checked={watch.value.last_checked_at !== null} />
        </article>
      ) : null}
    </>
  );
}

function WatchEditor({ watch, variant, onSaved }: { watch: WebWatch; variant: Variant; onSaved: (w: WebWatch) => void }) {
  const ids = useId();
  const [form, setForm] = useState<WatchForm>(() => formFrom(watch));
  const [saving, setSaving] = useState(false);
  const [note, setNote] = useState<{ kind: "saved" | "problem"; text: string } | null>(null);
  const start = useRef(formFrom(watch));
  const dirty = JSON.stringify(form) !== JSON.stringify(start.current);

  function set<K extends keyof WatchForm>(key: K, value: WatchForm[K]) {
    setForm((f) => ({ ...f, [key]: value }));
    setNote(null);
  }

  async function save(e: React.FormEvent) {
    e.preventDefault();
    setSaving(true);
    setNote(null);
    try {
      const saved = await api.editWebWatch(watch.id, {
        looking_for: form.lookingFor,
        filters: filtersFrom(form),
        interval_hours: form.interval,
      });
      start.current = formFrom(saved);
      setForm(formFrom(saved));
      onSaved(saved);
      setNote({ kind: "saved", text: SAVED });
    } catch (err) {
      setNote({ kind: "problem", text: reasonOf(err) });
    } finally {
      setSaving(false);
    }
  }

  const field = (key: "maxPrice" | "minYear" | "maxMileage", label: string) => (
    <div className="wform__field">
      <label className="wform__label" htmlFor={`${ids}-${key}`}>{label}</label>
      <input
        id={`${ids}-${key}`}
        className="wform__input"
        inputMode="numeric"
        value={form[key]}
        onChange={(e) => set(key, e.target.value)}
      />
    </div>
  );

  const intervals: number[] = CHECK_EVERY_HOURS.includes(form.interval as (typeof CHECK_EVERY_HOURS)[number])
    ? [...CHECK_EVERY_HOURS]
    : [...CHECK_EVERY_HOURS, form.interval].sort((a, b) => a - b);

  return (
    <section className={`wform wform--${variant}`} aria-labelledby={`${ids}-h`}>
      <h2 id={`${ids}-h`} className="wpage__h2">
        {EDIT_HEADING}
      </h2>
      <form onSubmit={(e) => void save(e)} className="wform__grid">
        <div className="wform__field wform__field--wide">
          <label className="wform__label" htmlFor={`${ids}-looking`}>{LOOKING_FOR}</label>
          <input
            id={`${ids}-looking`}
            className="wform__input"
            value={form.lookingFor}
            onChange={(e) => set("lookingFor", e.target.value)}
          />
        </div>
        <div className="wform__field wform__field--wide">
          <label className="wform__label" htmlFor={`${ids}-words`}>{WORDS}</label>
          <input
            id={`${ids}-words`}
            className="wform__input"
            value={form.words}
            aria-describedby={`${ids}-words-hint`}
            onChange={(e) => set("words", e.target.value)}
          />
          <span id={`${ids}-words-hint`} className="wform__hint">
            {WORDS_HINT}
          </span>
        </div>
        {field("maxPrice", MAX_PRICE)}
        {field("minYear", MIN_YEAR)}
        {field("maxMileage", MAX_MILEAGE)}
        <div className="wform__field">
          <label className="wform__label" htmlFor={`${ids}-every`}>{CHECK_EVERY}</label>
          <select
            id={`${ids}-every`}
            className="wform__input"
            value={form.interval}
            onChange={(e) => set("interval", Number(e.target.value))}
          >
            {intervals.map((h) => (
              <option key={h} value={h}>
                {everyWords(h)}
              </option>
            ))}
          </select>
        </div>
        <div className="wform__save">
          <button type="submit" className="ink-btn ink-btn--primary wpage__btn" disabled={!dirty || saving}>
            {saving ? SAVING : SAVE_CHANGES}
          </button>
          {note ? (
            <span className={`wform__note wform__note--${note.kind}`} role={note.kind === "problem" ? "alert" : "status"}>
              {note.text}
            </span>
          ) : null}
        </div>
      </form>
    </section>
  );
}

const CHANGE_ICON = {
  new: <Plus size={16} aria-hidden="true" />,
  gone: <Minus size={16} aria-hidden="true" />,
} as const;

function changeIcon(change: WebWatchChange) {
  if (change.kind !== "price_changed") return CHANGE_ICON[change.kind];
  return change.line.startsWith("Price down") ? (
    <TrendingDown size={16} aria-hidden="true" />
  ) : (
    <TrendingUp size={16} aria-hidden="true" />
  );
}

function ChangeList({ changes }: { changes: WebWatchChange[] }) {
  const ids = useId();
  return (
    <section className="wsec" aria-labelledby={`${ids}-h`}>
      <h2 id={`${ids}-h`} className="wpage__h2">
        {CHANGES_HEADING}
      </h2>
      {changes.length === 0 ? (
        <p className="wpage__note">{NO_CHANGES}</p>
      ) : (
        <ol className="wchanges">
          {changes.map((change) => (
            <li key={change.id} className="wchange" data-kind={change.kind}>
              <span className="wchange__icon">{changeIcon(change)}</span>
              <span className="wchange__text">
                {isWeb(change.item.link) ? (
                  <a href={change.item.link} target="_blank" rel="noopener noreferrer">
                    {change.line}
                  </a>
                ) : (
                  change.line
                )}
                <span className="wchange__when">{foundAgo(change.found_at)}</span>
              </span>
              {change.matched ? (
                <span className="wchange__match" title={MATCH_HINT}>
                  {MATCH}
                </span>
              ) : null}
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

function ItemList({ items, checked }: { items: WebWatchItem[]; checked: boolean }) {
  const ids = useId();
  return (
    <section className="wsec" aria-labelledby={`${ids}-h`}>
      <h2 id={`${ids}-h`} className="wpage__h2">
        {FOUND_HEADING} {items.length > 0 ? <span className="wpage__count">{items.length}</span> : null}
      </h2>
      {items.length === 0 ? (
        <p className="wpage__note">{checked ? EMPTY_PAGE : NOTHING_FOUND}</p>
      ) : (
        <ul className="witems">
          {items.map((item, i) => (
            <li key={`${item.link ?? item.title}-${i}`} className="witem">
              <span className="witem__title">
                {isWeb(item.link) ? (
                  <a href={item.link} target="_blank" rel="noopener noreferrer">
                    {item.title}
                  </a>
                ) : (
                  item.title
                )}
              </span>
              {itemFacts(item) ? <span className="witem__facts">{itemFacts(item)}</span> : null}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
