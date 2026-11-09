// A member's saved cards and the household's shared ones, by collection. A recipe, a listing and a
// place each have their own face; the owner renames, moves, shares and deletes. Both shells draw this.

import { useCallback, useEffect, useId, useMemo, useRef, useState } from "react";
import {
  ArrowLeft,
  Bookmark,
  ChefHat,
  ExternalLink,
  Eye,
  FileText,
  MapPin,
  MessageSquare,
  Pencil,
  StickyNote,
  Tag,
  Trash2,
  Users,
} from "lucide-react";
import "../watches/watches.css";
import "./saved.css";
import { api } from "../api/PondApiClient";
import type { SavedCollection, SavedItem, SavedItemEdit, SavedKind } from "../api/types";
import { MarkdownText } from "../components/MarkdownText";
import { useConfirm } from "../components/shared";
import {
  ADDRESS,
  ALL,
  ALL_SAVED,
  CANCEL,
  COLLECTIONS_LABEL,
  DELETE,
  DELETE_TITLE,
  HOURS,
  INGREDIENTS,
  KIND_LABEL,
  LOADING_SAVED,
  LOCATION,
  LOOSE,
  MILEAGE,
  MOVE,
  MOVE_LABEL,
  NAME_LABEL,
  NEW_COLLECTION,
  NEW_COLLECTION_NAME,
  NOTES,
  NOT_YOURS,
  NO_SAVED_BODY,
  NO_SAVED_TITLE,
  OPEN_LISTING,
  OPEN_PLACE,
  PRICE_WATCHED,
  RENAME,
  RETRY,
  SAVED_INTRO,
  SAVED_TITLE,
  SAVE_NAME,
  SERVES,
  SHARED,
  SHARE_HINT,
  SHARE_LABEL,
  SOURCES,
  START_IN_CHAT,
  STEPS,
  TIME,
  WATCHING_PRICE,
  WATCH_PRICE,
  YEAR,
  MANAGE_CARD,
  cardFacts,
  deleteQuestion,
  excerpt,
  priceOf,
  sameCollection,
} from "./savedWords";

type Variant = "classic" | "hub";

type Loaded = { status: "loading" } | { status: "failed"; message: string } | { status: "ready"; items: SavedItem[]; collections: SavedCollection[] };

/** A filter: every card, the cards in no collection, or one collection by name. */
type Filter = { kind: "all" } | { kind: "loose" } | { kind: "collection"; name: string };

// Picker values are positions, never names, so no collection's name can be taken for one.
const LOOSE_VALUE = "loose";
const NEW_VALUE = "new";

function reasonOf(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

const isWeb = (url: string | undefined): url is string => !!url && /^https?:\/\//i.test(url);

const KIND_ICON: Record<SavedKind, React.ReactNode> = {
  recipe: <ChefHat size={16} aria-hidden="true" />,
  listing: <Tag size={16} aria-hidden="true" />,
  place: <MapPin size={16} aria-hidden="true" />,
  report: <FileText size={16} aria-hidden="true" />,
  note: <StickyNote size={16} aria-hidden="true" />,
};

interface Props {
  variant: Variant;
  onOpenChat?: () => void;
}

export function SavedScreen({ variant, onOpenChat }: Props) {
  const [loaded, setLoaded] = useState<Loaded>({ status: "loading" });
  const [filter, setFilter] = useState<Filter>({ kind: "all" });
  const [openId, setOpenId] = useState<string | null>(null);
  const headingRef = useRef<HTMLHeadingElement>(null);
  const wasOpen = useRef(false);

  const reload = useCallback(async () => {
    try {
      const [items, collections] = await Promise.all([api.listSavedItems(), api.listSavedCollections()]);
      setLoaded({
        status: "ready",
        items: Array.isArray(items) ? items : [],
        collections: Array.isArray(collections) ? collections : [],
      });
    } catch (e) {
      setLoaded({ status: "failed", message: reasonOf(e) });
    }
  }, []);

  useEffect(() => {
    void reload();
  }, [reload]);

  useEffect(() => {
    if (wasOpen.current && openId === null) headingRef.current?.focus();
    wasOpen.current = openId !== null;
  }, [openId]);

  /** A card as the pond now has it; the collections are counted again since a move changes them. */
  function replace(item: SavedItem) {
    setLoaded((current) =>
      current.status === "ready" ? { ...current, items: current.items.map((i) => (i.id === item.id ? item : i)) } : current,
    );
    void api
      .listSavedCollections()
      .then((collections) =>
        setLoaded((current) => (current.status === "ready" && Array.isArray(collections) ? { ...current, collections } : current)),
      )
      .catch(() => undefined);
  }

  function removed(id: string) {
    setOpenId(null);
    setLoaded((current) => (current.status === "ready" ? { ...current, items: current.items.filter((i) => i.id !== id) } : current));
    void reload();
  }

  const shown = useMemo(() => {
    if (loaded.status !== "ready") return [];
    if (filter.kind === "all") return loaded.items;
    if (filter.kind === "loose") return loaded.items.filter((i) => !i.collection);
    return loaded.items.filter((i) => sameCollection(i.collection, filter.name));
  }, [loaded, filter]);

  // A collection that has emptied drops out of the chips; its filter falls back to everything.
  useEffect(() => {
    if (loaded.status !== "ready" || filter.kind !== "collection") return;
    if (!loaded.collections.some((c) => sameCollection(c.name, filter.name))) setFilter({ kind: "all" });
  }, [loaded, filter]);

  const open = loaded.status === "ready" && openId ? loaded.items.find((i) => i.id === openId) : undefined;
  if (open && loaded.status === "ready") {
    return (
      <div className={`wpage wpage--${variant}`}>
        <SavedDetail
          item={open}
          collections={loaded.collections}
          variant={variant}
          onBack={() => setOpenId(null)}
          onChanged={replace}
          onRemoved={removed}
        />
      </div>
    );
  }

  const looseCount = loaded.status === "ready" ? loaded.items.filter((i) => !i.collection).length : 0;

  return (
    <div className={`wpage wpage--${variant}`}>
      <header className="wpage__head">
        <h1 ref={headingRef} className="wpage__title" tabIndex={-1}>
          {SAVED_TITLE}
        </h1>
        <p className="wpage__intro">{SAVED_INTRO}</p>
      </header>

      {loaded.status === "loading" ? <p className="wpage__note" role="status">{LOADING_SAVED}</p> : null}

      {loaded.status === "failed" ? (
        <div className="wpage__problem" role="alert">
          <span>{loaded.message}</span>
          <button type="button" className="ink-btn ink-btn--quiet wpage__btn" onClick={() => void reload()}>
            {RETRY}
          </button>
        </div>
      ) : null}

      {loaded.status === "ready" && loaded.items.length === 0 ? (
        <div className="wpage__empty">
          <span className="wpage__empty-icon" aria-hidden="true">
            <Bookmark size={24} />
          </span>
          <h2 className="wpage__empty-title">{NO_SAVED_TITLE}</h2>
          <p className="wpage__empty-body">{NO_SAVED_BODY}</p>
          {onOpenChat ? (
            <button type="button" className="ink-btn ink-btn--primary wpage__btn" onClick={onOpenChat}>
              <MessageSquare size={16} aria-hidden="true" />
              {START_IN_CHAT}
            </button>
          ) : null}
        </div>
      ) : null}

      {loaded.status === "ready" && loaded.items.length > 0 ? (
        <>
          <nav className="scoll" aria-label={COLLECTIONS_LABEL}>
            <CollectionChip label={ALL} count={loaded.items.length} on={filter.kind === "all"} onPick={() => setFilter({ kind: "all" })} />
            {loaded.collections.map((c) => (
              <CollectionChip
                key={c.name}
                label={c.name}
                count={c.count}
                on={filter.kind === "collection" && sameCollection(c.name, filter.name)}
                onPick={() => setFilter({ kind: "collection", name: c.name })}
              />
            ))}
            {looseCount > 0 && loaded.collections.length > 0 ? (
              <CollectionChip label={LOOSE} count={looseCount} on={filter.kind === "loose"} onPick={() => setFilter({ kind: "loose" })} />
            ) : null}
          </nav>
          <ul className="sgrid" aria-label={SAVED_TITLE}>
            {shown.map((item) => (
              <li key={item.id}>
                <SavedCard item={item} variant={variant} onOpen={() => setOpenId(item.id)} />
              </li>
            ))}
          </ul>
        </>
      ) : null}
    </div>
  );
}

function CollectionChip({ label, count, on, onPick }: { label: string; count: number; on: boolean; onPick: () => void }) {
  return (
    <button type="button" className="scoll__chip reach" aria-pressed={on} onClick={onPick}>
      <span>{label}</span>
      <span className="scoll__count">{count}</span>
    </button>
  );
}

function CardFoot({ item }: { item: SavedItem }) {
  if (!item.collection && !item.shared) return null;
  return (
    <span className="scard__foot">
      {item.collection ? <span className="scard__tag">{item.collection}</span> : null}
      {item.shared ? (
        <span className="scard__tag">
          <Users size={12} aria-hidden="true" />
          {SHARED}
        </span>
      ) : null}
    </span>
  );
}

/** A card's face: a recipe leads with its size, a listing with its price, a place with where it is. */
function SavedCard({ item, variant, onOpen }: { item: SavedItem; variant: Variant; onOpen: () => void }) {
  const facts = cardFacts(item);
  const words = item.kind === "report" || item.kind === "note" || !facts ? excerpt(item.body) : "";
  return (
    <button type="button" className={`ink-edge ink-card ink-pressable scard scard--${item.kind} scard--${variant}`} onClick={onOpen}>
      <span className="scard__kind">
        {KIND_ICON[item.kind]}
        {KIND_LABEL[item.kind]}
      </span>
      <span className="scard__title">{item.title}</span>
      {item.kind === "listing" && priceOf(item) ? <span className="scard__price">{priceOf(item)}</span> : null}
      {facts ? <span className="scard__facts">{facts}</span> : null}
      {words ? <span className="scard__excerpt">{words}</span> : null}
      <CardFoot item={item} />
    </button>
  );
}

interface DetailProps {
  item: SavedItem;
  collections: SavedCollection[];
  variant: Variant;
  onBack: () => void;
  onChanged: (item: SavedItem) => void;
  onRemoved: (id: string) => void;
}

function SavedDetail({ item, collections, variant, onBack, onChanged, onRemoved }: DetailProps) {
  const confirm = useConfirm();
  const titleId = useId();
  const titleRef = useRef<HTMLHeadingElement>(null);
  const [busy, setBusy] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  useEffect(() => {
    titleRef.current?.focus();
  }, []);

  async function edit(change: SavedItemEdit): Promise<boolean> {
    setBusy(true);
    setProblem(null);
    try {
      onChanged(await api.editSavedItem(item.id, change));
      return true;
    } catch (e) {
      setProblem(reasonOf(e));
      return false;
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    const ok = await confirm(deleteQuestion(item.title), { title: DELETE_TITLE, confirmLabel: DELETE, destructive: true });
    if (!ok) return;
    setBusy(true);
    setProblem(null);
    try {
      await api.deleteSavedItem(item.id);
      onRemoved(item.id);
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
          {ALL_SAVED}
        </button>
      </div>

      <article className={`sdetail sdetail--${item.kind}`} aria-labelledby={titleId}>
        <header className="wpage__head">
          <span className="scard__kind">
            {KIND_ICON[item.kind]}
            {KIND_LABEL[item.kind]}
          </span>
          <h1 id={titleId} ref={titleRef} className="wpage__title" tabIndex={-1}>
            {item.title}
          </h1>
          <CardFoot item={item} />
        </header>

        <KindBody item={item} variant={variant} />

        {item.body.trim() !== "" ? (
          <section className="wsec" aria-label={NOTES}>
            {item.kind === "recipe" ? <h2 className="wpage__h2">{NOTES}</h2> : null}
            <MarkdownText source={item.body} headings="report" className="sdetail__body" />
          </section>
        ) : null}

        {item.sources && item.sources.length > 0 ? (
          <section className="wsec" aria-label={SOURCES}>
            <h2 className="wpage__h2">{SOURCES}</h2>
            <ul className="ssources">
              {item.sources.filter((s) => isWeb(s.url)).map((s) => (
                <li key={s.url}>
                  <a href={s.url} target="_blank" rel="noopener noreferrer">
                    {s.title}
                  </a>
                </li>
              ))}
            </ul>
          </section>
        ) : null}

        {item.mine ? (
          <OwnerControls item={item} collections={collections} busy={busy} onEdit={edit} onDelete={() => void remove()} />
        ) : (
          <p className="wpage__note">{NOT_YOURS}</p>
        )}
        {problem ? (
          <p className="wpage__problem" role="alert">
            {problem}
          </p>
        ) : null}
      </article>
    </>
  );
}

function Facts({ rows }: { rows: Array<[string, string | undefined]> }) {
  const shown = rows.filter((r): r is [string, string] => !!r[1]);
  if (shown.length === 0) return null;
  return (
    <dl className="sfacts">
      {shown.map(([name, value]) => (
        <div key={name} className="sfacts__row">
          <dt>{name}</dt>
          <dd>{value}</dd>
        </div>
      ))}
    </dl>
  );
}

function KindBody({ item, variant }: { item: SavedItem; variant: Variant }) {
  switch (item.kind) {
    case "recipe":
      return (
        <div className="srecipe">
          <Facts rows={[[SERVES, item.servings], [TIME, item.time]]} />
          {item.ingredients && item.ingredients.length > 0 ? (
            <section className="wsec">
              <h2 className="wpage__h2">
                {INGREDIENTS} <span className="wpage__count">{item.ingredients.length}</span>
              </h2>
              <ul className="srecipe__ingredients">
                {item.ingredients.map((line, i) => (
                  <li key={i}>{line}</li>
                ))}
              </ul>
            </section>
          ) : null}
          {item.steps && item.steps.length > 0 ? (
            <section className="wsec">
              <h2 className="wpage__h2">{STEPS}</h2>
              <ol className="srecipe__steps">
                {item.steps.map((line, i) => (
                  <li key={i}>{line}</li>
                ))}
              </ol>
            </section>
          ) : null}
        </div>
      );
    case "listing":
      return (
        <div className="slisting">
          {priceOf(item) ? <p className="slisting__price">{priceOf(item)}</p> : null}
          <Facts rows={[[YEAR, item.year], [MILEAGE, item.mileage], [LOCATION, item.location]]} />
          <div className="wpage__actions">
            {isWeb(item.link) ? (
              <a className="wpage__link" href={item.link} target="_blank" rel="noopener noreferrer">
                <ExternalLink size={16} aria-hidden="true" />
                {OPEN_LISTING}
              </a>
            ) : null}
            {isWeb(item.link) ? <WatchPrice id={item.id} variant={variant} /> : null}
          </div>
        </div>
      );
    case "place":
      return (
        <div className="splace">
          <Facts rows={[[ADDRESS, item.address], [HOURS, item.hours]]} />
          {isWeb(item.link) ? (
            <div className="wpage__actions">
              <a className="wpage__link" href={item.link} target="_blank" rel="noopener noreferrer">
                <ExternalLink size={16} aria-hidden="true" />
                {OPEN_PLACE}
              </a>
            </div>
          ) : null}
        </div>
      );
    default:
      return null;
  }
}

function WatchPrice({ id, variant }: { id: string; variant: Variant }) {
  const [state, setState] = useState<{ step: "idle" | "busy" | "done" } | { step: "failed"; message: string }>({ step: "idle" });
  async function watch() {
    setState({ step: "busy" });
    try {
      await api.watchSavedItem(id);
      setState({ step: "done" });
    } catch (e) {
      setState({ step: "failed", message: reasonOf(e) });
    }
  }
  if (state.step === "done") {
    return (
      <span className="swatch__done" role="status">
        <Eye size={16} aria-hidden="true" />
        {PRICE_WATCHED}
      </span>
    );
  }
  return (
    <>
      <button
        type="button"
        className={`ink-btn ink-btn--quiet wpage__btn swatch--${variant}`}
        disabled={state.step === "busy"}
        onClick={() => void watch()}
      >
        <Eye size={16} aria-hidden="true" />
        {state.step === "busy" ? WATCHING_PRICE : WATCH_PRICE}
      </button>
      {state.step === "failed" ? (
        <span className="wpage__problem" role="alert">
          {state.message}
        </span>
      ) : null}
    </>
  );
}

function OwnerControls({
  item,
  collections,
  busy,
  onEdit,
  onDelete,
}: {
  item: SavedItem;
  collections: SavedCollection[];
  busy: boolean;
  onEdit: (change: SavedItemEdit) => Promise<boolean>;
  onDelete: () => void;
}) {
  const ids = useId();
  const [renaming, setRenaming] = useState(false);
  const [name, setName] = useState(item.title);
  const [creating, setCreating] = useState(false);
  const [newName, setNewName] = useState("");

  const options = useMemo(() => {
    const names = collections.map((c) => c.name);
    if (item.collection && !names.some((n) => sameCollection(n, item.collection ?? ""))) names.push(item.collection);
    return names;
  }, [collections, item.collection]);

  const current = item.collection ? options.findIndex((n) => sameCollection(n, item.collection ?? "")) : -1;
  const selected = creating ? NEW_VALUE : current >= 0 ? `c${current}` : LOOSE_VALUE;

  async function rename(e: React.FormEvent) {
    e.preventDefault();
    const title = name.trim();
    if (!title || title === item.title) {
      setRenaming(false);
      return;
    }
    if (await onEdit({ title })) setRenaming(false);
  }

  function pick(value: string) {
    if (value === NEW_VALUE) {
      setCreating(true);
      return;
    }
    setCreating(false);
    void onEdit({ collection: value === LOOSE_VALUE ? null : options[Number(value.slice(1))] });
  }

  async function moveToNew(e: React.FormEvent) {
    e.preventDefault();
    const collection = newName.trim();
    if (!collection) return;
    if (await onEdit({ collection })) {
      setNewName("");
      setCreating(false);
    }
  }

  return (
    <section className="sowner" aria-label={MANAGE_CARD}>
      {renaming ? (
        <form className="sowner__row" onSubmit={(e) => void rename(e)}>
          <div className="wform__field sowner__grow">
            <label className="wform__label" htmlFor={`${ids}-name`}>{NAME_LABEL}</label>
            <input id={`${ids}-name`} className="wform__input" value={name} onChange={(e) => setName(e.target.value)} autoFocus />
          </div>
          <button type="submit" className="ink-btn ink-btn--primary wpage__btn" disabled={busy}>
            {SAVE_NAME}
          </button>
          <button
            type="button"
            className="ink-btn ink-btn--quiet wpage__btn"
            onClick={() => {
              setName(item.title);
              setRenaming(false);
            }}
          >
            {CANCEL}
          </button>
        </form>
      ) : null}

      <div className="sowner__row">
        <div className="wform__field sowner__grow">
          <label className="wform__label" htmlFor={`${ids}-move`}>{MOVE_LABEL}</label>
          <select id={`${ids}-move`} className="wform__input" value={selected} disabled={busy} onChange={(e) => pick(e.target.value)}>
            <option value={LOOSE_VALUE}>{LOOSE}</option>
            {options.map((n, i) => (
              <option key={n} value={`c${i}`}>
                {n}
              </option>
            ))}
            <option value={NEW_VALUE}>{NEW_COLLECTION}</option>
          </select>
        </div>
        {creating ? (
          <form className="sowner__row sowner__grow" onSubmit={(e) => void moveToNew(e)}>
            <div className="wform__field sowner__grow">
              <label className="wform__label" htmlFor={`${ids}-new`}>{NEW_COLLECTION_NAME}</label>
              <input id={`${ids}-new`} className="wform__input" value={newName} onChange={(e) => setNewName(e.target.value)} autoFocus />
            </div>
            <button type="submit" className="ink-btn ink-btn--primary wpage__btn" disabled={busy || newName.trim() === ""}>
              {MOVE}
            </button>
          </form>
        ) : null}
      </div>

      <div className="sowner__share">
        <button
          type="button"
          role="switch"
          className="ink-switch"
          aria-checked={item.shared}
          aria-labelledby={`${ids}-share`}
          aria-describedby={`${ids}-share-hint`}
          disabled={busy}
          onClick={() => void onEdit({ shared: !item.shared })}
        >
          <span className="ink-switch__knob" />
        </button>
        <span className="sowner__share-text">
          <span id={`${ids}-share`} className="wform__label">
            {SHARE_LABEL}
          </span>
          <span id={`${ids}-share-hint`} className="wform__hint">
            {SHARE_HINT}
          </span>
        </span>
      </div>

      <div className="wpage__actions">
        {!renaming ? (
          <button type="button" className="ink-btn ink-btn--quiet wpage__btn" disabled={busy} onClick={() => setRenaming(true)}>
            <Pencil size={16} aria-hidden="true" />
            {RENAME}
          </button>
        ) : null}
        <button type="button" className="ink-btn ink-btn--quiet wpage__btn wpage__btn--danger" disabled={busy} onClick={onDelete}>
          <Trash2 size={16} aria-hidden="true" />
          {DELETE}
        </button>
      </div>
    </section>
  );
}
