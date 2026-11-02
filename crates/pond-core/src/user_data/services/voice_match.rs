//! Voice Match enrolment: one flow of five takes teaches the wake word and the member's voice.
//! What the takes add up to: each member's latest run, whether the wake CNN hears the household,
//! and how alike a member's prints are.

use std::collections::BTreeMap;

use chrono::{DateTime, Duration, Utc};

use crate::user_data::domain::voice_print::{VoicePrint, WakeTake};

/// Sentences in one enrolment run, and prints a member needs before the room recognises them.
pub const TAKES_PER_RUN: usize = 5;
/// The CNN takes over from Whisper only when it heard this many of every member's latest run.
pub const CNN_TAKES_NEEDED: usize = 4;
/// A take this long after a member's previous one starts a new run.
pub const RUN_GAP: Duration = Duration::minutes(15);

/// A member's most recent run of takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeRun {
    pub profile_id: Option<String>,
    pub takes: usize,
    pub heard: usize,
}

/// Each taught member's latest run; a run ends at [`TAKES_PER_RUN`] takes or a [`RUN_GAP`].
pub fn latest_runs(takes: &[WakeTake]) -> Vec<TakeRun> {
    let mut by_member: BTreeMap<Option<&str>, Vec<&WakeTake>> = BTreeMap::new();
    for take in takes {
        by_member
            .entry(take.profile_id.as_deref())
            .or_default()
            .push(take);
    }
    by_member
        .into_iter()
        .map(|(profile_id, mut takes)| {
            takes.sort_by_key(|t| t.created_at);
            let mut run: Vec<&WakeTake> = Vec::new();
            for take in takes {
                let gap = run
                    .last()
                    .is_some_and(|last| take.created_at - last.created_at > RUN_GAP);
                if run.len() >= TAKES_PER_RUN || gap {
                    run.clear();
                }
                run.push(take);
            }
            TakeRun {
                profile_id: profile_id.map(str::to_string),
                takes: run.len(),
                heard: run.iter().filter(|t| t.cnn_heard).count(),
            }
        })
        .collect()
}

/// The household's wake calibration across every taught member's latest run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeVerdict {
    pub heard: usize,
    pub of: usize,
    pub cnn_hears_household: bool,
    /// Members whose latest run the CNN missed too often; `None` is a take nobody was named for.
    pub missed: Vec<Option<String>>,
}

pub fn wake_verdict(takes: &[WakeTake]) -> WakeVerdict {
    let runs = latest_runs(takes);
    let missed: Vec<Option<String>> = runs
        .iter()
        .filter(|r| r.heard < CNN_TAKES_NEEDED)
        .map(|r| r.profile_id.clone())
        .collect();
    WakeVerdict {
        heard: runs.iter().map(|r| r.heard).sum(),
        of: runs.iter().map(|r| r.takes).sum(),
        cnn_hears_household: !runs.is_empty() && missed.is_empty(),
        missed,
    }
}

/// How alike a member's prints are, 0 to 1: their mean pairwise cosine; `None` under two.
pub fn consistency(embeddings: &[Vec<f32>]) -> Option<f32> {
    if embeddings.len() < 2 {
        return None;
    }
    let unit: Vec<Vec<f32>> = embeddings.iter().map(|e| normalized(e)).collect();
    let mut total = 0.0;
    let mut pairs = 0usize;
    for (i, a) in unit.iter().enumerate() {
        for b in &unit[i + 1..] {
            total += a.iter().zip(b).map(|(x, y)| x * y).sum::<f32>();
            pairs += 1;
        }
    }
    Some((total / pairs as f32).clamp(0.0, 1.0))
}

fn normalized(e: &[f32]) -> Vec<f32> {
    let norm = e.iter().map(|v| v * v).sum::<f32>().sqrt();
    if norm == 0.0 {
        return e.to_vec();
    }
    e.iter().map(|v| v / norm).collect()
}

/// One member's Voice Match, as `GET /voice/prints` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct MemberPrints {
    pub profile_id: String,
    pub takes: usize,
    pub enrolled: bool,
    pub consistency: Option<f32>,
    pub updated_at: Option<DateTime<Utc>>,
    pub embeddings: Vec<Vec<f32>>,
}

/// Prints grouped by member; a member is enrolled from [`TAKES_PER_RUN`] prints.
pub fn members(prints: &[VoicePrint]) -> Vec<MemberPrints> {
    let mut by_member: BTreeMap<&str, Vec<&VoicePrint>> = BTreeMap::new();
    for print in prints {
        by_member.entry(&print.profile_id).or_default().push(print);
    }
    by_member
        .into_iter()
        .map(|(profile_id, prints)| {
            let embeddings: Vec<Vec<f32>> = prints.iter().map(|p| p.embedding.clone()).collect();
            MemberPrints {
                profile_id: profile_id.to_string(),
                takes: prints.len(),
                enrolled: prints.len() >= TAKES_PER_RUN,
                consistency: consistency(&embeddings),
                updated_at: prints.iter().map(|p| p.created_at).max(),
                embeddings,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn take(who: Option<&str>, minute: i64, heard: bool) -> WakeTake {
        WakeTake {
            id: format!("{who:?}-{minute}"),
            profile_id: who.map(str::to_string),
            phrase: "hey goose".into(),
            cnn_heard: heard,
            created_at: DateTime::<Utc>::from_timestamp(1_800_000_000, 0).unwrap()
                + Duration::minutes(minute),
        }
    }

    #[test]
    fn the_cnn_takes_over_only_when_it_heard_four_of_every_members_latest_five() {
        let mut takes: Vec<WakeTake> = (0..5).map(|i| take(Some("amina"), i, i != 2)).collect();
        assert!(wake_verdict(&takes).cnn_hears_household);
        takes.extend((10..15).map(|i| take(Some("zawadi"), i, i < 13)));
        let verdict = wake_verdict(&takes);
        assert!(!verdict.cnn_hears_household);
        assert_eq!(verdict.missed, [Some("zawadi".to_string())]);
        assert_eq!((verdict.heard, verdict.of), (7, 10));
        // Zawadi's next run replaces her last one; Amina's stays.
        takes.extend((40..45).map(|i| take(Some("zawadi"), i, true)));
        assert!(wake_verdict(&takes).cnn_hears_household);
        assert!(!wake_verdict(&[]).cnn_hears_household, "nobody taught it");
    }

    #[test]
    fn a_run_ends_at_five_takes_or_a_long_pause() {
        let takes: Vec<WakeTake> = [0, 1, 2, 30, 31]
            .into_iter()
            .map(|m| take(Some("amina"), m, true))
            .collect();
        assert_eq!(
            latest_runs(&takes)[0].takes,
            2,
            "the pause started a new run"
        );
        let seven: Vec<WakeTake> = (0..7).map(|m| take(None, m, true)).collect();
        assert_eq!(latest_runs(&seven)[0].takes, 2);
    }

    #[test]
    fn consistency_is_how_alike_the_prints_are() {
        assert_eq!(consistency(&[vec![1.0, 0.0]]), None);
        assert_eq!(consistency(&[vec![1.0, 0.0], vec![2.0, 0.0]]), Some(1.0));
        assert_eq!(consistency(&[vec![1.0, 0.0], vec![0.0, 1.0]]), Some(0.0));
        assert_eq!(consistency(&[vec![1.0, 0.0], vec![-1.0, 0.0]]), Some(0.0));
    }

    #[test]
    fn a_member_is_enrolled_from_five_prints() {
        let print = |who: &str, i: i64| VoicePrint {
            id: format!("{who}{i}"),
            profile_id: who.into(),
            embedding: vec![1.0, i as f32 * 0.01],
            model_name: "m".into(),
            created_at: DateTime::<Utc>::from_timestamp(1_800_000_000 + i, 0).unwrap(),
        };
        let mut prints: Vec<VoicePrint> = (0..5).map(|i| print("amina", i)).collect();
        prints.push(print("zawadi", 9));
        let members = members(&prints);
        assert_eq!(members.len(), 2);
        assert!(members[0].enrolled && !members[1].enrolled);
        assert_eq!(members[0].takes, 5);
        assert!(members[0].consistency.unwrap() > 0.99);
        assert_eq!(members[1].consistency, None);
    }
}
