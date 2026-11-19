// What the idea cards say. Nothing here shows an id.

import type { IdeaCard, IdeaCardActionKind, IdeaCardKind, IdeaCardSetState, IdeaCardSourceKind } from "../api/types";

export const DECK_TITLE = "Ideas for you";
export const LOADING_CARDS = "Loading your cards...";
export const NO_CARDS = "Nothing yet. Refresh, and Goose makes a few ideas from your plans, to-dos and interests.";
export const MAKING_CARDS = "Goose is making your cards. They show here when they are ready.";
export const CARDS_LABEL = "Your cards";
export const DO_IT = "Do it";
export const SAVE_CARD = "Save";
export const UNSAVE_CARD = "Unsave";
export const NOT_THIS = "Not this";
export const GO_DEEPER = "Go deeper";
export const DETAILS = "Details";
export const FOUND_HEADING = "What Goose found";
export const NEXT_HEADING = "What it would do next";
export const CAVEATS_HEADING = "What it isn't sure of";
export const SOURCES_HEADING = "Sources";
export const PREVIOUS = "Previous";
export const NEXT = "Next";
export const MADE_FRESH = "Goose is making fresh cards.";
export const UNSAVED = "Unsaved. It fades with the rest of its set.";

const SET_WORDS: Record<IdeaCardSetState, string> = {
  none: "Nothing yet",
  queued: "Making your cards",
  making: "Making your cards",
  ready: "Ready",
};

/**
 * The set's state in words. A queued set says what it waits behind: the engine runs one job at a
 * time for the household, and a refresh behind three plans being researched read as stuck.
 */
export function setStateWords(state: IdeaCardSetState, ahead?: number): string {
  if (state === "queued" && ahead !== undefined && ahead > 0) {
    return ahead === 1 ? "Goose is finishing 1 thing first" : `Goose is finishing ${ahead} things first`;
  }
  return SET_WORDS[state] ?? "Nothing yet";
}

export function isMaking(state: IdeaCardSetState): boolean {
  return state === "queued" || state === "making";
}

const KIND_WORDS: Record<IdeaCardKind, string> = {
  do: "Something to do",
  next_step: "Next step",
  reminder: "Reminder",
  ask: "Ask Goose",
};

export function cardKindWords(kind: IdeaCardKind): string {
  return KIND_WORDS[kind] ?? "Idea";
}

const SOURCE_WORDS: Record<IdeaCardSourceKind, string> = {
  memory: "From a memory",
  fact: "From what Goose knows about you",
  plan: "From your plan",
  interest: "From your interest",
  todo: "From your to-dos",
  web: "From the web",
};

export function sourceWords(source: IdeaCard["why"]["source"]): string {
  const lead = SOURCE_WORDS[source.kind] ?? "From Goose";
  return source.label.trim() === "" ? lead : `${lead}: ${source.label}`;
}

/** The structured fields a card carries, in reading order; a card shows only the ones it has. */
export function cardFields(card: IdeaCard): { label: string; value: string }[] {
  const fields: { label: string; value: string | null | undefined }[] = [
    { label: "When", value: card.when },
    { label: "Where", value: card.where },
    { label: "Cost", value: card.cost },
    { label: "Effort", value: card.effort },
  ];
  return fields.filter((f): f is { label: string; value: string } => !!f.value && f.value.trim() !== "");
}

export function positionWords(at: number, of: number): string {
  return `Card ${at} of ${of}`;
}

export function countWords(count: number): string {
  return count === 1 ? "1 card" : `${count} cards`;
}

export function savedWords(title: string): string {
  return `Saved: ${title} stays until you are done with it.`;
}

export function dismissedWords(title: string): string {
  return `Not this: ${title} is gone, and Goose keeps your reasons in mind.`;
}

const DONE_WORDS: Record<IdeaCardActionKind, (title: string) => string> = {
  todo: (title) => `Added to your to-dos: ${title}.`,
  reminder: (title) => `Reminder set: ${title}.`,
  chat: (title) => `Asked Goose: ${title}.`,
  watch: (title) => `Goose is watching it: ${title}.`,
  go_deeper: (title) => `Going deeper on ${title}. Goose makes a plan for it under Plans.`,
};

export function doneWords(kind: IdeaCardActionKind, title: string): string {
  return (DONE_WORDS[kind] ?? ((t: string) => `Done: ${t}.`))(title);
}
