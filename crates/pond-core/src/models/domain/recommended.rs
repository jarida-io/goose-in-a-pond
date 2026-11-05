//! GIAP's suggestions among its picks: a reason each, and numbers only where they were measured
//! on that class of machine. Shown, never imposed: only the REST view reads this module, never
//! seeding, startup or activation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Rank {
    Primary,
    Lighter,
    Alternative,
}

/// The machine a measurement was taken on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeviceClass {
    /// The 8 GB Orin Nano, or anything living within its budget.
    Orin,
    Desktop,
}

impl DeviceClass {
    pub fn current() -> Self {
        if super::device_budget::budgeted_device() {
            Self::Orin
        } else {
            Self::Desktop
        }
    }
}

/// What one pick did on one class of machine.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Measured {
    pub device: DeviceClass,
    /// The household's sentence for these numbers.
    pub summary: &'static str,
    pub first_reply_s: Option<f32>,
    pub tokens_per_second: Option<(u32, u32)>,
    pub window_tokens: Option<u32>,
    /// When, as `YYYY-MM-DD`.
    pub measured_on: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Recommendation {
    /// The pick's `"{category}/{name}"`.
    pub model_id: &'static str,
    /// The role it is suggested for.
    pub role: &'static str,
    pub rank: Rank,
    pub reason: &'static str,
    pub measured: &'static [Measured],
}

impl Recommendation {
    /// Numbers for `device`; `None` where nothing was measured there.
    pub fn measured_on(&self, device: DeviceClass) -> Option<&'static Measured> {
        self.measured.iter().find(|m| m.device == device)
    }
}

/// The same picks on every machine; the Orin numbers are the household's logs and P0's runs.
pub const RECOMMENDED: &[Recommendation] = &[
    Recommendation {
        model_id: "gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL",
        role: "chat",
        rank: Rank::Primary,
        reason: "Best answers this pond can run",
        measured: &[Measured {
            device: DeviceClass::Orin,
            summary: "First reply in about 1 s, 15-16 tokens a second, 16k window",
            first_reply_s: Some(1.0),
            tokens_per_second: Some((15, 16)),
            window_tokens: Some(16_384),
            measured_on: "2026-10-05",
        }],
    },
    Recommendation {
        model_id: "gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL",
        role: "chat",
        rank: Rank::Lighter,
        reason: "Faster replies and a smaller download, with a little less depth",
        measured: &[],
    },
    Recommendation {
        model_id: "litert/gemma-4-E4B-it.litertlm",
        role: "chat",
        rank: Rank::Alternative,
        reason: "Runs on Google's LiteRT-LM engine; text only",
        measured: &[Measured {
            device: DeviceClass::Orin,
            summary: "12-14 tokens a second, 8k window",
            first_reply_s: None,
            tokens_per_second: Some((12, 14)),
            window_tokens: Some(8_192),
            measured_on: "2026-10-05",
        }],
    },
    Recommendation {
        model_id: "embedding/embeddinggemma-2",
        role: "embedding",
        rank: Rank::Primary,
        reason: "Finds facts in Swahili, Sheng or English, whichever language you ask in",
        measured: &[Measured {
            device: DeviceClass::Orin,
            summary: "Searches memory in 18 ms on the graphics processor (GPU) or 41 ms on the \
                      main processor (CPU). Puts the right Swahili or mixed-language fact in its \
                      top five 73% of the time, against 39% for all-MiniLM-L6-v2, the default",
            first_reply_s: None,
            tokens_per_second: None,
            window_tokens: None,
            measured_on: "2026-10-31",
        }],
    },
];

pub fn recommendation_for(model_id: &str) -> Option<&'static Recommendation> {
    RECOMMENDED.iter().find(|r| r.model_id == model_id)
}

/// Whether a Memory recommendation stands on this pond: not for a model that takes more than
/// `free_mb`, MemAvailable plus what the model in use gives back, so the margin spent too. The
/// model running now stands; with nothing read, every one does.
pub fn memory_recommendation_stands(takes_mb: u64, free_mb: Option<u64>, running: bool) -> bool {
    running || free_mb.is_none_or(|free| takes_mb <= free)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::domain::curated;
    use crate::models::domain::embedding_models::EMBEDDING_MODELS;

    #[test]
    fn every_recommendation_is_a_pick_with_a_rank_of_its_own() {
        let mut ranks = Vec::new();
        for r in RECOMMENDED {
            let pick = match r.role {
                "chat" => curated::CURATED.iter().any(|p| p.id() == r.model_id),
                "embedding" => EMBEDDING_MODELS.iter().any(|m| m.id() == r.model_id),
                other => panic!("{}: no picks for the {other} role", r.model_id),
            };
            assert!(pick, "{} is not one of GIAP's picks", r.model_id);
            assert!(
                !ranks.contains(&(r.role, r.rank)),
                "{:?} twice for {}",
                r.rank,
                r.role
            );
            ranks.push((r.role, r.rank));
            assert!(!r.reason.is_empty() && !r.reason.ends_with('.'));
        }
        assert_eq!(
            recommendation_for("gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL").map(|r| r.rank),
            Some(Rank::Primary)
        );
        assert_eq!(recommendation_for("gguf/llama-3.2-3b"), None);
    }

    /// A number is shown only where it was measured; nothing has been measured on a desktop.
    #[test]
    fn numbers_exist_only_where_they_were_measured() {
        for r in RECOMMENDED {
            assert!(
                r.measured_on(DeviceClass::Desktop).is_none(),
                "{}",
                r.model_id
            );
            for m in r.measured {
                assert_eq!(m.measured_on.len(), 10, "{}: a date", r.model_id);
                assert!(!m.summary.is_empty() && !m.summary.ends_with('.'));
                // The structured numbers are a chat model's; an embedder's live in its summary.
                if r.role == "chat" {
                    assert!(
                        m.first_reply_s.is_some()
                            || m.tokens_per_second.is_some()
                            || m.window_tokens.is_some()
                    );
                }
            }
        }
        let e2b = recommendation_for("gguf/gemma-4-E2B-it-qat-UD-Q4_K_XL").unwrap();
        assert!(
            e2b.measured.is_empty(),
            "E2B QAT has not been measured on the Orin"
        );
        let eg2 = recommendation_for("embedding/embeddinggemma-2").unwrap();
        assert_eq!((eg2.role, eg2.rank), ("embedding", Rank::Primary));
        assert_eq!(
            eg2.measured_on(DeviceClass::Orin).map(|m| m.measured_on),
            Some("2026-10-31"),
            "P0's run on the nano"
        );
        let e4b = recommendation_for("gguf/gemma-4-E4B-it-qat-UD-Q4_K_XL").unwrap();
        assert_eq!(
            e4b.measured_on(DeviceClass::Orin)
                .and_then(|m| m.window_tokens),
            Some(16_384)
        );
    }

    /// DESIGN.md section 6: an acronym is spelled out where it is used, and a comparison names what
    /// it is compared with.
    #[test]
    fn a_measured_sentence_is_plain_language() {
        for r in RECOMMENDED {
            for m in r.measured {
                for (short, spelled) in [
                    ("GPU", "graphics processor (GPU)"),
                    ("CPU", "main processor (CPU)"),
                ] {
                    assert!(
                        !m.summary.contains(short) || m.summary.contains(spelled),
                        "{}: {short}",
                        r.model_id
                    );
                }
                assert!(
                    !m.summary.contains("for the default"),
                    "{}: name the default",
                    r.model_id
                );
            }
        }
    }

    #[test]
    fn a_memory_recommendation_goes_only_where_the_model_cannot_load() {
        use crate::models::domain::embedding_models::EMBEDDINGGEMMA_2;
        let takes = EMBEDDINGGEMMA_2.ram_estimate_mb();
        assert!(memory_recommendation_stands(takes, Some(takes), false));
        assert!(!memory_recommendation_stands(takes, Some(takes - 1), false));
        assert!(
            memory_recommendation_stands(takes, Some(0), true),
            "the model running here has loaded"
        );
        assert!(memory_recommendation_stands(takes, None, false));
    }
}
