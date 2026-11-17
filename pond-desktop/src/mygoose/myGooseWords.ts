// What the My Goose screens say, in the words a household would use. Nothing here shows an id.

import type {
  Anchor,
  AnchorKind,
  DeliverableKind,
  DeliverableSummary,
  DontRule,
  EngineJobKind,
  EngineJobState,
  EngineJobStep,
  EngineJobSummary,
  FactCategory,
  FactLine,
  Intention,
  NewAnchorKind,
  ObjectionKind,
  ReasonChip,
  Specialist,
} from "../api/types";

/** Classic is dense for a mouse; the hub takes the touch sizes. */
export type Variant = "classic" | "hub";

/** Classic buttons keep a compact face and reach 44px invisibly; the hub's are 44px to the eye. */
export function buttonSize(variant: Variant): "sm" | "md" {
  return variant === "hub" ? "md" : "sm";
}

export const MY_GOOSE_TITLE = "My Goose";
export const MY_GOOSE_INTRO = "What Goose works on for you, what it asks, and what it knows about you.";

export type MyGooseTab = "plans" | "questions" | "about" | "specialists" | "activity";

export const TABS: { value: MyGooseTab; label: string }[] = [
  { value: "plans", label: "Plans" },
  { value: "questions", label: "Questions" },
  { value: "about", label: "About you" },
  { value: "specialists", label: "Specialists" },
  { value: "activity", label: "Activity" },
];

export const TABS_LABEL = "Parts of My Goose";
export const TRY_AGAIN = "Try again";

export const PLANS_HEADING = "Plans and things you follow";
export const PLANS_INTRO =
  "Tell Goose about something real in your life, such as a trip, an event, something to buy, a project or an interest. It works ahead and makes you a plan, a dossier or a shortlist, and checks each one before you see it.";
export const LOADING_PLANS = "Loading your plans...";
export const CANDIDATES_HEADING = "Is this happening?";
export const ACTIVE_HEADING = "What Goose is working on";
export const NO_PLANS_TITLE = "Nothing planned yet";
export const NO_PLANS_BODY =
  "Add something coming up, such as a trip or an event, and Goose starts on it: what to know, what to book and what to bring.";
export const ADD_PLAN = "Add something to plan";
export const CONFIRM = "Confirm";
export const NOT_NOW = "Not now";
export const ANSWER_THEM = "Answer them";

export const ADD_FORM_HEADING = "Add something to plan";
export const KIND_LABEL = "What kind of thing";
export const TITLE_LABEL = "What is it?";
export const TITLE_PLACEHOLDER = "Diani trip with the family";
export const TITLE_MISSING = "Say what it is in a few words.";
export const WHEN_LABEL = "When";
export const WHEN_PLACEHOLDER = "Pick dates, if it has any";
export const CLEAR_DATES = "Clear dates";
export const PLACE_LABEL = "Where";
export const PLACE_PLACEHOLDER = "Diani, Kwale";
export const NOTES_LABEL = "Anything Goose should know?";
export const NOTES_PLACEHOLDER = "Four of us, a quiet place near the beach";
export const ADD_IT = "Add it";
export const CANCEL = "Cancel";

export const ALL_PLANS = "All plans";
export const LOADING_PLAN = "Loading this plan...";
export const WHY_PREFIX = "Why Goose thinks so:";
export const MADE_HEADING = "What Goose made";
export const NOTHING_MADE = "Nothing yet. Goose starts with the first piece and adds the next once it is ready.";
export const FACTS_HEADING = "What Goose knows about it";
export const WORKING_HEADING = "Working on it";
export const WAITING_TURN = "Waiting for its turn. Goose works on one thing at a time.";
export const STARTING = "Starting.";
export const STOP = "Stop";
export const SHARE = "Share";
export const NEW_BADGE = "New";

export const STOP_DESCRIPTION = "Goose stops working on it and makes nothing more for it. Say why, so it does better next time.";
export const WHAT_WAS_WRONG = "What was wrong?";
export const IN_YOUR_WORDS = "In your own words, if you like";
export const NEVER_AGAIN = "Never suggest this again";
export const STOP_IT = "Stop it";
export const REASONS_LABEL = "Why";

export const SHARE_DESCRIPTION = "They can read it and answer its questions. Only you can change it.";
export const SHARE_NOBODY = "Nobody else in your household has My Goose yet.";
export const SAVE_SHARING = "Save sharing";
export const SHARED_SAVED = "Sharing saved.";

export const BACK_TO = (anchorTitle: string): string => `Back to ${anchorTitle}`;
export const LOADING_DELIVERABLE = "Loading...";
export const UNSURE_NOTICE = "Goose isn't sure about this one. Its checker still had objections when time ran out:";
export const TO_PASS = "What would fix it:";
export const CHANGED_HEADING = "What changed";
export const GLANCE_HEADING = "At a glance";
export const STILL_MAKING = "Goose is still making this. It shows here once it has been checked.";
export const REFRESHING_NOW = "Goose is making a fresh version now.";
export const REFRESH = "Refresh";
export const DOWNLOAD = "Download";
export const NOT_THIS_ONE = "Not this one";
export const NEVER_THIS = "Never this";
export const NOT_THIS_DESCRIPTION = "Goose drops this one and keeps your reasons in mind.";
export const NEVER_THIS_DESCRIPTION =
  "Goose won't make anything like this again. Say what to avoid in your own words and it becomes a rule.";

export const QUESTIONS_HEADING = "Questions from Goose";
export const QUESTIONS_INTRO = "Goose asks when a plan is missing a detail. Your answers go into the work at once and are kept for next time.";
export const LOADING_QUESTIONS = "Loading questions...";
export const NO_QUESTIONS_TITLE = "Nothing to answer";
export const NO_QUESTIONS_BODY = "When Goose needs a detail for one of your plans, it asks here.";
export const SEE_PLANS = "See your plans";
export const OTHER = "Other";
export const OTHER_ANSWER = "Your answer";
export const SEND_ANSWER = "Send answer";
export const ANSWER_SENT = "Answer sent.";
export const SKIP = "Skip";
export const SKIPPED = "Skipped.";
export const BATCH_DONE = "That's all. Goose uses your answers from now on.";
export const RECOMMENDED = "Recommended";
export const DONE_ASKING = "Done";
export const WENT_AHEAD = "Goose goes ahead with what it has.";
export const QUESTIONS_PAGER = "Questions in this set";
export const WAITING_FOR_YOU = "Waiting for your answers";
export const GETTING_READY = "Getting the questions ready...";
export const YOUR_ANSWERS = "Your answers";
export const PREVIEW_HEADING = "What this leads to";
export const OPEN_PAGE = "Open the page";

export const ABOUT_HEADING = "What Goose knows about you";
export const ABOUT_INTRO =
  "Goose keeps these lines and reads them before it makes anything for you. Pin the ones that matter, fix the ones that are wrong and remove what it should forget.";
export const LOADING_FACTS = "Loading what Goose knows...";
export const NO_FACTS_TITLE = "Nothing yet";
export const NO_FACTS_BODY = "Goose writes these from your conversations and answers. Add a line yourself, or ask it to go over what it has.";
export const REBUILD = "Rebuild";
export const REBUILD_ASKED = "Rebuild asked for. Goose goes over your memories again and updates this list when it's done.";
export const PIN = "Pin";
export const UNPIN = "Unpin";
export const EDIT = "Edit";
export const SAVE = "Save";
export const SAVED = "Saved.";
export const REMOVE = "Remove";
export const REMOVE_TITLE = "Remove this line";
export const REMOVE_QUESTION = (fact: string): string =>
  `Remove "${fact}"? Goose won't learn it again and won't build anything on it.`;
export const REMOVED = "Removed.";
export const ADD_LINE_HEADING = "Add a line";
export const CATEGORY_LABEL = "Kind of line";
export const LINE_LABEL = "What should Goose know?";
export const LINE_PLACEHOLDER = "I'm vegetarian";
export const ADD_LINE = "Add line";
export const LINE_ADDED = "Line added.";
export const LINE_MISSING = "Write the line first.";
export const EDIT_LINE_LABEL = "The line";

export const DONT_HEADING = "Things Goose won't offer";
export const DONT_INTRO = "Built from what you turned down. A never rule always applies; an avoid rule applies where it is relevant.";
export const NO_RULES = "No rules yet. Turning something down with a reason adds one.";
export const RULE_LABEL = "A new rule";
export const RULE_PLACEHOLDER = "Recipes built on a colour I like";
export const STRENGTH_LABEL = "How strict";
export const ADD_RULE = "Add rule";
export const RULE_ADDED = "Rule added.";
export const RULE_MISSING = "Write the rule first.";
export const DELETE = "Delete";
export const RULE_DELETED = "Rule deleted.";

export const SPECIALISTS_HEADING = "Specialists";
export const SPECIALISTS_INTRO = "Each one knows how to do one kind of work well. Goose hands them the plans they are good at.";
export const LOADING_SPECIALISTS = "Loading specialists...";
export const NO_SPECIALISTS = "This pond has no specialists yet.";
export const OFFERED_PREFIX = "Goose suggests turning this on.";

export const ACTIVITY_HEADING = "Activity";
export const WAITING_HEADING = "Waiting to happen";
export const WAITING_INTRO = "Things Goose means to do once something holds, until a deadline.";
export const NOTHING_WAITING = "Nothing waiting.";
export const WITHDRAW = "Withdraw";
export const WITHDRAWN = "Withdrawn.";
export const LOG_HEADING = "What Goose did lately";
export const LOADING_ACTIVITY = "Loading activity...";
export const NOTHING_DONE = "Nothing yet. Work Goose does for you shows here as it happens.";

export const PERSONAL_TITLE = "Open My Goose on your phone";
export const PERSONAL_BODY =
  "My Goose is personal: your plans, your questions and what Goose knows about you. This screen isn't linked to anyone in your household, so it can't tell whose to show.";
export const NOT_SERVED_TITLE = "This pond has no My Goose yet";
export const NOT_SERVED_BODY = "Update the pond to the newest version, then come back here.";

const KIND_WORDS: Record<AnchorKind, string> = {
  event: "Event",
  trip: "Trip",
  purchase: "Something to buy",
  project: "Project",
  interest: "Interest",
  standing: "Everyday",
};

export const NEW_KINDS: { value: NewAnchorKind; label: string }[] = (
  ["event", "trip", "purchase", "project", "interest"] as const
).map((value) => ({ value, label: KIND_WORDS[value] }));

export function kindWords(kind: AnchorKind): string {
  return KIND_WORDS[kind] ?? "Plan";
}

const DELIVERABLE_KIND_WORDS: Record<DeliverableKind, string> = {
  dossier: "Dossier",
  plan: "Plan",
  shortlist: "Shortlist",
  digest: "Digest",
  checklist: "Checklist",
};

export function deliverableKindWords(kind: DeliverableKind): string {
  return DELIVERABLE_KIND_WORDS[kind] ?? "Made for you";
}

/** A refresh of something already made reads differently from its first making. */
export function deliverableStateWords(d: Pick<DeliverableSummary, "state" | "version">): string {
  switch (d.state) {
    case "queued":
      return d.version > 0 ? "Ready, a fresh version is next in line" : "Next in line";
    case "making":
      return d.version > 0 ? "Ready, a fresh version is being made" : "Being made";
    case "ready":
      return "Ready";
    case "unsure":
      return "Ready, but Goose isn't sure";
    case "rejected":
      return "Turned down";
    case "archived":
      return "Put away";
  }
}

/** A first version still being made has nothing to read yet. */
export function canOpen(d: Pick<DeliverableSummary, "state" | "version">): boolean {
  if (d.state === "ready" || d.state === "unsure") return true;
  return (d.state === "queued" || d.state === "making") && d.version > 0;
}

const DATE: Intl.DateTimeFormatOptions = { day: "numeric", month: "short", year: "numeric" };

function dateOf(iso: string | null): Date | null {
  if (!iso) return null;
  const d = new Date(iso);
  return Number.isNaN(d.getTime()) ? null : d;
}

export function dayWords(iso: string | null): string | null {
  const d = dateOf(iso);
  return d ? d.toLocaleDateString(undefined, DATE) : null;
}

export function whenWords(startsAt: string | null, endsAt: string | null): string | null {
  const start = dayWords(startsAt);
  const end = dayWords(endsAt);
  if (start && end) return start === end ? start : `${start} to ${end}`;
  if (start) return start;
  if (end) return `Until ${end}`;
  return null;
}

export function anchorMeta(anchor: Pick<Anchor, "kind" | "starts_at" | "ends_at" | "place">): string {
  return [kindWords(anchor.kind), whenWords(anchor.starts_at, anchor.ends_at), anchor.place]
    .filter((part): part is string => !!part && part.trim() !== "")
    .join(" · ");
}

export function sharedByWords(name: string | null): string {
  return name ? `Shared with you by ${name}. You can read it and answer its questions.` : "Shared with you. You can read it and answer its questions.";
}

export function roundsWords(rounds: number): string | null {
  if (rounds <= 0) return null;
  if (rounds === 1) return "Checked once";
  if (rounds === 2) return "Checked twice";
  return `Checked ${rounds} times`;
}

export function jobLine(job: EngineJobSummary): string {
  return job.state === "running" ? `Working on it now: ${job.subject}` : `Next in line: ${job.subject}`;
}

const STEP_WORDS: Record<EngineJobStep["state"], string> = {
  pending: "Waiting",
  running: "Working on it",
  completed: "Done",
  failed: "Did not finish",
  skipped: "Skipped",
  awaiting_approval: "Waiting for your yes",
};

export function stepWords(state: EngineJobStep["state"]): string {
  return STEP_WORDS[state] ?? "Waiting";
}

export function stepMarker(state: EngineJobStep["state"]): "done" | "current" | "todo" {
  if (state === "running") return "current";
  if (state === "pending" || state === "awaiting_approval") return "todo";
  return "done";
}

export const REASONS: { value: ReasonChip; label: string }[] = [
  { value: "too_generic", label: "Too generic" },
  { value: "not_relevant", label: "Not relevant to me" },
  { value: "wrong_facts", label: "Wrong facts" },
  { value: "already_know", label: "I already know this" },
  { value: "bad_timing", label: "Bad timing" },
];

export function reasonWords(reason: ReasonChip): string {
  return REASONS.find((r) => r.value === reason)?.label ?? "";
}

const OBJECTION_WORDS: Record<ObjectionKind, string> = {
  grounding: "Not backed by a source",
  relevance: "May not matter to you",
  duplicate: "Repeats something you already have",
  dont_list: "Goes against something you turned down",
  vague: "Too vague",
  stale: "May be out of date",
};

export function objectionWords(kind: ObjectionKind): string {
  return OBJECTION_WORDS[kind] ?? "An objection";
}

export const FACT_CATEGORIES: { value: FactCategory; label: string }[] = [
  { value: "identity", label: "Who you are" },
  { value: "home", label: "Home" },
  { value: "work", label: "Work" },
  { value: "people", label: "People" },
  { value: "plan", label: "Plans" },
  { value: "hunt", label: "Looking for" },
  { value: "interest", label: "Interests" },
  { value: "food", label: "Food" },
  { value: "health", label: "Health" },
  { value: "style", label: "Style" },
  { value: "routine", label: "Routines" },
  { value: "avoid", label: "Things to avoid" },
  { value: "other", label: "Other" },
];

export function factGroups(facts: FactLine[]): { category: FactCategory; label: string; lines: FactLine[] }[] {
  return FACT_CATEGORIES.map(({ value, label }) => ({
    category: value,
    label,
    lines: facts
      .filter((f) => (FACT_CATEGORIES.some((c) => c.value === f.category) ? f.category === value : value === "other"))
      .sort((a, b) => Number(b.pinned) - Number(a.pinned)),
  })).filter((g) => g.lines.length > 0);
}

const CONFIDENCE_WORDS: Record<FactLine["confidence"], string> = {
  sure: "Sure",
  likely: "Likely",
  said: "Said once",
};

const SOURCE_WORDS: Record<FactLine["source"]["kind"], string> = {
  memory: "from a memory",
  summary: "from a conversation",
  interest: "from an interest",
  anchor: "from a plan",
  answer: "from your answer",
  interview: "from your interview",
  you: "you wrote it",
};

/** "giap-weather__get_forecast" reads as "get forecast". */
export function toolWords(tool: string): string {
  const name = tool.split("__").pop() ?? tool;
  return name.replace(/^giap-/, "").replace(/[_-]+/g, " ").trim();
}

export function factNote(line: Pick<FactLine, "confidence" | "source" | "tools" | "since">): string {
  const parts = [`${CONFIDENCE_WORDS[line.confidence] ?? "Likely"}, ${SOURCE_WORDS[line.source.kind] ?? "from Goose"}`];
  if (line.since) parts.push(`since ${line.since}`);
  const tools = line.tools.map(toolWords).filter(Boolean);
  if (tools.length > 0) parts.push(`Helps with: ${tools.join(", ")}`);
  return parts.join(". ");
}

export function countWords(count: number, limit: number): string {
  return `${count} of ${limit} lines`;
}

export const STRENGTHS: { value: DontRule["strength"]; label: string }[] = [
  { value: "never", label: "Never" },
  { value: "avoid", label: "Avoid" },
];

export function strengthWords(strength: DontRule["strength"]): string {
  return strength === "never" ? "Never" : "Avoid";
}

const ORIGIN_WORDS: Record<DontRule["origin"], string> = {
  reject: "You turned something down",
  stop: "You stopped a plan",
  fact: "You removed a line about you",
  you: "You wrote it",
};

export function ruleNote(rule: Pick<DontRule, "origin" | "reasons" | "said">): string {
  const parts = [ORIGIN_WORDS[rule.origin] ?? "You wrote it"];
  const reasons = rule.reasons.map(reasonWords).filter(Boolean);
  if (reasons.length > 0) parts.push(reasons.join(", "));
  if (rule.said) parts.push(`"${rule.said}"`);
  return parts.join(". ");
}

const SPECIALIST_STATE_WORDS: Record<Specialist["state"], string> = {
  on: "On",
  off: "Off",
  offered: "Off, suggested by Goose",
  declined: "Off, you said no",
};

export function specialistStateWords(state: Specialist["state"]): string {
  return SPECIALIST_STATE_WORDS[state] ?? "Off";
}

const JOB_KIND_WORDS: Record<EngineJobKind, string> = {
  deliverable: "Something made for you",
  fact_sheet: "What Goose knows about you",
  memory: "Memories",
  consolidation: "A tidy-up of memories",
  heartbeat: "A check-in",
  capture: "A picture",
  ledger: "A payment record",
  rule: "A rule",
  questions: "Questions for you",
};

export function jobKindWords(kind: EngineJobKind): string {
  return JOB_KIND_WORDS[kind] ?? "Work";
}

const JOB_STATE_WORDS: Record<EngineJobState, string> = {
  queued: "Waiting its turn",
  running: "Working on it",
  passed: "Done, and it passed the check",
  unsure: "Done, but not sure",
  dropped: "Dropped: it did not pass the check",
  failed: "Did not finish",
  cancelled: "Called off",
};

export function jobStateWords(state: EngineJobState): string {
  return JOB_STATE_WORDS[state] ?? "Waiting its turn";
}

const ACT_WORDS: Record<Intention["act"], string> = {
  push: "Tell you",
  refresh: "Refresh a plan",
  look_into: "Look into it",
  ask: "Ask you",
  propose_todo: "Suggest a to-do",
};

export function actWords(act: Intention["act"]): string {
  return ACT_WORDS[act] ?? "Do it";
}

export function intentionNote(intention: Pick<Intention, "when" | "until">): string {
  const until = dayWords(intention.until);
  return until ? `When: ${intention.when}. Until ${until}.` : `When: ${intention.when}.`;
}

export function moreWords(count: number, one: string, many: string): string {
  return count === 1 ? `1 more ${one}` : `${count} more ${many}`;
}

export function questionCountWords(count: number): string {
  return count === 1 ? "Goose has a question about your plans." : `Goose has ${count} questions about your plans.`;
}

export function badgeWords(anchor: Pick<Anchor, "deliverables" | "open_questions">): string[] {
  const unseen = anchor.deliverables.filter((d) => d.unseen).length;
  const out: string[] = [];
  if (unseen > 0) out.push(unseen === 1 ? "1 new" : `${unseen} new`);
  if (anchor.open_questions > 0) out.push(anchor.open_questions === 1 ? "1 question" : `${anchor.open_questions} questions`);
  return out;
}

export function confirmedWords(title: string): string {
  return `Confirmed: ${title}. Goose is starting on it.`;
}

export function notNowWords(title: string): string {
  return `Not now: ${title} is set aside.`;
}

export function addedWords(title: string): string {
  return `Added: ${title}. Goose is starting on it.`;
}

export function stoppedWords(title: string): string {
  return `Stopped: Goose makes nothing more for ${title}.`;
}

export function refreshWords(state: "queued" | "running"): string {
  return state === "running" ? "Refresh started." : "Refresh asked for. It's next in line.";
}

export function turnedDownWords(verb: "not_this" | "never", title: string): string {
  return verb === "never" ? `Never this: Goose won't make anything like ${title} again.` : `Not this one: ${title} is turned down.`;
}

export function questionStep(at: number, of: number): string {
  return `Question ${at} of ${of}`;
}
