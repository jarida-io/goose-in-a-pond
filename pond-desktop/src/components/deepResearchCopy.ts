// What the composers say about deep research, shared so the classic and hub shells agree.

export const DEEP_RESEARCH = "Deep research";

export const ADD_MENU_LABEL = "Add to message";

export const ADD_PICTURES = "Add pictures";

/** The item's line while it is free to use. */
export const DEEP_RESEARCH_HINT = "The pond looks into your question and reports back here.";

/** The item's line once it is on. */
export const DEEP_RESEARCH_ON_HINT = "On for your next message.";

/** The item's reason for a guest or an unattributed caller; the pond cannot run it for them. */
export const DEEP_RESEARCH_UNIDENTIFIED = "The pond needs to know who is asking before it can do this.";

/** The item's reason while pictures wait in the tray. */
export const DEEP_RESEARCH_NEEDS_NO_PICTURES = "Remove the pictures first. Deep research works from words alone.";

/** Add pictures' reason while the mode is on, and the line a pasted picture gets. */
export const PICTURES_NOT_IN_RESEARCH = "Deep research works from words alone. Leave it to add pictures.";

export const RESEARCH_PLACEHOLDER = "What should the pond research?";

export const RESEARCH_SEND = "Start deep research";

/** Under the chip while a reply is still streaming: Send is held, and this says why. */
export const RESEARCH_WAITS = "Starts once this answer ends.";

export const LEAVE_RESEARCH = "Leave deep research";

/** The choice beside the chip: how much the pond writes. Its default leaves it to the question. */
export const RESEARCH_ANSWER_LABEL = "How much to write";

export const RESEARCH_ANSWERS = [
  { value: "auto", label: "Pond decides", hint: "Short for a quick question, a report for a big one." },
  { value: "short", label: "Short answer", hint: "A few sentences, here in the chat." },
  { value: "report", label: "Full report", hint: "Sections with sources, to read and export." },
] as const;

/** Clause for a refused research turn's message; the question is already back in the box. */
export const RESEARCH_REFUSED_CLAUSE = " Your question is back in the box. You can still send it as an ordinary message.";
