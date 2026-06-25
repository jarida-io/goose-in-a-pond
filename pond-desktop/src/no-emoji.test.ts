import { describe, expect, it } from "vitest";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, extname, dirname } from "node:path";
import { fileURLToPath } from "node:url";

const SRC_DIR = dirname(fileURLToPath(import.meta.url));

function collectSourceFiles(dir: string): string[] {
  const result: string[] = [];
  for (const entry of readdirSync(dir)) {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) {
      result.push(...collectSourceFiles(full));
    } else if ([".ts", ".tsx"].includes(extname(entry))) {
      result.push(full);
    }
  }
  return result;
}

// Matches any Unicode extended pictographic (covers all emoji and pictographic symbols)
const EMOJI_RE = /\p{Extended_Pictographic}/u;

describe("no-emoji lint", () => {
  it("pond-desktop/src contains no emoji characters", () => {
    const violations: string[] = [];

    for (const file of collectSourceFiles(SRC_DIR)) {
      const lines = readFileSync(file, "utf8").split("\n");
      lines.forEach((line, i) => {
        if (EMOJI_RE.test(line)) {
          violations.push(`${file.replace(SRC_DIR + "/", "")}:${i + 1}: ${line.trim()}`);
        }
      });
    }

    expect(
      violations,
      `Emoji found in source — replace with lucide-react icons:\n${violations.join("\n")}`,
    ).toEqual([]);
  });
});
