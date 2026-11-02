import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

// A cheap tripwire, not coverage (the component tests are that): what a report is drawn from is
// text the pond's model wrote, so a path that could turn it into HTML is caught by its mere presence.
const SRC_DIR = dirname(dirname(fileURLToPath(import.meta.url)));
const read = (rel: string) => readFileSync(join(SRC_DIR, rel), "utf8");

const DRAWS_REPORT_TEXT = [
  "components/MarkdownText.tsx",
  "lib/markdownBlocks.ts",
  "longtasks/ReportSheet.tsx",
  "longtasks/ReportExport.tsx",
  "longtasks/LongTaskCard.tsx",
  "lib/saveFile.ts",
  "lib/printHtml.ts",
];

describe("a report is drawn as elements, never as html", () => {
  for (const file of DRAWS_REPORT_TEXT) {
    it(`${file} has no way to put a string into the page as markup`, () => {
      const src = read(file);
      for (const sink of ["dangerouslySetInnerHTML", "innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "createContextualFragment"]) {
        expect(src.includes(sink), `${file} uses ${sink}, which would draw the pond's text as markup`).toBe(false);
      }
    });
  }

  it("the frame that prints the pond's page is sandboxed without scripts", () => {
    const src = read("lib/printHtml.ts");
    const sandbox = /setAttribute\("sandbox", "([^"]*)"\)/.exec(src)?.[1] ?? "";
    expect(sandbox, "the print frame has no sandbox").not.toBe("");
    expect(sandbox.includes("allow-scripts"), "the print frame may run the page's scripts").toBe(false);
  });
});

describe("the card and the sheet keep the report's controls", () => {
  it("the card opens the sheet and offers saving and printing", () => {
    const src = read("longtasks/LongTaskCard.tsx");
    expect(src.includes("<ReportSheet"), "the card no longer opens the whole report").toBe(true);
    expect(src.includes("<ReportExport"), "the card lost its save and print control").toBe(true);
  });

  it("the sheet offers saving and printing too", () => {
    expect(read("longtasks/ReportSheet.tsx").includes("<ReportExport"), "the sheet lost its save and print control").toBe(true);
  });

  it("saving goes through the api client, which sends the bearer token a plain link cannot", () => {
    const src = read("longtasks/ReportExport.tsx");
    expect(src.includes("api.getLongTaskExport"), "the export is not fetched through the client").toBe(true);
    expect(/href=\{[^}]*export/i.test(src), "the export is linked to directly").toBe(false);
  });
});
