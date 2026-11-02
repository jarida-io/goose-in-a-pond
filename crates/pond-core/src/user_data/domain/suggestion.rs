//! Domain model for suggestions and their grounding context.

use serde::{Deserialize, Serialize};

/// Grounding context that originated a suggestion.
///
/// Rides `<suggestion-context>` in `<system-context>` so the model knows
/// exactly which entities, memory, or prior conversation thread spawned this turn.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SuggestionContext {
    Environmental {
        device_ids: Vec<String>,
        summary: String,
    },
    SessionContinuity {
        prior_session_id: String,
        topic: String,
        summary: String,
    },
    Temporal {
        event_title: String,
        timing_summary: String,
    },
    PersonalMemory {
        source_memory_id: String,
        fact: String,
    },
}

impl SuggestionContext {
    /// Render into XML-like envelope suitable for `<system-context>`.
    pub fn render_envelope(&self) -> String {
        match self {
            Self::Environmental {
                device_ids,
                summary,
            } => {
                let devices = device_ids.join(", ");
                format!(
                    "<suggestion-context origin=\"environmental\">\n\
                     Observed state: {summary}\n\
                     Target devices: {devices}\n\
                     </suggestion-context>"
                )
            }
            Self::SessionContinuity {
                prior_session_id,
                topic,
                summary,
            } => {
                format!(
                    "<suggestion-context origin=\"session_continuity\" prior_session=\"{prior_session_id}\">\n\
                     Topic: {topic}\n\
                     Prior state: {summary}\n\
                     </suggestion-context>"
                )
            }
            Self::Temporal {
                event_title,
                timing_summary,
            } => {
                format!(
                    "<suggestion-context origin=\"temporal\">\n\
                     Event: {event_title}\n\
                     Timing: {timing_summary}\n\
                     </suggestion-context>"
                )
            }
            Self::PersonalMemory {
                source_memory_id,
                fact,
            } => {
                format!(
                    "<suggestion-context origin=\"memory\" source_memory_id=\"{source_memory_id}\">\n\
                     Grounding fact: {fact}\n\
                     </suggestion-context>"
                )
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environmental_envelope_renders_device_ids_and_summary() {
        let ctx = SuggestionContext::Environmental {
            device_ids: vec!["light.patio_1".to_string(), "light.patio_2".to_string()],
            summary: "lights on after sunset".to_string(),
        };
        let env = ctx.render_envelope();
        assert!(env.starts_with("<suggestion-context origin=\"environmental\">"));
        assert!(env.contains("Target devices: light.patio_1, light.patio_2"));
        assert!(env.contains("Observed state: lights on after sunset"));
        assert!(env.ends_with("</suggestion-context>"));
    }

    #[test]
    fn session_continuity_envelope_renders_topic_and_prior_state() {
        let ctx = SuggestionContext::SessionContinuity {
            prior_session_id: "s123".to_string(),
            topic: "flight booking".to_string(),
            summary: "compared two airlines".to_string(),
        };
        let env = ctx.render_envelope();
        assert!(env.starts_with(
            "<suggestion-context origin=\"session_continuity\" prior_session=\"s123\">"
        ));
        assert!(env.contains("Topic: flight booking"));
        assert!(env.contains("Prior state: compared two airlines"));
        assert!(env.ends_with("</suggestion-context>"));
    }
}
