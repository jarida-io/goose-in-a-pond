// What the report's reading view and its save menu say, in the words a household would use.

export const SAVE_OR_PRINT = "Save or print";

export const SAVE_MARKDOWN = "Save as Markdown";
export const SAVE_MARKDOWN_HINT = "Plain text you can open in any editor.";

export const SAVE_WEB_PAGE = "Save as a web page";
export const SAVE_WEB_PAGE_HINT = "One page you can open in a browser.";

export const PRINT_REPORT = "Print or save as PDF";
export const PRINT_REPORT_HINT = "Opens the print window.";

export const ADD_TO_CALENDAR = "Add to calendar";
export const ADD_TO_CALENDAR_HINT = "Saves the dated events as a calendar file.";

export const CHECKING_SAVE = "Checking what can be saved...";
export const TRY_AGAIN = "Try again";
export const CANT_CHECK_SAVE = "Couldn't check what can be saved.";

/** The line under the one item an older pond can offer, which cannot make the other kinds. */
export const ONLY_MARKDOWN = "This pond can only save the result as Markdown for now.";

export const GETTING_FILE = "Getting the file...";
export const NO_DATED_EVENTS = "This report has no dated events to add.";

export function fileReady(name: string): string {
  return `${name} is ready.`;
}

export function couldntGetFile(reason: string): string {
  return `Couldn't get the file. ${reason}`;
}

export function couldntPrint(reason: string): string {
  return `Couldn't open the print window. ${reason}`;
}

export const REPORT_SECTIONS_LABEL = "Sections";
export const REPORT_TEXT_LABEL = "The report";
export const CLOSE_REPORT = "Close the report";

/** "12 sections, 8 sources", leaving out whichever is none. */
export function reportCounts(sections: number, sources: number): string {
  const parts: string[] = [];
  if (sections > 0) parts.push(`${sections} ${sections === 1 ? "section" : "sections"}`);
  if (sources > 0) parts.push(`${sources} ${sources === 1 ? "source" : "sources"}`);
  return parts.join(", ");
}
