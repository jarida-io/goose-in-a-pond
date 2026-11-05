# DESIGN.md — the Jarida design ethos, as GIAP keeps it

This is the judgment layer: what we decide and why. For the parts list — every token, primitive
and class convention — see [`docs/design-system.md`](docs/design-system.md). That file tells you
what exists. This one tells you what to do with it, and what not to.

The designer-facing edition of this document is
[`docs/design/Jarida_GIAP_DesignEthos_v1.0_20260816.pdf`](docs/design/Jarida_GIAP_DesignEthos_v1.0_20260816.pdf).
Same rules, longer form, printable, with the ethos stated at the end.

The executable edition is the npm package [`@jarida/ink`](https://github.com/jarida-io/ink) — the
tokens, the edge geometry and the components, for React and React Native/Expo. It is a dependency
(`pond-desktop/package.json`), not a workspace package: there is no `packages/ink` in this repo.
Three of the rules below are held there rather than described: there is no `color` prop on any
component, `InkBudget` counts raised surfaces against a budget, and the contrast values that depend
on a runtime accent are derived instead of written down. Its test suite parses `design-tokens.css`
and fails if the two have drifted.

---

## 1. Precedence — read this before resolving any conflict

Three documents govern design here and they do not always agree. When they collide:

| | Source | Wins when |
|---|---|---|
| **1** | **The ink edge** — the shipped system: `design-tokens.css` and the stylesheets that use it | **Always.** It is the only one of the three that is running in front of a household. |
| 2 | *Jarida Product & Brand Design Heuristics* (Aug 2025) | Anything the ink edge does not cover: logo, attribution, tone, licensing, the review gate |
| 3 | *Designing and Building for Constraints* (Spotify Design) | Method only — how to evaluate. It never overrides a Jarida rule |

Known conflicts, already resolved:

- **Charcoal.** The brand deck specifies `#1C1C1C`. The product ships `#171616`. **`#171616` wins**
  — it is what people are looking at. `#1C1C1C` survives in the product as `--grey-900`.
- **Typeface.** The brand deck names Inter for Google Docs and Slides, and *"Quicksand and Comfortaa
  in our products"*. No conflict: GIAP is a product, so Quicksand leads and Comfortaa is its
  declared fallback. Inter is for the org's document templates, not for GIAP's UI.
- **Scoring scale.** Jarida scores 0–5, Spotify 1–5. Both run higher-is-better. We use **Jarida's
  0–5** and borrow Spotify's language for what the low end means.
- **Ground.** `@jarida/ink` and the designer PDF give the ground as Jarida White, `#FFF9F9`. The
  product ships `#FAF3E1`, Jarida Warm White: `hub/state/themeStore.ts` strips ink's inline value
  so the stylesheet's wins, and `ink-adoption.test.ts` exempts it as a brand choice. **`#FAF3E1`
  wins.**
- **Purple.** Jarida Purple, `#8C4BFF`, is the mark. The UI accent runs at `#7C3AED`. Both stay,
  each in its own place: the mark in brand material (§8), the accent in the product (§2).

When you find another conflict, resolve it in favour of the shipped system, then fix whichever
document was wrong in the same change. Do not leave the contradiction for the next person.

---

## 2. The material

Full token table in [`docs/design-system.md`](docs/design-system.md#design-tokens-stylesdesign-tokenscss).
The six that carry the identity, as they run with the default accent, Purple. Each ratio is
measured on its own scheme's ground, `#FAF3E1` in light and `#131119` in dark:

| Token | Light · dark | Contrast, light · dark | Role |
|---|---|---|---|
| `--color-bg` | `#FAF3E1` · `#131119` | — | Jarida Warm White. The ground. Warm, not neutral |
| `--color-text` | `#171616` · `#F3F1F8` | 16.32:1 · 16.71:1 | Jarida Charcoal. Body text |
| `--color-accent` | `#7C3AED` · `#7C3AED` | 5.15:1 · **3.28:1** | The household's accent. Edges, rings, fills — **never body text** |
| `--border-ink` | `#7C3AED` · `#7D3BED` | 5.15:1 · 3.32:1 | The ink line, and the hard offset shadow. Follows the accent |
| `--color-text-secondary` | `#566178` · `rgba(243,241,248,0.55)` | 5.62:1 · 5.65:1 | Slate. Meta, captions, provenance |
| `--bg-brand-soft` | `#F5F3FF` · `#241C33` | — | Quiet fill |

`design-tokens.css` alone does not give these values. `hub/state/themeStore.ts` writes
`@jarida/ink`'s tokens inline on `<html>`, and inline beats the stylesheet, so the accent, text and
fill above are ink's. It then removes ink's `--color-bg`, so the ground is the stylesheet's, and
re-points `--border-ink` at the chosen accent, floored at 3:1 on the page and every panel. Dark
Purple is 2.97:1 on the `#1E1B26` panel, so its edge moves one step per channel to `#7D3BED`, too
small to see. Measure what the page computes, not what a file says.

**The accent is not a text colour.** The household picks it under Appearance → Accent. Purple, the
default, is 5.15:1 on the light ground, enough for body copy, and 3.28:1 on the dark one, which is
not. On the light ground Blue is 4.67:1, Magenta 4.15:1, Teal 3.38:1 and Coral 2.53:1. No accent
on offer clears the 4.5:1 body copy needs in both schemes. So the accent is an edge, a ring and a
fill, and it is never a sentence. As a line it needs the 3:1 WCAG asks of a focus indicator or a UI
boundary, which Coral does not have on its own; that is why `--border-ink` and `--focus-ring` are
derived from the accent rather than copied from it. This is not a preference; it is the measurement.

**Two purples.** `#8C4BFF` is Jarida Purple, the mark: the logo and the brand material in §8. It
measures 4.16:1 on the light ground. `#7C3AED` is the purple the product runs at. They differ on
purpose, and `themeStore.ts` says why: collapsing them either dulls the logo or fails text contrast.
The mark survives in the stylesheet as `--purple-500` and in a few fixed hues, such as the voice
orb's listening state.

Type is **Quicksand** (display, heading, body) with **Comfortaa** declared as its fallback, and
**JetBrains Mono** for data — numbers, timestamps, file paths, measurements. Screen titles are set
bold italic at `-0.02em`; that italic is the single most recognisable thing about our typography
and it is why a settings page reads as something built for a house rather than a server rack.

---

## 3. The rules

Each is enforced somewhere. Cited so you can read the original argument rather than trust this
summary — and so you can tell when the summary has gone stale.

**State is elevation, not hue.** A chosen thing rises on its offset; everything unchosen lies flat
with a hairline. Colour never means *selected*. Spending the page's one real colour on decoration
leaves nothing for the control that is actually asking a question.
`pond-desktop/src/hub/views/settings/voice-picker.css`

**Spend the offset about twice per screen, and mean it both times.** On the voice picker: the voice
you chose, and the button that speaks it. Three levels of the same treatment is how a signal stops
being one.
`voice-picker.css`

**The edge marks content, never chrome.** Cards wear the 2px ink border and the hard offset because
they are what the screen is for. Toolbars, filters and navigation stay on a hairline. If the chrome
starts shouting, nothing on the page is loud any more.
`styles/chat-history.css`, `styles/models.css`

**Press is physical.** At rest a card floats 4px above its shadow; hover lifts it to 7px; pressing
drives it to 0 and translates the card by exactly the offset it loses, so it lands in the space its
own shadow occupied. It reads as a key being struck rather than a rectangle that got darker.
`styles/chat-history.css` (`--ink-lift`)

**Let form carry data.** On the conversation wall a card is as tall as its conversation was long —
three computed heights, not decorative ones. Shape is a channel most interfaces leave switched off.
`styles/chat-history.css`

**Never invent meaning the data lacks.** Voices are not colour-coded: they differ by name, accent
and grade, and assigning each a hue would assert a structure that does not exist. An interface that
colours things arbitrarily teaches people to read patterns that are not there.
`voice-picker.css`

**Every colour carries its measurement.** A token that moved says what it measured and why, in the
comment, with the ratio. Secondary text left slate-400 at 2.6:1. The focus ring left a 45% tint at
2.06:1 for solid accent. The next person inherits the reasoning, not just the hex.
`styles/design-tokens.css`

**Touch the chrome, not the data.** Visual size and hit size are different things. Navigation and
primary actions grow *visually* to 44px. Dense controls — filter pills, chips, view toggles — keep
a 36px look and reach 44px through an invisible extension, **vertically only**, because growing
sideways makes neighbouring pills steal each other's taps.
`styles/touch.css`

**Say what happened, not that something did.** Applying a voice reports whether it downloaded,
whether the engine reloaded, and which tier is actually running — because a swap, a half-megabyte
fetch and a 326 MB one feel nothing alike. A screen that cannot tell them apart has to either spin
or lie.
`crates/pond-core/src/models/ports/tts_control.rs`

---

## 4. Two surfaces, one material

Neither is deprecated. They share tokens, type and the ink edge; what they do not share is how
tightly they pack.

**Hub** — 1024×600 and 800×480, a panel on a shelf, driven by a finger and read from across a
room. Every target clears 44px on its own. One decision per screen.

**Classic** — desktop, keyboard and mouse, someone who wants all of it at once. Density is the
entire reason it exists: Logs puts ten filter pills in a row. Compact visuals, full hit areas
underneath. Still desktop software — one hairline, not a phone toolbar.

The standing temptation is to inflate the dense view until it matches the touch one. That trade
deletes the only reason to keep it.

---

## 5. Designing under constraint

Jarida's heuristics require low-bandwidth, low-end-device, offline-first thinking for the African
context. The Spotify toolkit supplies the method, built around three constraints — Device, Data,
Network. GIAP does not inherit those unchanged, because a pond is not a phone. Translate before you
score:

| Toolkit constraint | What it is here |
|---|---|
| **Network** | *There isn't one, by design.* The pond runs local-first and egress is gated (`check_egress`). The question is not "does it degrade gracefully offline" but "does it work with `network_mode = offline` at all" |
| **Device** | The Jetson Orin Nano **is** the low-end device: six cores and 8 GB shared between CPU and GPU, with the language model wanting all of it. Every millisecond speech takes is one inference does not get |
| **Data** | Model weights are the data cost. 92 MB, 154 MB, 326 MB are real choices a household pays for once, over a link that may be metered. Say the number before they spend it |
| **Screen** | 1024×600 and 800×480, often dim, often viewed at arm's length or further |

The toolkit's own warning applies with force here: **compare against other products under the same
constraints, not against your product under different ones.** A voice engine that feels instant on
an M-series Mac and stutters on the Orin has not passed; it has only been measured in the wrong
place.

What that looks like when done properly: Kokoro TTS at the default quality tier ran at **RTF 1.454**
on the Orin — slower than real time, so the pond fell further behind the longer it talked. Moving
to `q4f16` and deriving the ONNX thread count from the machine brought it to **RTF 0.780**. A third
tier, `q8f16`, returned a full-length buffer of digital silence on that board while working
perfectly on a Mac. None of that is visible from a laptop. Measure on the target.

---

## 6. Accessibility is a floor, not a feature

From the Jarida heuristics, non-negotiable:

- **WCAG 2.1 AA** — 4.5:1 normal text, 3:1 large text and UI indicators. Put the measured ratio in
  the comment when you pick or change a colour.
- **Keyboard** — every interactive element reachable and operable, with a visible focus state.
- **Screen readers** — semantic HTML, labelled headings, buttons and forms. Alt text on every image.
- **Motion** — honour `prefers-reduced-motion`. The ink lift is the one motion we rely on and it
  must be disable-able.
- **Language** — plain, inclusive, gender-neutral, jargon explained on first use. Acronyms spelled
  out the first time.

Two GIAP-specific ones that follow from the hardware: text must stay legible on a **dimmed 800×480
panel**, and important UI lines must stay visible on a **low-contrast screen** — which is another
reason the ink edge is 2px and not a hairline.

---

## 7. Voice and copy

Tone is **friendly, confident, inspiring, inclusive** — and, in the original phrasing, *defiant*.
Write from the household's side of the screen. Name things by what people control, never by how the
system is built.

- Active voice. A control says exactly what happens: "Save changes", not "Submit".
- An action keeps its name through the whole flow. The button that says *Publish* produces a toast
  that says *Published*.
- Errors explain what went wrong and what to do next, in the interface's voice. They do not
  apologise and they are never vague.
- An empty screen is an invitation to act, not a mood.
- **No emojis in UI or code.** `lucide-react` for functional icons, the official logo for brand.
  `pond-desktop/src/no-emoji.test.ts` scans source *including comments* and fails the build.

---

## 8. Brand and attribution

From the Jarida heuristics — these apply to anything leaving the project:

- Logo in `.SVG` / `.EPS` / `.PNG` (transparent). Clear space of **1× logo height** on all sides.
  Minimum **24px** digital, **10mm** print.
- Purple logo on light or neutral grounds; black where purple clashes. Never on a photograph
  without an overlay of at least 70% opacity behind it.
- The purple here is the mark, `#8C4BFF`. The product's accent, `#7C3AED`, is a different value
  (§2); don't take one for the other.
- Never stretch, skew, rotate, recolour outside the palette, or add effects.
- Credit **"Jarida Open Source Community"** unless waived; third-party adaptations comply with the
  **Jarida Fair License (JFL)** attribution clause.
- Documents follow `[Project][DocumentType][Version#]_[YYYYMMDD]`, e.g.
  `Jarida_GIAP_DesignEthos_v1.0_20260816.pdf`.

The GIAP logo stays where it already is; the Jarida org logo goes in new, appropriate places. Do
not swap one for the other in existing surfaces.

---

## 9. The gate — how work is judged before release

Jarida's scoring methodology, applied to design work. Score every criterion **0–5**:

| Score | Meaning |
|---|---|
| 0 | Not addressed, or a critical violation — a showstopper that prevents the task |
| 1 | Very poor; does not meet basic requirements |
| 2 | Below standard; significant revision needed — real delay, frustration or confusion |
| 3 | Meets the minimum, with room to improve — minor effect on usability |
| 4 | Meets the standard well; refinements possible |
| 5 | Excellent; not an issue |

Process: **at least two reviewers** score independently, scores are averaged, and the decision is
**release, revise, or hold**. The scoring sheet is stored in the project repository. Don't
overthink the scoring — a day or two, then discuss the disagreements, because the disagreements are
where the information is. Turn the lowest-scoring items into an actual plan.

What to score for GIAP: the nine rules in §3, the constraint translation in §5, the accessibility
floor in §6, the copy standards in §7, and the brand rules in §8.

---

## 10. The ethos

> Paper, one purple line, and the height between them.
>
> The edge marks what the screen is for, and nothing else raises its voice. State is elevation, not
> hue. Colour is spent once, on the control that is actually asking a question. Every number we
> show has been measured on the machine it will run on — not on the machine we build from. Nothing
> encodes a meaning the data does not have, and nothing says a thing happened without saying which
> thing.
>
> We build for a house on a shelf, on six cores that the language model also wants, on a panel that
> may be dim and a link that may be metered — and we treat that as the normal case rather than the
> degraded one.

---

*Governed by the Jarida Product & Brand Design Heuristics (Aug 2025). Method adapted from Designing
and Building for Constraints, Spotify Design, by Dominika Mazur and Linnea Strid. Maintained by
Jerry Ochieng Anyumba for the Jarida Open Source Community.*
