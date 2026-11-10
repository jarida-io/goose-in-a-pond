// The profile patch is a contract with `routes.rs :: particulars_for`, which reads snake_case
// keys from `profiles.preferences`; a camelCase key still gets a 200 but the model hears nothing.

import { describe, it, expect } from "vitest";
import { buildProfilePatch, buildSettingsPatch, PROFILE_PREF_KEYS } from "./useOnboardingPersist";
import type { OnboardingDraft } from "../onboarding.types";

function draft(over: Partial<OnboardingDraft> = {}): OnboardingDraft {
  return {
    userName: "Jerry",
    preferredName: "Cap",
    birthday: "1990-04-02",
    avatar: "duck",
    atypicalSpeech: false,
    slowSpeech: false,
    highContrast: false,
    reduceMotion: false,
    ...over,
  } as OnboardingDraft;
}

/** A draft that looks fully filled-in (as one pre-filled from real settings would), so a test
 *  sending an empty `touched` set is actually exercising "nothing was touched", not "there was
 *  nothing to send". */
function filledDraft(over: Partial<OnboardingDraft> = {}): OnboardingDraft {
  return {
    userName: "Jerry",
    timezone: "Africa/Nairobi",
    locationName: "Nairobi",
    enableWeather: true,
    latitude: -1.286389,
    longitude: 36.817223,
    promptStyle: "concise",
    personality: "friendly and concise",
    assistantName: "Goose",
    ttsVoice: "af_heart",
    ttsRate: 120,
    wakeWord: "hey goose",
    wakeWordCustom: "",
    enableMcpMemory: true,
    ...over,
  } as OnboardingDraft;
}

describe("buildSettingsPatch", () => {
  it("sends nothing for a step the user clicked through untouched", () => {
    const touched = new Set<keyof OnboardingDraft>();
    expect(buildSettingsPatch("personality", filledDraft(), touched)).toEqual({});
    expect(buildSettingsPatch("wake-word", filledDraft(), touched)).toEqual({});
    expect(buildSettingsPatch("locale", filledDraft(), touched)).toEqual({});
    expect(buildSettingsPatch("about-you", filledDraft(), touched)).toEqual({});
    expect(buildSettingsPatch("complete", filledDraft(), touched)).toEqual({});
  });

  it("sends only the field the user actually changed, not its step-mates", () => {
    const touched = new Set<keyof OnboardingDraft>(["personality"]);
    const patch = buildSettingsPatch("personality", filledDraft(), touched);
    expect(patch).toEqual({ assistant_personality: "friendly and concise" });
  });

  it("sends every field once every field in the step was touched", () => {
    const touched = new Set<keyof OnboardingDraft>([
      "promptStyle", "personality", "assistantName", "ttsVoice", "ttsRate",
    ]);
    const patch = buildSettingsPatch("personality", filledDraft(), touched);
    expect(patch).toEqual({
      prompt_style: "concise",
      assistant_personality: "friendly and concise",
      assistant_name: "Goose",
      voice_tts_voice: "af_heart",
      voice_tts_speed: 1.2,
    });
  });

  it("a custom wake phrase counts as touched via wakeWordCustom, not the preset field", () => {
    const customDraft = filledDraft({ wakeWord: "custom", wakeWordCustom: "hey pond" });

    // The preset radio was never actually clicked onto "custom" in this run (e.g. it arrived
    // pre-filled) — only the free-text field was. Still must send.
    const touched = new Set<keyof OnboardingDraft>(["wakeWordCustom"]);
    expect(buildSettingsPatch("wake-word", customDraft, touched)).toEqual({
      voice_wake_word: "hey pond",
    });

    // Neither was touched — a filled-in custom phrase must not be resent as if chosen.
    expect(buildSettingsPatch("wake-word", customDraft, new Set())).toEqual({});
  });

  it("still omits geocoded coordinates even if latitude/longitude end up touched but zero", () => {
    const touched = new Set<keyof OnboardingDraft>(["latitude", "longitude"]);
    const patch = buildSettingsPatch("locale", filledDraft({ latitude: 0, longitude: 0 }), touched);
    expect(patch).not.toHaveProperty("weather_latitude");
    expect(patch).not.toHaveProperty("weather_longitude");
  });

  it("returns null for a step with no Settings fields", () => {
    expect(buildSettingsPatch("welcome", filledDraft(), new Set())).toBeNull();
  });
});

const ALL_ABOUT_YOU_FIELDS = new Set<keyof OnboardingDraft>([
  "preferredName", "birthday", "avatar", "atypicalSpeech", "slowSpeech", "highContrast", "reduceMotion",
]);

describe("buildProfilePatch", () => {
  it("emits the snake_case keys the server actually reads", () => {
    const patch = buildProfilePatch("about-you", draft(), ALL_ABOUT_YOU_FIELDS)!;

    // Literal keys, not PROFILE_PREF_KEYS: deriving them would test the constant against itself.
    expect(patch).toHaveProperty("preferred_name", "Cap");
    expect(patch).toHaveProperty("birthday", "1990-04-02");

    for (const camel of ["preferredName", "atypicalSpeech", "slowSpeech", "highContrast"]) {
      expect(patch, `camelCase key ${camel} would be stored and never read`).not.toHaveProperty(
        camel,
      );
    }
  });

  it("keeps the mapping honest about which spelling is the server's", () => {
    expect(PROFILE_PREF_KEYS.preferredName).toBe("preferred_name");
    expect(PROFILE_PREF_KEYS.atypicalSpeech).toBe("accessibility_atypical_speech");
    expect(PROFILE_PREF_KEYS.language).toBe("language");
  });

  it("writes booleans as the literal strings the server compares against", () => {
    // `profiles.preferences` is HashMap<String, String>; the server compares against "true".
    const on = buildProfilePatch("about-you", draft({ atypicalSpeech: true }), ALL_ABOUT_YOU_FIELDS)!;
    expect(on["accessibility_atypical_speech"]).toBe("true");
    expect(typeof on["accessibility_atypical_speech"]).toBe("string");

    const off = buildProfilePatch("about-you", draft({ atypicalSpeech: false }), ALL_ABOUT_YOU_FIELDS)!;
    expect(off["accessibility_atypical_speech"]).toBe("false");
  });

  it("omits an empty value rather than writing a blank one", () => {
    // The prompt builder renders a present-but-empty key ("The user's birthday is .").
    const patch = buildProfilePatch(
      "about-you",
      draft({ birthday: "", preferredName: "  " }),
      ALL_ABOUT_YOU_FIELDS,
    )!;
    expect(patch).not.toHaveProperty("birthday");
    expect(patch).not.toHaveProperty("preferred_name");
    // A real value on the same step still comes through.
    expect(patch["accessibility_atypical_speech"]).toBe("false");
  });

  it("returns nothing for a step that carries no member preferences", () => {
    expect(buildProfilePatch("assistant", draft(), ALL_ABOUT_YOU_FIELDS)).toBeNull();
  });

  it("sends nothing when the user clicks through About You untouched", () => {
    // avatar defaults to a non-empty emoji and the flags default to "false" — both non-empty
    // strings, so only the `touched` gate (not the empty-value filter) can catch these.
    const patch = buildProfilePatch("about-you", draft(), new Set())!;
    expect(patch).toEqual({});
  });

  it("sends only the fields the user actually touched, not the untouched step-mates", () => {
    const patch = buildProfilePatch(
      "about-you",
      draft({ avatar: "duck", atypicalSpeech: true }),
      new Set<keyof OnboardingDraft>(["avatar"]),
    )!;
    expect(patch).toEqual({ [PROFILE_PREF_KEYS.avatar]: "duck" });
  });
});
