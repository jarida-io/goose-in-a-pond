// Step 1, About You (Basics + Accessibility); userName is required.

import { MessageCircle, Turtle, Contrast, CircleSlash } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { FormLabel } from "../primitives/FormLabel";
import { Lead } from "../primitives/StepShell";
import { AVATARS } from "../onboarding.constants";

const ICON_PROPS = { size: 18, strokeWidth: 1.8 } as const;

export function StepAboutYou() {
  const { draft, patch } = useOnboarding();

  const accessibilityItems = [
    {
      key: "atypicalSpeech" as const,
      icon: <MessageCircle {...ICON_PROPS} />,
      title: "Atypical speech support",
      desc: "Goose waits longer before responding \u2014 helpful for stutter, pauses, or non-standard speech patterns.",
    },
    {
      key: "slowSpeech" as const,
      icon: <Turtle {...ICON_PROPS} />,
      title: "Slow speech mode",
      desc: "Goose speaks at a reduced pace so responses are easier to follow.",
    },
    {
      key: "highContrast" as const,
      icon: <Contrast {...ICON_PROPS} />,
      title: "High contrast",
      desc: "Increases visual contrast across the dashboard interface.",
    },
    {
      key: "reduceMotion" as const,
      icon: <CircleSlash {...ICON_PROPS} />,
      title: "Reduce motion",
      desc: "Disables animations and transitions throughout the interface.",
    },
  ];

  return (
    <div className="ob-step-content">
      <Lead>
        Goose uses this to greet you by name and personalise responses.
      </Lead>

      {/* Name fields */}
      <div className="ob-field-grid">
        <div>
          <FormLabel>Your name <span className="ob-required">*</span></FormLabel>
          <input
            className="ob-input"
            placeholder="e.g. Jack Smith"
            value={draft.userName}
            onChange={(e) => patch({ userName: e.target.value })}
            required
          />
        </div>
        <div>
          <FormLabel optional>Preferred name</FormLabel>
          <input
            className="ob-input"
            placeholder="What should Goose call you?"
            value={draft.preferredName}
            onChange={(e) => patch({ preferredName: e.target.value })}
          />
        </div>
      </div>

      {/* Avatar */}
      <div className="ob-field">
        <FormLabel optional>Avatar</FormLabel>
        <div className="ob-avatar-grid">
          {AVATARS.map((em) => (
            <button
              key={em}
              type="button"
              onClick={() => patch({ avatar: em })}
              className={`ob-avatar-btn ${draft.avatar === em ? "ob-avatar-btn--selected" : ""}`}
            >
              {em}
            </button>
          ))}
        </div>
      </div>

      {/* Birthday */}
      <div className="ob-field ob-field--narrow">
        <FormLabel optional>Birthday</FormLabel>
        <input
          className="ob-input"
          type="date"
          value={draft.birthday}
          onChange={(e) => patch({ birthday: e.target.value })}
        />
        <p className="ob-field-hint">
          Goose will wish you well on the day
        </p>
      </div>

      {/* Accessibility section */}
      <div className="ob-field">
        <FormLabel>Accessibility</FormLabel>
        <p className="ob-field-hint">
          All options can be changed at any time in Settings &rarr; Accessibility.
        </p>
        <div className="ob-toggle-stack">
          {accessibilityItems.map((it) => (
            <div key={it.key} className="ob-toggle-row">
              <span className="ob-toggle-row__icon">{it.icon}</span>
              <div className="ob-toggle-row__body">
                <div className="ob-toggle-row__header">
                  <strong className="ob-toggle-row__title">{it.title}</strong>
                </div>
                <p className="ob-toggle-row__desc">{it.desc}</p>
              </div>
              <button
                type="button"
                role="switch"
                aria-checked={draft[it.key]}
                onClick={() => patch({ [it.key]: !draft[it.key] })}
                className={`ob-switch ${draft[it.key] ? "ob-switch--on" : ""}`}
              >
                <span className="ob-switch__thumb" />
              </button>
            </div>
          ))}
        </div>
      </div>
    </div>
  );
}
