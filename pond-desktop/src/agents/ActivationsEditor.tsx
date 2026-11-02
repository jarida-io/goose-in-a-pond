import { useId, useState } from "react";
import { AlertTriangle, CalendarClock, Link2, Plus, Trash2, Zap } from "lucide-react";
import { ErrorIndex, paths } from "./agentErrors";
import {
  newEvent,
  newSchedule,
  type ActivationDraft,
  type AgentDraft,
  type EventDraft,
  type EventKind,
  type FromAgentsDraft,
  type ScheduleDraft,
} from "./agentModel";
import {
  DAY_NAMES,
  DAY_SHORT,
  HOUR_STEPS,
  REPEAT_CHOICES,
  pickForChoice,
  repeatChoice,
  toggleDay,
  type RepeatChoice,
  type SchedulePick,
} from "./agentSchedule";
import { describeActivation, type Names, type OwnerReading } from "./agentWords";
import { Field, FormSection, Messages } from "./AgentField";
import { gateOpen, type AgentEnvironment } from "./useAgentEnvironment";
import type { EditDraft } from "./useAgentBuilder";

const COOLDOWNS = [
  { value: "", label: "The usual (30 minutes)" },
  { value: "5m", label: "5 minutes" },
  { value: "10m", label: "10 minutes" },
  { value: "1h", label: "1 hour" },
  { value: "2h", label: "2 hours" },
  { value: "6h", label: "6 hours" },
  { value: "12h", label: "12 hours" },
  { value: "1d", label: "1 day" },
];

const EVENT_CHOICES: Array<{ value: EventKind; label: string }> = [
  { value: "presence.arrive", label: "Someone arrives home" },
  { value: "presence.leave", label: "Someone leaves home" },
  { value: "camera.person", label: "A camera sees a person" },
  { value: "camera.motion", label: "A camera sees movement" },
  { value: "sensor", label: "A sensor reading crosses a number" },
  { value: "device", label: "A device changes" },
  { value: "time.daily", label: "A time of day, every day" },
];

const MINUTES = [0, 5, 10, 15, 20, 25, 30, 35, 40, 45, 50, 55];

function pad2(n: number): string {
  return String(n).padStart(2, "0");
}

function minuteOptions(current: number): number[] {
  return MINUTES.includes(current) ? MINUTES : [...MINUTES, current].sort((a, b) => a - b);
}

interface Option {
  value: string;
  label: string;
}

function TimeFields({
  label,
  hour,
  minute,
  onChange,
}: {
  label: string;
  hour: number;
  minute: number;
  onChange: (hour: number, minute: number) => void;
}) {
  const groupId = useId();
  return (
    <div className="ag-field">
      <span className="ag-label" id={groupId}>
        At
      </span>
      <div className="ag-time" role="group" aria-labelledby={groupId}>
        <select
          className="ag-select"
          aria-label={`${label}: hour`}
          value={hour}
          onChange={(e) => onChange(Number(e.target.value), minute)}
        >
          {Array.from({ length: 24 }, (_, h) => (
            <option key={h} value={h}>
              {pad2(h)}
            </option>
          ))}
        </select>
        <span aria-hidden="true">:</span>
        <select
          className="ag-select"
          aria-label={`${label}: minute`}
          value={minute}
          onChange={(e) => onChange(hour, Number(e.target.value))}
        >
          {minuteOptions(minute).map((m) => (
            <option key={m} value={m}>
              {pad2(m)}
            </option>
          ))}
        </select>
      </div>
    </div>
  );
}

function SubjectSelect({
  label,
  value,
  anyLabel,
  options,
  errors,
  onChange,
}: {
  label: string;
  value: string;
  anyLabel: string;
  /** `null` while the list has not loaded; the field is then plain text. */
  options: Option[] | null;
  errors: readonly string[];
  onChange: (value: string) => void;
}) {
  const trimmed = value.trim();
  const current = trimmed === "" || trimmed.toLowerCase() === "any" ? "any" : trimmed;
  return (
    <Field label={label} errors={errors} className="ag-grow">
      {(control) =>
        options === null ? (
          <input {...control} className="ag-input" type="text" value={current} onChange={(e) => onChange(e.target.value)} />
        ) : (
          <select {...control} className="ag-select" value={current} onChange={(e) => onChange(e.target.value)}>
            <option value="any">{anyLabel}</option>
            {options.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
            {current !== "any" && !options.some((o) => o.value === current) ? (
              <option value={current}>{`As written: ${current}`}</option>
            ) : null}
          </select>
        )
      }
    </Field>
  );
}

function CooldownField({
  value,
  errors,
  onChange,
}: {
  value: string;
  errors: readonly string[];
  onChange: (value: string) => void;
}) {
  const shown = value.trim() === "30m" ? "" : value.trim();
  const known = COOLDOWNS.some((c) => c.value === shown);
  return (
    <Field
      label="Do not run again within"
      hint="Each run takes the pond's one model for a while, so it holds back after one."
      errors={errors}
    >
      {(control) => (
        <select {...control} className="ag-select" value={shown} onChange={(e) => onChange(e.target.value)}>
          {COOLDOWNS.map((c) => (
            <option key={c.value} value={c.value}>
              {c.label}
            </option>
          ))}
          {known ? null : <option value={shown}>{`As written: ${shown}`}</option>}
        </select>
      )}
    </Field>
  );
}

function BriefField({
  label,
  value,
  errors,
  onChange,
}: {
  label: string;
  value: string;
  errors: readonly string[];
  onChange: (value: string) => void;
}) {
  return (
    <Field
      label={label}
      hint="Written as a short brief to the agent. It is what the agent is told each time this starts it."
      errors={errors}
    >
      {(control) => (
        <textarea {...control} className="ag-textarea" rows={3} value={value} onChange={(e) => onChange(e.target.value)} />
      )}
    </Field>
  );
}

function RowHead({ summary, icon, onRemove }: { summary: string; icon: "schedule" | "event" | "agent"; onRemove?: () => void }) {
  const Icon = icon === "schedule" ? CalendarClock : icon === "event" ? Zap : Link2;
  return (
    <div className="ag-item__head">
      <h3 className="ag-item__title">
        <Icon size={16} aria-hidden="true" />
        <span>{summary}</span>
      </h3>
      {onRemove ? (
        <button type="button" className="ag-iconbtn" aria-label={`Remove: ${summary}`} onClick={onRemove}>
          <Trash2 size={15} aria-hidden="true" />
        </button>
      ) : null}
    </div>
  );
}

function ScheduleRow({
  index,
  item,
  errors,
  onChange,
  onRemove,
}: {
  index: number;
  item: ScheduleDraft;
  errors: ErrorIndex;
  onChange: (next: ScheduleDraft) => void;
  onRemove: () => void;
}) {
  const summary = describeActivation(item);
  const choice = repeatChoice(item.when);
  const when = item.when;
  const set = (next: SchedulePick) => onChange({ ...item, when: next });
  const cronErrors = errors.at(paths.schedule(index, "cron"));
  const fields = [paths.schedule(index, "cron"), paths.schedule(index, "brief"), paths.schedule(index, "cooldown")];
  const label = `Activation ${index + 1}`;
  return (
    <li className="ag-item">
      <div role="group" aria-label={`${label}: ${summary}`} className="ag-item__body">
        <RowHead summary={summary} icon="schedule" onRemove={onRemove} />
        <Messages messages={errors.leftovers(paths.activation(index), fields)} />
        <div className="ag-row">
          <Field label="Repeats" errors={choice === "cron" ? [] : cronErrors} className="ag-grow">
            {(control) => (
              <select
                {...control}
                className="ag-select"
                value={choice}
                onChange={(e) => set(pickForChoice(e.target.value as RepeatChoice, when))}
              >
                {REPEAT_CHOICES.map((c) => (
                  <option key={c.value} value={c.value}>
                    {c.label}
                  </option>
                ))}
              </select>
            )}
          </Field>
          {when.mode === "daily" || when.mode === "days" || when.mode === "monthly" ? (
            <TimeFields
              label={label}
              hour={when.hour}
              minute={when.minute}
              onChange={(hour, minute) => set({ ...when, hour, minute })}
            />
          ) : null}
        </div>
        {when.mode === "days" ? (
          <fieldset className="ag-days">
            <legend className="ag-label">On these days</legend>
            {DAY_NAMES.map((day) => (
              <label key={day} className="ag-day">
                <input
                  type="checkbox"
                  checked={when.days.includes(day)}
                  onChange={() => set({ ...when, days: toggleDay(when.days, day) })}
                />
                <span>{DAY_SHORT[day]}</span>
              </label>
            ))}
          </fieldset>
        ) : null}
        {when.mode === "hours" ? (
          <div className="ag-row">
            <Field label="Every">
              {(control) => (
                <select
                  {...control}
                  className="ag-select"
                  value={when.every}
                  onChange={(e) => set({ ...when, every: Number(e.target.value) })}
                >
                  {HOUR_STEPS.map((n) => (
                    <option key={n} value={n}>
                      {n === 1 ? "1 hour" : `${n} hours`}
                    </option>
                  ))}
                </select>
              )}
            </Field>
            <Field label="Minutes past the hour">
              {(control) => (
                <select
                  {...control}
                  className="ag-select"
                  value={when.minute}
                  onChange={(e) => set({ ...when, minute: Number(e.target.value) })}
                >
                  {minuteOptions(when.minute).map((m) => (
                    <option key={m} value={m}>
                      {pad2(m)}
                    </option>
                  ))}
                </select>
              )}
            </Field>
          </div>
        ) : null}
        {when.mode === "monthly" ? (
          <Field label="Day of the month">
            {(control) => (
              <select
                {...control}
                className="ag-select"
                value={when.day}
                onChange={(e) => set({ ...when, day: Number(e.target.value) })}
              >
                {Array.from({ length: 31 }, (_, d) => (
                  <option key={d + 1} value={d + 1}>
                    {d + 1}
                  </option>
                ))}
              </select>
            )}
          </Field>
        ) : null}
        {when.mode === "cron" ? (
          <Field
            label="Cron"
            hint="Six fields, seconds first: second minute hour day-of-month month day-of-week. For example 0 0 7 * * MON-FRI."
            errors={cronErrors}
          >
            {(control) => (
              <input
                {...control}
                className="ag-input ag-mono"
                type="text"
                spellCheck={false}
                value={when.cron}
                onChange={(e) => set({ mode: "cron", cron: e.target.value })}
              />
            )}
          </Field>
        ) : null}
        <BriefField
          label="What it does when it runs"
          value={item.brief}
          errors={errors.at(paths.schedule(index, "brief"))}
          onChange={(brief) => onChange({ ...item, brief })}
        />
        {item.cooldown.trim() !== "" ? (
          <CooldownField
            value={item.cooldown}
            errors={errors.at(paths.schedule(index, "cooldown"))}
            onChange={(cooldown) => onChange({ ...item, cooldown })}
          />
        ) : null}
      </div>
    </li>
  );
}

function EventRow({
  index,
  item,
  errors,
  names,
  members,
  cameras,
  devices,
  onChange,
  onRemove,
}: {
  index: number;
  item: EventDraft;
  errors: ErrorIndex;
  names: Names;
  members: Option[] | null;
  cameras: Option[] | null;
  devices: Option[] | null;
  onChange: (next: EventDraft) => void;
  onRemove: () => void;
}) {
  const summary = describeActivation(item, names);
  const label = `Activation ${index + 1}`;
  const direction: "above" | "below" = item.below.trim() !== "" && item.above.trim() === "" ? "below" : "above";
  const threshold = direction === "above" ? item.above : item.below;
  const setThreshold = (dir: "above" | "below", value: string) =>
    onChange({ ...item, above: dir === "above" ? value : "", below: dir === "below" ? value : "" });
  const fields = ["on", "member", "camera", "device", "above", "below", "at", "brief", "cooldown"].map((f) =>
    paths.event(index, f as Parameters<typeof paths.event>[1]),
  );

  function changeKind(kind: EventKind) {
    onChange({
      ...item,
      on: kind,
      sensorType: kind === "sensor" ? item.sensorType || "temperature" : item.sensorType,
      deviceKey: kind === "device" ? item.deviceKey || "power" : item.deviceKey,
      at: kind === "time.daily" ? item.at || "07:30" : item.at,
    });
  }

  const onErrors = errors.at(paths.event(index, "on"));
  const hasOther = item.on === "other";
  const [hour, minute] = item.at.split(":").map((p) => Number(p));
  const clock = Number.isInteger(hour) && Number.isInteger(minute) ? { hour, minute } : { hour: 7, minute: 30 };

  return (
    <li className="ag-item">
      <div role="group" aria-label={`${label}: ${summary}`} className="ag-item__body">
        <RowHead summary={summary} icon="event" onRemove={onRemove} />
        <Messages messages={errors.leftovers(paths.activation(index), fields)} />
        <Field label="When" errors={hasOther || item.on === "sensor" || item.on === "device" ? [] : onErrors}>
          {(control) => (
            <select {...control} className="ag-select" value={item.on} onChange={(e) => changeKind(e.target.value as EventKind)}>
              {EVENT_CHOICES.map((c) => (
                <option key={c.value} value={c.value}>
                  {c.label}
                </option>
              ))}
              {hasOther ? <option value="other">Something else, written by hand</option> : null}
            </select>
          )}
        </Field>
        {hasOther ? (
          <Field
            label="Event name as written"
            hint="One of presence.arrive, presence.leave, camera.person, camera.motion, sensor.TYPE, device.KEY or time.daily."
            errors={onErrors}
          >
            {(control) => (
              <input
                {...control}
                className="ag-input ag-mono"
                type="text"
                spellCheck={false}
                value={item.other}
                onChange={(e) => onChange({ ...item, other: e.target.value })}
              />
            )}
          </Field>
        ) : null}
        {item.on === "presence.arrive" || item.on === "presence.leave" ? (
          <SubjectSelect
            label="Who"
            value={item.member}
            anyLabel="Anyone"
            options={members}
            errors={errors.at(paths.event(index, "member"))}
            onChange={(member) => onChange({ ...item, member })}
          />
        ) : null}
        {item.on === "camera.person" || item.on === "camera.motion" ? (
          <SubjectSelect
            label="Which camera"
            value={item.camera}
            anyLabel="Any camera"
            options={cameras}
            errors={errors.at(paths.event(index, "camera"))}
            onChange={(camera) => onChange({ ...item, camera })}
          />
        ) : null}
        {item.on === "sensor" ? (
          <>
            <div className="ag-row">
              <Field label="Kind of reading" hint="For example temperature or humidity." errors={onErrors} className="ag-grow">
                {(control) => (
                  <input
                    {...control}
                    className="ag-input"
                    type="text"
                    value={item.sensorType}
                    onChange={(e) => onChange({ ...item, sensorType: e.target.value })}
                  />
                )}
              </Field>
              <SubjectSelect
                label="Which sensor"
                value={item.device}
                anyLabel="Any sensor"
                options={devices}
                errors={errors.at(paths.event(index, "device"))}
                onChange={(device) => onChange({ ...item, device })}
              />
            </div>
            <div className="ag-row">
              <Field label="Reading">
                {(control) => (
                  <select
                    {...control}
                    className="ag-select"
                    value={direction}
                    onChange={(e) => setThreshold(e.target.value as "above" | "below", threshold)}
                  >
                    <option value="above">Goes above</option>
                    <option value="below">Drops below</option>
                  </select>
                )}
              </Field>
              <Field
                label="This number"
                hint="It runs when the reading crosses the number, not on every reading past it."
                errors={[...errors.at(paths.event(index, "above")), ...errors.at(paths.event(index, "below"))]}
                className="ag-grow"
              >
                {(control) => (
                  <input
                    {...control}
                    className="ag-input"
                    type="text"
                    inputMode="decimal"
                    value={threshold}
                    onChange={(e) => setThreshold(direction, e.target.value)}
                  />
                )}
              </Field>
            </div>
          </>
        ) : null}
        {item.on === "device" ? (
          <div className="ag-row">
            <Field label="What changes" hint="The state it reports, such as power or brightness." errors={onErrors} className="ag-grow">
              {(control) => (
                <input
                  {...control}
                  className="ag-input"
                  type="text"
                  value={item.deviceKey}
                  onChange={(e) => onChange({ ...item, deviceKey: e.target.value })}
                />
              )}
            </Field>
            <SubjectSelect
              label="Which device"
              value={item.device}
              anyLabel="Any device"
              options={devices}
              errors={errors.at(paths.event(index, "device"))}
              onChange={(device) => onChange({ ...item, device })}
            />
          </div>
        ) : null}
        {item.on === "time.daily" ? (
          <>
            <TimeFields
              label={label}
              hour={clock.hour}
              minute={clock.minute}
              onChange={(h, m) => onChange({ ...item, at: `${pad2(h)}:${pad2(m)}` })}
            />
            <Messages messages={errors.at(paths.event(index, "at"))} />
          </>
        ) : null}
        <BriefField
          label="What it does when this happens"
          value={item.brief}
          errors={errors.at(paths.event(index, "brief"))}
          onChange={(brief) => onChange({ ...item, brief })}
        />
        <CooldownField
          value={item.cooldown}
          errors={errors.at(paths.event(index, "cooldown"))}
          onChange={(cooldown) => onChange({ ...item, cooldown })}
        />
      </div>
    </li>
  );
}

function HandedOn({
  draft,
  edit,
  errors,
  others,
}: {
  draft: AgentDraft;
  edit: EditDraft;
  errors: ErrorIndex;
  others: readonly string[];
}) {
  const at = draft.activations.findIndex((a) => a.kind === "agent");
  const from = at === -1 ? [] : (draft.activations[at] as FromAgentsDraft).from;
  const names = [...others, ...from.filter((n) => !others.includes(n))];

  function toggle(name: string) {
    edit(
      (d) => {
        const index = d.activations.findIndex((a) => a.kind === "agent");
        if (index === -1) return { ...d, activations: [...d.activations, { kind: "agent", from: [name] }] };
        const item = d.activations[index] as FromAgentsDraft;
        const next = item.from.includes(name) ? item.from.filter((n) => n !== name) : [...item.from, name];
        return {
          ...d,
          activations:
            next.length === 0
              ? d.activations.filter((_, i) => i !== index)
              : d.activations.map((a, i) => (i === index ? { ...item, from: next } : a)),
        };
      },
      { section: "activations", structural: true },
    );
  }

  if (names.length === 0) {
    return <p className="ag-hint">No other agents yet. When there is one, it can hand its result to this agent.</p>;
  }
  const summary = at === -1 ? "" : describeActivation(draft.activations[at]);
  return (
    <fieldset className="ag-group">
      <legend className="ag-label">Takes work handed on by these agents</legend>
      {at !== -1 ? <p className="ag-hint">{summary}</p> : null}
      {at !== -1 ? (
        <Messages messages={[...errors.at(paths.agent(at)), ...errors.leftovers(paths.activation(at), [paths.agent(at)])]} />
      ) : null}
      <ul className="ag-picks">
        {names.map((name) => {
          const j = from.indexOf(name);
          return (
            <li key={name} className="ag-pick">
              <label className="ag-option">
                <input type="checkbox" checked={j !== -1} onChange={() => toggle(name)} />
                <span className="ag-option__text">
                  <span className="ag-option__name">{name}</span>
                  {others.includes(name) ? null : <span className="ag-option__meta">No saved agent has this name</span>}
                </span>
              </label>
              {j !== -1 && at !== -1 ? <Messages messages={errors.at(paths.agentName(at, j))} /> : null}
            </li>
          );
        })}
      </ul>
    </fieldset>
  );
}

function GateNotice({ env }: { env: AgentEnvironment }) {
  const [pending, setPending] = useState(false);
  const [failed, setFailed] = useState<string | null>(null);
  const gate = env.gate;
  if (gateOpen(gate) || gate === null) return null;

  const why = !gate.delegation
    ? "Delegation is off in Settings. Schedules, events and the assistant handing an agent a job all need it."
    : !gate.activations
      ? "Starting agents by themselves is switched off on this pond."
      : "Long tasks are switched off on this pond, and every run an agent starts is a one-step long task.";

  async function turnOn() {
    setPending(true);
    setFailed(null);
    try {
      await env.enableDelegation();
    } catch (e) {
      setFailed(e instanceof Error ? e.message : String(e));
    } finally {
      setPending(false);
    }
  }

  return (
    <div className="ag-note ag-note--warn" role="status">
      <AlertTriangle size={16} aria-hidden="true" />
      <div className="ag-note__body">
        <p>Nothing here will start by itself on this pond right now. {why}</p>
        {!gate.delegation ? (
          <button type="button" className="ink-btn ink-btn--quiet ag-btn" disabled={pending} onClick={() => void turnOn()}>
            {pending ? "Turning on..." : "Turn on Delegation"}
          </button>
        ) : null}
        {failed ? <p className="ag-note__error">Could not turn it on. {failed}</p> : null}
      </div>
    </div>
  );
}

interface Props {
  draft: AgentDraft;
  edit: EditDraft;
  errors: ErrorIndex;
  env: AgentEnvironment;
  others: readonly string[];
  owner: OwnerReading;
  names: Names;
}

export function ActivationsEditor({ draft, edit, errors, env, others, owner, names }: Props) {
  const members = env.members ? env.members.map((m) => ({ value: m.id, label: m.name })) : null;
  const cameras = env.devices
    ? env.devices.filter((d) => /camera/i.test(d.device_type ?? "")).map((d) => ({ value: d.id, label: d.name }))
    : null;
  const devices = env.devices ? env.devices.map((d) => ({ value: d.id, label: d.name })) : null;

  function update(index: number, next: ActivationDraft) {
    edit((d) => ({ ...d, activations: d.activations.map((a, i) => (i === index ? next : a)) }), { section: "activations" });
  }

  function remove(index: number) {
    edit((d) => ({ ...d, activations: d.activations.filter((_, i) => i !== index) }), {
      section: "activations",
      structural: true,
    });
  }

  function setSimple(kind: "delegated" | "task_step", on: boolean) {
    edit(
      (d) => {
        const has = d.activations.some((a) => a.kind === kind);
        if (!on) return { ...d, activations: d.activations.filter((a) => a.kind !== kind) };
        if (has) return d;
        const at = kind === "delegated" ? 0 : d.activations[0]?.kind === "delegated" ? 1 : 0;
        return { ...d, activations: [...d.activations.slice(0, at), { kind }, ...d.activations.slice(at)] };
      },
      { section: "activations", structural: true },
    );
  }

  const has = (kind: string) => draft.activations.some((a) => a.kind === kind);
  const simpleMessages = draft.activations.flatMap((a, i) =>
    a.kind === "delegated" || a.kind === "task_step" ? errors.leftovers(paths.activation(i), []) : [],
  );

  return (
    <FormSection
      id="ag-when"
      title="When it runs"
      problems={errors.inSection("activations").length}
      lead="Every way this agent can start. None of them reads what anyone says: it starts when a schedule comes round, when something happens at home, or when the assistant or another agent hands it work."
    >
      <GateNotice env={env} />
      <Messages messages={errors.at(paths.activations)} />
      <fieldset className="ag-group">
        <legend className="ag-label">Handed to it</legend>
        <label className="ag-option">
          <input type="checkbox" checked={has("delegated")} onChange={(e) => setSimple("delegated", e.target.checked)} />
          <span className="ag-option__text">
            <span className="ag-option__name">When the assistant hands it a job</span>
          </span>
        </label>
        <label className="ag-option">
          <input type="checkbox" checked={has("task_step")} onChange={(e) => setSimple("task_step", e.target.checked)} />
          <span className="ag-option__text">
            <span className="ag-option__name">As a step in a long task</span>
          </span>
        </label>
        <Messages messages={simpleMessages} />
      </fieldset>

      <HandedOn draft={draft} edit={edit} errors={errors} others={others} />

      {draft.activations.some((a) => a.kind === "schedule" || a.kind === "event" || a.kind === "raw") ? (
        <ol className="ag-items">
          {draft.activations.map((a, i) => {
            if (a.kind === "schedule") {
              return (
                <ScheduleRow
                  key={i}
                  index={i}
                  item={a}
                  errors={errors}
                  onChange={(next) => update(i, next)}
                  onRemove={() => remove(i)}
                />
              );
            }
            if (a.kind === "event") {
              return (
                <EventRow
                  key={i}
                  index={i}
                  item={a}
                  errors={errors}
                  names={names}
                  members={members}
                  cameras={cameras}
                  devices={devices}
                  onChange={(next) => update(i, next)}
                  onRemove={() => remove(i)}
                />
              );
            }
            if (a.kind === "raw") {
              return (
                <li key={i} className="ag-item">
                  <div role="group" aria-label={`Activation ${i + 1}: written by hand`} className="ag-item__body">
                    <div className="ag-item__head">
                      <h3 className="ag-item__title">Written by hand</h3>
                      <button
                        type="button"
                        className="ag-iconbtn"
                        aria-label={`Remove activation ${i + 1}`}
                        onClick={() => remove(i)}
                      >
                        <Trash2 size={15} aria-hidden="true" />
                      </button>
                    </div>
                    <p className="ag-hint">The form has no controls for this one. Change it on the YAML tab.</p>
                    <pre className="ag-code">{JSON.stringify(a.value)}</pre>
                    <Messages messages={errors.leftovers(paths.activation(i), [])} />
                  </div>
                </li>
              );
            }
            return null;
          })}
        </ol>
      ) : null}

      <div className="ag-actions">
        <button
          type="button"
          className="ink-btn ink-btn--quiet ag-btn"
          onClick={() => edit((d) => ({ ...d, activations: [...d.activations, newSchedule()] }), { section: "activations" })}
        >
          <Plus size={14} aria-hidden="true" /> Run on a schedule
        </button>
        <button
          type="button"
          className="ink-btn ink-btn--quiet ag-btn"
          onClick={() => edit((d) => ({ ...d, activations: [...d.activations, newEvent()] }), { section: "activations" })}
        >
          <Plus size={14} aria-hidden="true" /> Run when something happens
        </button>
      </div>

      <p className="ag-hint">
        {env.timezone ? `Times follow the pond's clock (${env.timezone}).` : "Times follow the pond's own clock."}
      </p>
      <p className="ag-hint">{owner.line}</p>
      {owner.runs ? (
        <div className="ag-note ag-note--warn" role="status">
          <AlertTriangle size={16} aria-hidden="true" />
          <div className="ag-note__body">
            <p>{owner.runs}</p>
            <p>There is no way to set an owner from here yet.</p>
          </div>
        </div>
      ) : null}
    </FormSection>
  );
}
