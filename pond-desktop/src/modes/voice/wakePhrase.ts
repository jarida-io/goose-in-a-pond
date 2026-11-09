// Wake-phrase rules shared by the browser wake listener, Settings and onboarding. Matching
// mirrors `normalize_transcript` / `contains_trigger` in crates/pond-voice/src/text.rs, and
// `wakePhraseError` mirrors the server's `Check::WakePhrase`.

/** The sentences the calibration screen asks for, each containing `phrase`. Must produce the
 *  same list as `calibration_prompts` in crates/pond-voice/src/text.rs, which uses it to find the
 *  phrase in a take when no prompt is sent. */
export function buildCalibrationPrompts(phrase: string): string[] {
  const p = phrase.trim();
  return [`${p}`, `Hey ${p}, good morning`, `OK ${p}, what time is it`, `${p}, tell me a joke`, `Thank you ${p}`];
}

/** Lowercase letter-only words separated by single spaces, as the server normalizes. */
export function normalizeTranscript(s: string): string {
  return s.replace(/[^\p{L}]+/gu, " ").trim().split(/\s+/).filter(Boolean).join(" ").toLowerCase();
}

/** Whether normalized `haystack` holds normalized `trigger` as whole words, in order. */
export function containsTrigger(haystack: string, trigger: string): boolean {
  const hay = haystack.split(" ").filter(Boolean);
  const needle = trigger.split(" ").filter(Boolean);
  if (needle.length === 0 || needle.length > hay.length) return false;
  for (let i = 0; i + needle.length <= hay.length; i++) {
    if (needle.every((w, k) => hay[i + k] === w)) return true;
  }
  return false;
}

/** Whether a reply ends by asking something: only then is the next thing said taken for an answer
 *  without the wake word. Mirrors `asks_for_an_answer` in crates/pond-voice/src/text.rs. */
export function asksForAnAnswer(reply: string): boolean {
  return /[?？]$/u.test(reply.replace(/[\s"')\]*_”’»]+$/u, ""));
}

/** Why a phrase cannot be stored, or `null`. */
export function wakePhraseError(phrase: string): string | null {
  const p = phrase.trim();
  if (!/^[\p{L}\s'-]*$/u.test(p)) return "Use only letters, spaces, apostrophes and hyphens.";
  const words = p.split(/\s+/).filter(Boolean);
  if (words.length > 4 || p.length > 40) return "Keep it to four words or fewer.";
  if ([...p].filter((c) => /\p{L}/u.test(c)).length < 2) return "Type a word or two to say.";
  return null;
}

const DIGRAPHS = ["igh", "tch", "th", "sh", "ch", "ph", "wh", "ng", "ck", "qu", "oo", "ee", "ea", "ai", "ay", "oa", "ou", "ow", "oi", "oy", "au", "ey"];

/** Rough count of speech sounds in one English word: digraphs count once, a silent final e
 *  not at all, x twice, a doubled consonant once. */
export function estimateSounds(word: string): number {
  const w = word.toLowerCase().replace(/[^a-z]/g, "");
  let n = 0;
  let prevVowel = false;
  for (let i = 0; i < w.length; ) {
    const d = DIGRAPHS.find((g) => w.startsWith(g, i));
    if (d) {
      n += d === "qu" ? 2 : 1;
      prevVowel = /[aeiou]/.test(d) && d !== "qu";
      i += d.length;
      continue;
    }
    const c = w[i];
    const vowel = /[aeiou]/.test(c) || (c === "y" && i > 0);
    if (c === "e" && i === w.length - 1 && w.length > 2 && !prevVowel) { i++; continue; }
    if (i > 0 && w[i - 1] === c && !vowel) { i++; continue; }
    if (!(vowel && prevVowel)) n += c === "x" ? 2 : 1;
    prevVowel = vowel;
    i++;
  }
  return n;
}

/** Fewer sounds than this and a phrase wakes by accident far more often (Picovoice's guidance;
 *  "Alexa" has six). */
export const MIN_SOUNDS = 6;

/** A gentle note about a phrase that is valid but likely to wake by accident, or `null`. */
export function wakePhraseWarning(phrase: string): string | null {
  if (wakePhraseError(phrase)) return null;
  const words = normalizeTranscript(phrase).split(" ").filter(Boolean);
  const sounds = words.reduce((n, w) => n + estimateSounds(w), 0);
  if (sounds >= MIN_SOUNDS) return null;
  const opener = ["hey", "ok", "okay"].includes(words[0]) ? "" : ` Try "hey ${words.join(" ")}".`;
  return `Short phrases like this wake the pond by accident more often.${opener}`;
}
