// "Add something to plan": an anchor the member starts themselves, so it is active at once.

import { useId, useState, type FormEvent } from "react";
import { InkButton, InkDateRangePicker, InkField, InkFormField, InkInput, InkSegmented, InkTextArea } from "@jarida/ink/react";
import { fromCivilDate, type CivilDate } from "@jarida/ink/headless";
import { api } from "../api/PondApiClient";
import type { Anchor, NewAnchor, NewAnchorKind } from "../api/types";
import {
  ADD_FORM_HEADING,
  ADD_IT,
  CANCEL,
  CLEAR_DATES,
  KIND_LABEL,
  NEW_KINDS,
  NOTES_LABEL,
  NOTES_PLACEHOLDER,
  PLACE_LABEL,
  PLACE_PLACEHOLDER,
  TITLE_LABEL,
  TITLE_MISSING,
  TITLE_PLACEHOLDER,
  WHEN_LABEL,
  WHEN_PLACEHOLDER,
  buttonSize,
  type Variant,
} from "./myGooseWords";
import { problemWords } from "./problems";
import { ActionProblem } from "./MyGooseParts";

interface Dates {
  start: CivilDate | null;
  end: CivilDate | null;
}

const LAST_MINUTE = { hour: 23, minute: 59 };

function dayLabel(date: CivilDate): string {
  return fromCivilDate(date).toLocaleDateString(undefined, { day: "numeric", month: "short", year: "numeric" });
}

/** A day the member picked is a whole day where they are: it starts at their midnight. */
function anchorFrom(kind: NewAnchorKind, title: string, dates: Dates, place: string, notes: string): NewAnchor {
  const anchor: NewAnchor = { kind, title: title.trim() };
  if (dates.start) anchor.starts_at = fromCivilDate(dates.start).toISOString();
  if (dates.end) anchor.ends_at = fromCivilDate(dates.end, LAST_MINUTE).toISOString();
  if (place.trim() !== "") anchor.place = place.trim();
  if (notes.trim() !== "") anchor.notes = notes.trim();
  return anchor;
}

interface Props {
  variant: Variant;
  onAdded: (anchor: Anchor) => void;
  onCancel: () => void;
}

export function AddAnchorForm({ variant, onAdded, onCancel }: Props) {
  const ids = useId();
  const [kind, setKind] = useState<NewAnchorKind>("trip");
  const [title, setTitle] = useState("");
  const [dates, setDates] = useState<Dates>({ start: null, end: null });
  const [place, setPlace] = useState("");
  const [notes, setNotes] = useState("");
  const [titleMissing, setTitleMissing] = useState(false);
  const [saving, setSaving] = useState(false);
  const [problem, setProblem] = useState<string | null>(null);

  async function submit(e: FormEvent) {
    e.preventDefault();
    if (title.trim() === "") {
      setTitleMissing(true);
      return;
    }
    setSaving(true);
    setProblem(null);
    try {
      onAdded(await api.addAnchor(anchorFrom(kind, title, dates, place, notes)));
    } catch (err) {
      setProblem(problemWords(err));
      setSaving(false);
    }
  }

  return (
    <form className="mg-form" aria-label={ADD_FORM_HEADING} onSubmit={(e) => void submit(e)}>
      <h3 className="mg__h3">{ADD_FORM_HEADING}</h3>
      <InkField label={KIND_LABEL}>
        <InkSegmented label={KIND_LABEL} value={kind} options={NEW_KINDS} onChange={setKind} />
      </InkField>
      <InkFormField id={`${ids}-title`} label={TITLE_LABEL}>
        <InkInput
          label={TITLE_LABEL}
          value={title}
          placeholder={TITLE_PLACEHOLDER}
          invalid={titleMissing ? TITLE_MISSING : undefined}
          inputProps={{ id: `${ids}-title-control` }}
          onChange={(v) => {
            setTitle(v);
            setTitleMissing(false);
          }}
        />
      </InkFormField>
      <InkField label={WHEN_LABEL}>
        <div className="mg-form__dates">
          <InkDateRangePicker label={WHEN_LABEL} value={dates} placeholder={WHEN_PLACEHOLDER} format={dayLabel} onChange={setDates} />
          {dates.start ? (
            <InkButton size={buttonSize(variant)} onPress={() => setDates({ start: null, end: null })}>
              {CLEAR_DATES}
            </InkButton>
          ) : null}
        </div>
      </InkField>
      <InkFormField id={`${ids}-place`} label={PLACE_LABEL}>
        <InkInput
          label={PLACE_LABEL}
          value={place}
          placeholder={PLACE_PLACEHOLDER}
          inputProps={{ id: `${ids}-place-control` }}
          onChange={setPlace}
        />
      </InkFormField>
      <InkFormField id={`${ids}-notes`} label={NOTES_LABEL}>
        <InkTextArea
          label={NOTES_LABEL}
          value={notes}
          placeholder={NOTES_PLACEHOLDER}
          rows={2}
          inputProps={{ id: `${ids}-notes-control` }}
          onChange={setNotes}
        />
      </InkFormField>
      <div className="mg__actions">
        <InkButton type="submit" variant="primary" size={buttonSize(variant)} loading={saving}>
          {ADD_IT}
        </InkButton>
        <InkButton size={buttonSize(variant)} disabled={saving} onPress={onCancel}>
          {CANCEL}
        </InkButton>
      </div>
      <ActionProblem text={problem} />
    </form>
  );
}
