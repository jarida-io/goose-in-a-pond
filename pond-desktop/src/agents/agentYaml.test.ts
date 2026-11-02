import { describe, expect, it } from "vitest";
import { YamlError, emitYaml, parseYaml, type YamlValue } from "./agentYaml";

const SPEC_BLOCK = `giap_role:
  tool_groups: [giap-knowledge, giap-weather, giap-task-work]
  personal_data: inherit            # or deny: the agent sees no personal data
  max_turns: 8
  instructions: |
    What the agent is for, in plain words.
  context:
    examples:
      - brief: "Brief me"
        answer: "It is sunny."
  activations:
    - delegated
    - task_step
    - schedule: { cron: "0 0 7 * * MON-FRI", brief: "Brief the house." }
    - event: { on: presence.arrive, member: any, brief: "Greet them.", cooldown: 30m }
    - agent: [trip-planner]
  hooks:
    before: [today, weather]
    after: [notify_owner]
  a2a:
    expose: false
`;

function thrown(text: string): YamlError {
  try {
    parseYaml(text);
  } catch (e) {
    if (e instanceof YamlError) return e;
    throw e;
  }
  throw new Error("expected the text to be refused");
}

describe("parseYaml", () => {
  it("reads the block the spec shows, flow lists and maps included", () => {
    expect(parseYaml(SPEC_BLOCK)).toEqual({
      giap_role: {
        tool_groups: ["giap-knowledge", "giap-weather", "giap-task-work"],
        personal_data: "inherit",
        max_turns: 8,
        instructions: "What the agent is for, in plain words.\n",
        context: { examples: [{ brief: "Brief me", answer: "It is sunny." }] },
        activations: [
          "delegated",
          "task_step",
          { schedule: { cron: "0 0 7 * * MON-FRI", brief: "Brief the house." } },
          { event: { on: "presence.arrive", member: "any", brief: "Greet them.", cooldown: "30m" } },
          { agent: ["trip-planner"] },
        ],
        hooks: { before: ["today", "weather"], after: ["notify_owner"] },
        a2a: { expose: false },
      },
    });
  });

  it("reads a list written at the same indent as its key", () => {
    expect(parseYaml("tool_groups:\n- a\n- b\nother: 1\n")).toEqual({ tool_groups: ["a", "b"], other: 1 });
  });

  it("reads a map as the first thing in a list item, with its keys lined up beneath", () => {
    expect(parseYaml("items:\n  - a: 1\n    b: two\n  - a: 3\n")).toEqual({
      items: [{ a: 1, b: "two" }, { a: 3 }],
    });
  });

  it("reads quoted text with its escapes, and a plain apostrophe as part of the word", () => {
    expect(parseYaml('a: "tab\\there \\"quoted\\" \\u00e9"\nb: \'it\'\'s\'\nc: don\'t\n')).toEqual({
      a: 'tab\there "quoted" \u00e9',
      b: "it's",
      c: "don't",
    });
  });

  it("keeps a hash inside quotes and drops one that starts a comment", () => {
    expect(parseYaml('a: "x # y" # a comment\nb: plain # also a comment\nc: a#b\n')).toEqual({
      a: "x # y",
      b: "plain",
      c: "a#b",
    });
  });

  it("reads null, booleans and numbers, and leaves other words alone", () => {
    expect(parseYaml("a: ~\nb: true\nc: false\nd: -3.5\ne: 30m\nf: 007\ng:\n")).toEqual({
      a: null,
      b: true,
      c: false,
      d: -3.5,
      e: "30m",
      f: "007",
      g: null,
    });
  });

  it("reads block scalars with clip, strip and keep", () => {
    expect(parseYaml("a: |\n  one\n  two\nb: |-\n  one\nc: |+\n  one\n\nd: x\n")).toEqual({
      a: "one\ntwo\n",
      b: "one",
      c: "one\n\n",
      d: "x",
    });
  });

  it("folds a > block into one line", () => {
    expect(parseYaml("a: >\n  one\n  two\n")).toEqual({ a: "one two\n" });
  });

  it("joins a flow list that runs over several lines", () => {
    expect(parseYaml("a: [\n  x,\n  y\n]\nb: 1\n")).toEqual({ a: ["x", "y"], b: 1 });
  });

  it("reads an empty text as nothing", () => {
    expect(parseYaml("")).toBeNull();
    expect(parseYaml("# only a comment\n")).toBeNull();
  });

  it("refuses a repeated key, naming the line", () => {
    expect(thrown("a: 1\na: 2\n").line).toBe(2);
  });

  it("refuses an unclosed quote, list and map", () => {
    expect(thrown('a: "x\n').message).toMatch(/never closed/);
    expect(thrown("a: [1, 2\n").message).toMatch(/never closed/);
    expect(thrown("a: { b: 1\n").message).toMatch(/never closed/);
  });

  it("refuses an unquoted colon and a tab indent, with the line", () => {
    expect(thrown("a: b: c\n").message).toMatch(/double quotes/);
    expect(thrown("a:\n\tb: 1\n").line).toBe(2);
  });

  it("refuses an anchor rather than reading it wrong", () => {
    expect(thrown("a: &x 1\n").message).toMatch(/double quotes/);
  });
});

describe("emitYaml", () => {
  it("writes maps and lists in block style, quoting only what needs it", () => {
    expect(emitYaml({ a: 1, b: "x y", c: [1, 2], d: { e: "f" }, g: [], h: {} })).toBe(
      ["a: 1", 'b: "x y"', "c:", "  - 1", "  - 2", "d:", "  e: f", "g: []", "h: {}", ""].join("\n"),
    );
  });

  it("quotes words YAML would read as something else", () => {
    expect(emitYaml({ a: "true", b: "07:30", c: "30m", d: "0 0 7 * * MON-FRI", e: "" })).toBe(
      ['a: "true"', 'b: "07:30"', 'c: "30m"', 'd: "0 0 7 * * MON-FRI"', 'e: ""', ""].join("\n"),
    );
  });

  it("writes several lines as a block scalar, and one line as a block when asked", () => {
    expect(emitYaml({ a: "one\ntwo\n", b: "x" })).toBe("a: |\n  one\n  two\nb: x\n");
    expect(emitYaml({ instructions: "Hello\n" }, { block: ["instructions"] })).toBe("instructions: |\n  Hello\n");
    expect(emitYaml({ instructions: "Hello" }, { block: ["instructions"] })).toBe("instructions: |-\n  Hello\n");
  });

  it("falls back to quotes for text a block scalar cannot hold", () => {
    expect(emitYaml({ a: " leading\nsecond" })).toBe('a: " leading\\nsecond"\n');
  });

  it("starts a list item with its map's first key", () => {
    expect(emitYaml({ items: [{ a: 1, b: { c: 2 } }, "x"] })).toBe(
      ["items:", "  - a: 1", "    b:", "      c: 2", "  - x", ""].join("\n"),
    );
  });
});

describe("a round trip through emitYaml and parseYaml", () => {
  const cases: Array<[string, YamlValue]> = [
    ["the agent block", parseYaml(SPEC_BLOCK)],
    [
      "awkward text",
      {
        a: 'say "hi": there # not a comment',
        b: "line one\nline two\n\nafter a blank",
        c: "tab\there",
        d: "caf\u00e9 and \u4e2d\u6587",
        e: "- not a list",
        f: "[not, a, list]",
        g: "{not: a map}",
      },
    ],
    [
      "nesting",
      {
        activations: [{ schedule: { cron: "0 0 7 * * *", brief: "Hi" } }, "delegated", { agent: ["a", "b"] }],
        empty: [],
        none: null,
        yes: true,
        n: 12,
        nested: [[1, 2], [3]],
      },
    ],
  ];

  for (const [name, value] of cases) {
    it(`keeps ${name} intact`, () => {
      expect(parseYaml(emitYaml(value))).toEqual(value);
    });
  }

  it("keeps a block scalar's trailing newlines as they were", () => {
    for (const text of ["x", "x\n", "x\n\n", "a\nb", "a\nb\n"]) {
      expect(parseYaml(emitYaml({ t: text }, { block: ["t"] }))).toEqual({ t: text });
    }
  });
});
