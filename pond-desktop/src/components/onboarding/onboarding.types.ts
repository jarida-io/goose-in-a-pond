// Onboarding wizard types.

export interface StepMeta {
  id: string;
  label: string;
  caption: string;
  /** When true, the step's "Skip" button is hidden. */
  required: boolean;
}

export interface OnboardingDraft {
  // About You
  userName: string;
  preferredName: string;
  birthday: string;
  avatar: string;
  // Accessibility
  atypicalSpeech: boolean;
  slowSpeech: boolean;
  highContrast: boolean;
  reduceMotion: boolean;
  // Locale
  language: string;
  timezone: string;
  locationName: string;
  /** Filled by Auto-detect. 0/0 means unset, not the Gulf of Guinea. */
  latitude: number;
  longitude: number;
  enableWeather: boolean;
  // Personality & Identity
  promptStyle: string;
  personality: string;
  assistantName: string;
  ttsVoice: string;
  ttsRate: number;
  // Wake Word
  wakeWord: string;
  wakeWordCustom: string;
  // Extensions
  enableMcpMemory: boolean;
  enableHomeAssistant: boolean;
  enableCalendar: boolean;
}

export interface StepDraftProps {
  draft: OnboardingDraft;
  patch: (p: Partial<OnboardingDraft>) => void;
}

/** System info returned by GET /api/v1/system/info */
export interface SystemInfo {
  hostname: string;
  version: string;
  platform: string;
  arch: string;
}

export interface LangOption {
  key: string;
  label: string;
}

export interface PromptStyle {
  value: string;
  label: string;
  icon: string;
  desc: string;
}


export interface WakePreset {
  value: string;
  label: string;
  desc: string;
}
