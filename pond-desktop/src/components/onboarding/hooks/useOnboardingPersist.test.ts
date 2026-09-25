// The profile patch is a contract with `routes.rs :: particulars_for`, which reads snake_case
// keys from `profiles.preferences`; a camelCase key still gets a 200 but the model hears nothing.

import { describe, it, expect } from "vitest";
import { buildProfilePatch, PROFILE_PREF_KEYS } from "./useOnboardingPersist";
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

describe("buildProfilePatch", () => {
  it("emits the snake_case keys the server actually reads", () => {
    const patch = buildProfilePatch("about-you", draft())!;

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
    const on = buildProfilePatch("about-you", draft({ atypicalSpeech: true }))!;
    expect(on["accessibility_atypical_speech"]).toBe("true");
    expect(typeof on["accessibility_atypical_speech"]).toBe("string");

    const off = buildProfilePatch("about-you", draft({ atypicalSpeech: false }))!;
    expect(off["accessibility_atypical_speech"]).toBe("false");
  });

  it("omits an empty value rather than writing a blank one", () => {
    // The prompt builder renders a present-but-empty key ("The user's birthday is .").
    const patch = buildProfilePatch("about-you", draft({ birthday: "", preferredName: "  " }))!;
    expect(patch).not.toHaveProperty("birthday");
    expect(patch).not.toHaveProperty("preferred_name");
    // A real value on the same step still comes through.
    expect(patch["accessibility_atypical_speech"]).toBe("false");
  });

  it("returns nothing for a step that carries no member preferences", () => {
    expect(buildProfilePatch("assistant", draft())).toBeNull();
  });
});
