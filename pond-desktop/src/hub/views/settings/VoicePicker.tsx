import { useMemo } from "react";
import { Play, Square, Loader2, AlertCircle, ArrowDownToLine } from "lucide-react";
import { VoiceOrb } from "../../../components/VoiceOrb";
import {
  describeVoice,
  gradeFor,
  noteFor,
  gradeRank,
  paceLabel,
  clampPace,
  qualityAdvice,
  describeQuality,
  VOICE_QUALITY_TIERS,
  MIN_PACE,
  MAX_PACE,
} from "../../../voice/voiceCatalogue";
import type { useVoicePreview } from "../../../voice/useVoicePreview";

type Preview = ReturnType<typeof useVoicePreview>;

interface Props {
  /** Installed voice ids. */
  voices: string[];
  selected: string;
  onSelect: (id: string) => void;
  /** Pace multiplier, 0.5–2.0. */
  pace: number;
  onPaceChange: (pace: number) => void;
  preview: Preview;
  loading: boolean;
  /** Voices on disk; any other offered voice is marked as a download on selection. */
  installed?: Set<string>;
  /** True while the engine is being fetched or reconfigured. */
  applying?: boolean;
  /** In-flight transfers from the page's shared feed, so a 326 MB tier change shows progress. */
  transfers?: { filename: string; downloaded: number; total: number | null }[];
  /** Max voices per accent, best-graded first (for setup); the selected voice is always kept. */
  maxPerAccent?: number;
  /** Quality tier, and the memory reading that says whether to move off it. */
  quality?: string;
  onQualityChange?: (value: string) => void;
  /** Free memory after the language model, in MB. Null when unknown. */
  availableMb?: number | null;
}

/** Accents, in the order the picker offers them. */
function accentsOf(voices: string[]): string[] {
  const seen: string[] = [];
  for (const v of voices) {
    const lang = describeVoice(v).language;
    if (lang && !seen.includes(lang)) seen.push(lang);
  }
  // The pond speaks English; lead with it rather than alphabetically.
  return seen.sort((a, b) => {
    const rank = (l: string) =>
      l.startsWith("American") ? 0 : l.startsWith("British") ? 1 : 2;
    return rank(a) - rank(b) || a.localeCompare(b);
  });
}

const pctOf = (pace: number) => Math.round(clampPace(pace) * 100);

/**
 * Picks the pond's voice by ear; selection is elevation, not hue. No expressivity slider: Kokoro's
 * only continuous control is `speed`. Shows each voice's grade from Kokoro's `VOICES.md` (A to F+).
 */
export function VoicePicker({
  voices,
  selected,
  onSelect,
  pace,
  onPaceChange,
  preview,
  loading,
  quality,
  onQualityChange,
  availableMb = null,
  installed,
  applying = false,
  transfers = [],
  maxPerAccent,
}: Props) {
  const accents = useMemo(() => accentsOf(voices), [voices]);
  const currentAccent = describeVoice(selected).language ?? accents[0] ?? null;

  const shown = useMemo(() => {
    const ranked = voices
      .filter((v) => describeVoice(v).language === currentAccent)
      .sort((a, b) => gradeRank(a) - gradeRank(b) || a.localeCompare(b));
    if (!maxPerAccent || ranked.length <= maxPerAccent) return ranked;
    const top = ranked.slice(0, maxPerAccent);
    // Never drop the selected voice, or the pond looks to have silently changed it.
    return top.includes(selected) || !ranked.includes(selected)
      ? top
      : [...top.slice(0, maxPerAccent - 1), selected];
  }, [voices, currentAccent, maxPerAccent, selected]);

  const info = describeVoice(selected);
  const grade = gradeFor(selected);
  const note = noteFor(selected);

  return (
    <section className="vpick" aria-label="Voice">
      {/* ── The orb, saying something ──────────────────────────────
          Driven by the real envelope of the clip playing, so it moves
          because THIS voice is speaking. A synthetic pulse would look
          identical for every voice — which is the one comparison this
          screen exists to make. */}
      {/* What is actually being fetched, while it is being fetched. A voice is
          half a megabyte and passes in a blink; a quality tier is up to 326 MB
          and a spinner for that long says nothing about whether it is working. */}
      {transfers.length > 0 && (
        <div className="vpick__xfer" role="status" aria-live="polite">
          {transfers.map((t) => {
            const pct =
              t.total && t.total > 0
                ? Math.min(100, Math.round((t.downloaded / t.total) * 100))
                : null;
            return (
              <div key={t.filename} className="vpick__xrow">
                <span className="vpick__xname">
                  <ArrowDownToLine size={12} strokeWidth={2.4} />
                  {t.filename}
                </span>
                <span className="vpick__xtrack">
                  <span
                    className="vpick__xfill"
                    data-indeterminate={pct === null}
                    style={pct === null ? undefined : { width: `${pct}%` }}
                  />
                </span>
                <span className="vpick__xpct">
                  {pct === null ? "…" : `${pct}%`}
                </span>
              </div>
            );
          })}
        </div>
      )}

      <div className="vpick__stage">
        <VoiceOrb
          state={preview.state === "playing" ? "speaking" : preview.state === "loading" ? "thinking" : "idle"}
          size="lg"
          audioLevel={preview.level}
        />
        <p className="vpick__said" aria-live="polite">
          {transfers.length > 0
            ? "Fetching what this voice needs…"
            : applying
              ? "Getting that voice ready…"
              : (preview.statement ?? "Press play to hear this voice.")}
        </p>
      </div>

      {preview.error && (
        <p className="vpick__err" role="status">
          <AlertCircle size={13} strokeWidth={2} />
          {preview.error}
        </p>
      )}

      {/* ── Accent ── */}
      {accents.length > 1 && (
        <div className="vpick__field">
          <span className="vpick__label">Accent</span>
          <div className="vpick__tabs" role="tablist" aria-label="Accent">
            {accents.map((a) => {
              const on = a === currentAccent;
              return (
                <button
                  key={a}
                  role="tab"
                  type="button"
                  aria-selected={on}
                  className="vpick__tab"
                  data-on={on}
                  disabled={loading}
                  onClick={() => {
                    // Switching accent selects its best-graded voice, not whichever id sorts first.
                    const best = voices
                      .filter((v) => describeVoice(v).language === a)
                      .sort((x, y) => gradeRank(x) - gradeRank(y))[0];
                    if (best) onSelect(best);
                  }}
                >
                  {a.replace(" English", "")}
                </button>
              );
            })}
          </div>
        </div>
      )}

      {/* ── Voices ── */}
      <div className="vpick__field">
        <span className="vpick__label">
          Voice
          {grade && (
            <span className="vpick__gradeline">
              {info.name} · grade {grade}
              {note ? ` · ${note}` : ""}
            </span>
          )}
        </span>
        <div className="vpick__voices" role="radiogroup" aria-label="Voice">
          {shown.map((id) => {
            const v = describeVoice(id);
            const g = gradeFor(id);
            const on = id === selected;
            return (
              <button
                key={id}
                type="button"
                role="radio"
                aria-checked={on}
                className="vpick__voice"
                data-on={on}
                disabled={loading || applying}
                onClick={() => onSelect(id)}
                title={
                  installed && !installed.has(id)
                    ? `${v.name}${g ? ` — grade ${g}` : ""} · downloads when selected`
                    : `${v.name}${g ? ` — grade ${g}` : ""}`
                }
              >
                <span className="vpick__vname">{v.name}</span>
                <span className="vpick__vgrade">
                  {g ?? "—"}
                  {installed && !installed.has(id) && (
                    <ArrowDownToLine
                      size={11}
                      strokeWidth={2.4}
                      className="vpick__dl"
                      aria-label="downloads when selected"
                    />
                  )}
                </span>
              </button>
            );
          })}
        </div>
      </div>

      {/* ── Pace ── */}
      <div className="vpick__field">
        <span className="vpick__label">
          Pace
          <span className="vpick__gradeline">
            {paceLabel(pace)} · {pace.toFixed(2)}×
          </span>
        </span>
        {/* Keyed so the handle remounts when the loaded value differs from
            what was drawn — the native range is uncontrolled here on purpose
            (dragging a controlled range through a debounce fights the user),
            so it seeds from `defaultValue` once. */}
        <input
          key={`pace-${loading ? "loading" : pctOf(pace)}`}
          className="vpick__slider"
          type="range"
          min={pctOf(MIN_PACE)}
          max={pctOf(MAX_PACE)}
          step={5}
          defaultValue={pctOf(pace)}
          disabled={loading}
          aria-label="Speaking pace"
          onChange={(e) => onPaceChange(Number(e.target.value) / 100)}
        />
      </div>

      {/* ── Quality ──
          Last, and quieter than the rest, because it is a different kind of
          decision: it picks the engine's precision and what gets downloaded,
          not how the pond sounds to you. */}
      {onQualityChange && (
        <div className="vpick__field">
          <span className="vpick__label">
            Quality
            <span className="vpick__gradeline">
              {describeQuality(quality).label} · {describeQuality(quality).sizeMb} MB
            </span>
          </span>
          <div className="vpick__tiers" role="radiogroup" aria-label="Voice quality">
            {VOICE_QUALITY_TIERS.map((t) => {
              const on = t.value === describeQuality(quality).value;
              return (
                <button
                  key={t.value}
                  type="button"
                  role="radio"
                  aria-checked={on}
                  className="vpick__tier"
                  data-on={on}
                  disabled={loading}
                  onClick={() => onQualityChange(t.value)}
                  title={t.detail}
                >
                  <span className="vpick__tname">{t.label}</span>
                  <span className="vpick__tsize">{t.sizeMb} MB</span>
                </button>
              );
            })}
          </div>
          {/* Says the trade, not "higher is better" — then says exactly what
              switching costs. Both halves are true now and neither was before:
              the fetch happens immediately (with progress above), and the
              engine reloads lazily, so the pause lands on the next thing said
              rather than at the moment of choosing. */}
          <p className="vpick__advice">
            {qualityAdvice(quality ?? "", availableMb)}{" "}
            Switching downloads the tier now. The next thing the pond says pauses
            while the new engine loads.
          </p>
        </div>
      )}

      {/* The one control that asks a question keeps the page's only colour. */}
      <button
        type="button"
        className="vpick__play"
        disabled={loading}
        onClick={() => (preview.busy ? preview.stop() : void preview.play())}
      >
        {preview.state === "loading" ? (
          <Loader2 size={15} strokeWidth={2.2} className="vpick__spin" />
        ) : preview.state === "playing" ? (
          <Square size={13} strokeWidth={2.2} fill="currentColor" />
        ) : (
          <Play size={15} strokeWidth={2.2} fill="currentColor" />
        )}
        {preview.state === "loading"
          ? "Preparing"
          : preview.state === "playing"
            ? "Stop"
            : "Hear this voice"}
      </button>
    </section>
  );
}
