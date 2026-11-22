//! A scripted [`ExtractionGate`]: one ruling for every window, and what each window showed it.

use std::sync::Mutex;

use anyhow::Result;
use async_trait::async_trait;

use crate::user_data::ports::conversation_extractor::ExtractionWindow;
use crate::user_data::ports::extraction_gate::ExtractionGate;
use crate::user_data::services::extraction_gate::GateCandidate;

enum Ruling {
    Drops(Vec<(usize, String)>),
    Unreadable,
    Unheard,
}

pub struct ScriptedExtractionGate {
    ruling: Ruling,
    shown: Mutex<Vec<Vec<GateCandidate>>>,
}

impl ScriptedExtractionGate {
    fn ruling(ruling: Ruling) -> Self {
        Self {
            ruling,
            shown: Mutex::new(Vec::new()),
        }
    }

    /// Drops the candidates at these places, counted from 0, each for its reason.
    pub fn dropping(drops: &[(usize, &str)]) -> Self {
        Self::ruling(Ruling::Drops(
            drops
                .iter()
                .map(|(place, why)| (*place, (*why).to_string()))
                .collect(),
        ))
    }

    pub fn keeping_everything() -> Self {
        Self::dropping(&[])
    }

    /// Answers every window with a reply nobody can read.
    pub fn unreadable() -> Self {
        Self::ruling(Ruling::Unreadable)
    }

    /// Fails every call, as a model a person's turn took does.
    pub fn unheard() -> Self {
        Self::ruling(Ruling::Unheard)
    }

    /// The candidates of each window it was asked about, in the order asked.
    pub fn shown(&self) -> Vec<Vec<GateCandidate>> {
        self.shown.lock().unwrap().clone()
    }

    pub fn calls(&self) -> usize {
        self.shown.lock().unwrap().len()
    }
}

#[async_trait]
impl ExtractionGate for ScriptedExtractionGate {
    async fn judge(
        &self,
        _window: ExtractionWindow<'_>,
        candidates: &[GateCandidate],
    ) -> Result<Option<Vec<(usize, String)>>> {
        self.shown.lock().unwrap().push(candidates.to_vec());
        match &self.ruling {
            Ruling::Drops(drops) => Ok(Some(drops.clone())),
            Ruling::Unreadable => Ok(None),
            Ruling::Unheard => anyhow::bail!("a person's turn took the model"),
        }
    }
}
