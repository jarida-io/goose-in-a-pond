use serde::{Deserialize, Serialize};
use std::str::FromStr;

/// Onboarding wizard steps. Middleware gates only on `Completed`; an unknown stored step
/// string parses as `Err(())`, which restarts the wizard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OnboardingStep {
    /// GIAP intro; the handshake happens here.
    Welcome,
    /// Display name, preferred name, birthday, avatar.
    Basics,
    /// Locale, timezone, city, weather.
    Location,
    /// Atypical speech, slow TTS, high contrast, reduce motion.
    Accessibility,
    /// Prompt style and assistant personality hint.
    Personality,
    /// Assistant name, TTS voice, household note.
    GooseIdentity,
    /// A preset or custom wake phrase.
    WakeWord,
    /// LLM provider and model.
    Model,
    /// Curated extensions (Weather, MCP Memory).
    Extensions,
    /// Terminal state — unlocks all protected routes.
    Completed,
}

impl OnboardingStep {
    /// Every step in wizard order, `Completed` included; progress counts derive from it.
    pub const ALL: [OnboardingStep; 10] = [
        Self::Welcome,
        Self::Basics,
        Self::Location,
        Self::Accessibility,
        Self::Personality,
        Self::GooseIdentity,
        Self::WakeWord,
        Self::Model,
        Self::Extensions,
        Self::Completed,
    ];

    /// 1-based position within [`OnboardingStep::ALL`] (`Completed` → 10).
    pub fn position(self) -> usize {
        Self::ALL
            .iter()
            .position(|s| *s == self)
            .map(|i| i + 1)
            .unwrap_or(0)
    }

    pub fn next(self) -> Self {
        match self {
            Self::Welcome => Self::Basics,
            Self::Basics => Self::Location,
            Self::Location => Self::Accessibility,
            Self::Accessibility => Self::Personality,
            Self::Personality => Self::GooseIdentity,
            Self::GooseIdentity => Self::WakeWord,
            Self::WakeWord => Self::Model,
            Self::Model => Self::Extensions,
            Self::Extensions => Self::Completed,
            Self::Completed => Self::Completed,
        }
    }

    pub fn is_complete(&self) -> bool {
        matches!(self, Self::Completed)
    }
}

impl ToString for OnboardingStep {
    fn to_string(&self) -> String {
        format!("{:?}", self)
    }
}

impl FromStr for OnboardingStep {
    type Err = ();

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Welcome" => Ok(Self::Welcome),
            "Basics" => Ok(Self::Basics),
            "Location" => Ok(Self::Location),
            "Accessibility" => Ok(Self::Accessibility),
            "Personality" => Ok(Self::Personality),
            "GooseIdentity" => Ok(Self::GooseIdentity),
            "WakeWord" => Ok(Self::WakeWord),
            "Model" => Ok(Self::Model),
            "Extensions" => Ok(Self::Extensions),
            "Completed" => Ok(Self::Completed),
            _ => Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_progression_order() {
        assert_eq!(OnboardingStep::Welcome.next(), OnboardingStep::Basics);
        assert_eq!(OnboardingStep::Basics.next(), OnboardingStep::Location);
        assert_eq!(
            OnboardingStep::Location.next(),
            OnboardingStep::Accessibility
        );
        assert_eq!(
            OnboardingStep::Accessibility.next(),
            OnboardingStep::Personality
        );
        assert_eq!(
            OnboardingStep::Personality.next(),
            OnboardingStep::GooseIdentity
        );
        assert_eq!(
            OnboardingStep::GooseIdentity.next(),
            OnboardingStep::WakeWord
        );
        assert_eq!(OnboardingStep::WakeWord.next(), OnboardingStep::Model);
        assert_eq!(OnboardingStep::Model.next(), OnboardingStep::Extensions);
        assert_eq!(OnboardingStep::Extensions.next(), OnboardingStep::Completed);
    }

    #[test]
    fn completed_is_terminal() {
        assert_eq!(OnboardingStep::Completed.next(), OnboardingStep::Completed);
    }

    #[test]
    fn all_covers_every_variant_and_is_ordered() {
        assert_eq!(OnboardingStep::ALL.len(), 10);
        for pair in OnboardingStep::ALL.windows(2) {
            if pair[0] != OnboardingStep::Completed {
                assert_eq!(pair[0].next(), pair[1]);
            }
        }
    }

    #[test]
    fn position_is_one_based_and_monotonic() {
        assert_eq!(OnboardingStep::Welcome.position(), 1);
        assert_eq!(OnboardingStep::Extensions.position(), 9);
        assert_eq!(OnboardingStep::Completed.position(), 10);
        let positions: Vec<usize> = OnboardingStep::ALL.iter().map(|s| s.position()).collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]));
    }

    #[test]
    fn is_complete_only_for_completed() {
        assert!(OnboardingStep::Completed.is_complete());
        assert!(!OnboardingStep::Welcome.is_complete());
        assert!(!OnboardingStep::Basics.is_complete());
        assert!(!OnboardingStep::Model.is_complete());
        assert!(!OnboardingStep::Extensions.is_complete());
    }

    #[test]
    fn unknown_step_string_returns_err() {
        assert!(OnboardingStep::from_str("VerifyDevice").is_err());
        assert!(OnboardingStep::from_str("CreateProfile").is_err());
        assert!(OnboardingStep::from_str("ConfigurePersonality").is_err());
        assert!(OnboardingStep::from_str("ConnectDevices").is_err());
        assert!(OnboardingStep::from_str("Identity").is_err());
        assert!(OnboardingStep::from_str("Assistant").is_err());
    }

    #[test]
    fn roundtrip_to_string_and_back() {
        for step in [
            OnboardingStep::Welcome,
            OnboardingStep::Basics,
            OnboardingStep::Location,
            OnboardingStep::Accessibility,
            OnboardingStep::Personality,
            OnboardingStep::GooseIdentity,
            OnboardingStep::WakeWord,
            OnboardingStep::Model,
            OnboardingStep::Extensions,
            OnboardingStep::Completed,
        ] {
            let s = step.to_string();
            assert_eq!(OnboardingStep::from_str(&s), Ok(step));
        }
    }
}
