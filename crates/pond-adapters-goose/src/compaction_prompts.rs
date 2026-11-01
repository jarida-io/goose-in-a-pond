//! The compaction prompt and summary goose uses on the budgeted device.
//!
//! goose summarises a conversation near the edge of its window with `compaction.md` and renders
//! the summary it gets back with `compaction_summary.md`, preferring a copy of either in its config
//! directory's `prompts/` over its own, read afresh at every compaction. Its own prompt is written
//! for coding sessions and asks for every user message and the whole length budget. On the Orin's
//! 8k LiteRT-LM window gemma-4-E4B carried every earlier request forward, the summary grew with
//! each compaction, and from the sixth turn goose compacted on every turn, four to five minutes
//! each. The bounded pair asks for four short fields and renders only the eight most recent
//! requests, so a compaction leaves about half the window in use however long the conversation
//! has run. The cost is detail: a dish named earlier in a conversation does not survive it.
//!
//! Every file this module writes starts with [`MARK`]. Only a file that does is ever replaced or
//! removed, so a prompt a person wrote there is left as it is.

use std::io;
use std::path::Path;

/// The first characters of every file the pond writes here, and the test for ownership.
const MARK: &str = "{# giap: bounded compaction";
const PROMPT: (&str, &str) = (
    "compaction.md",
    include_str!("prompts/compaction_bounded.md"),
);
const SUMMARY: (&str, &str) = (
    "compaction_summary.md",
    include_str!("prompts/compaction_summary_bounded.md"),
);
/// `bounded` or `builtin` overrides the device's choice, for measuring either on either device.
const OVERRIDE_ENV: &str = "GIAP_COMPACTION_PROMPT";

/// Whether the bounded pair applies: on the budgeted device, unless the override says otherwise.
/// An override that is neither word is ignored, so a typo never changes the device's choice.
pub fn bounded(budgeted_device: bool, override_value: Option<&str>) -> bool {
    match override_value.map(|value| value.trim().to_ascii_lowercase()) {
        Some(value) if value == "bounded" => true,
        Some(value) if value == "builtin" => false,
        _ => budgeted_device,
    }
}

/// What happened to one template file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Applied {
    /// The pond's copy was written where there was none.
    Installed,
    /// An older pond copy was replaced.
    Updated,
    /// The pond's copy was already current.
    Current,
    /// The pond's copy was removed: this device uses goose's own.
    Removed,
    /// Nothing there, and nothing wanted.
    Absent,
    /// A file someone else wrote is there; left as it is.
    NotOurs,
}

/// Installs the bounded pair in `prompts_dir`, or removes the pond's copies, file by file.
pub fn apply(prompts_dir: &Path, bounded: bool) -> io::Result<[(&'static str, Applied); 2]> {
    Ok([
        (PROMPT.0, apply_one(prompts_dir, PROMPT, bounded)?),
        (SUMMARY.0, apply_one(prompts_dir, SUMMARY, bounded)?),
    ])
}

fn apply_one(
    prompts_dir: &Path,
    (name, content): (&str, &str),
    bounded: bool,
) -> io::Result<Applied> {
    let path = prompts_dir.join(name);
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => return Err(error),
    };
    match (existing, bounded) {
        (Some(text), _) if !text.starts_with(MARK) => Ok(Applied::NotOurs),
        (Some(text), true) if text == content => Ok(Applied::Current),
        (existing, true) => {
            std::fs::create_dir_all(prompts_dir)?;
            // Through a rename, so goose never reads half a file and two processes starting at
            // once (the server and its voice child) cannot interleave their writes.
            let partial = prompts_dir.join(format!(".{name}.{}.tmp", std::process::id()));
            std::fs::write(&partial, content)?;
            std::fs::rename(&partial, &path)?;
            Ok(if existing.is_some() {
                Applied::Updated
            } else {
                Applied::Installed
            })
        }
        (Some(_), false) => {
            std::fs::remove_file(&path)?;
            Ok(Applied::Removed)
        }
        (None, false) => Ok(Applied::Absent),
    }
}

/// At startup, before any turn can compact: goose's own prompts directory, the device's choice and
/// `GIAP_COMPACTION_PROMPT`. Needs `GOOSE_PATH_ROOT` set first, or it writes to goose's app dir.
pub fn apply_for_this_device() {
    let budgeted = pond_core::models::domain::device_budget::budgeted_device();
    let override_value = std::env::var(OVERRIDE_ENV).ok();
    let bounded = bounded(budgeted, override_value.as_deref());
    let prompts_dir = goose::config::paths::Paths::config_dir().join("prompts");
    match apply(&prompts_dir, bounded) {
        Ok(applied) => tracing::info!(
            target: "giap::trace",
            kind = "compaction_prompts",
            bounded,
            budgeted_device = budgeted,
            override_value = override_value.as_deref().unwrap_or(""),
            prompt_file = ?applied[0].1,
            summary_file = ?applied[1].1,
            dir = %prompts_dir.display(),
            "compaction prompts for this device"
        ),
        Err(error) => tracing::warn!(
            %error,
            dir = %prompts_dir.display(),
            "could not set the compaction prompts for this device; goose uses its own"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use goose::context_mgmt::structured::StructuredSummary;
    use goose::prompt_template::render_string;

    #[test]
    fn the_budgeted_device_takes_the_bounded_pair_unless_told_otherwise() {
        assert!(bounded(true, None));
        assert!(!bounded(false, None));
        assert!(!bounded(true, Some("builtin")));
        assert!(bounded(false, Some(" Bounded ")));
        assert!(
            bounded(true, Some("bnd")),
            "a typo keeps the device's choice"
        );
        assert!(!bounded(false, Some("")));
    }

    #[test]
    fn installs_both_then_finds_them_current() {
        let dir = tempfile::tempdir().unwrap();
        let prompts = dir.path().join("prompts");
        let first = apply(&prompts, true).unwrap();
        assert_eq!(first.map(|(_, applied)| applied), [Applied::Installed; 2]);
        assert_eq!(
            std::fs::read_to_string(prompts.join("compaction.md")).unwrap(),
            PROMPT.1
        );
        let again = apply(&prompts, true).unwrap();
        assert_eq!(again.map(|(_, applied)| applied), [Applied::Current; 2]);
        assert_eq!(
            std::fs::read_dir(&prompts).unwrap().count(),
            2,
            "no partial file is left behind"
        );
    }

    #[test]
    fn replaces_an_older_copy_of_its_own_and_removes_its_own_off_the_device() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("compaction.md"),
            format!("{MARK} an older version #}}\nold prompt"),
        )
        .unwrap();
        let updated = apply(dir.path(), true).unwrap();
        assert_eq!(updated[0].1, Applied::Updated);
        assert_eq!(updated[1].1, Applied::Installed);

        let removed = apply(dir.path(), false).unwrap();
        assert_eq!(removed.map(|(_, applied)| applied), [Applied::Removed; 2]);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        let again = apply(dir.path(), false).unwrap();
        assert_eq!(again.map(|(_, applied)| applied), [Applied::Absent; 2]);
    }

    #[test]
    fn a_prompt_someone_else_wrote_is_never_touched() {
        let dir = tempfile::tempdir().unwrap();
        let theirs = "Summarise this for me:\n{{ messages }}\n";
        std::fs::write(dir.path().join("compaction.md"), theirs).unwrap();
        for bounded in [true, false] {
            let applied = apply(dir.path(), bounded).unwrap();
            assert_eq!(applied[0].1, Applied::NotOurs);
            assert_eq!(
                std::fs::read_to_string(dir.path().join("compaction.md")).unwrap(),
                theirs
            );
        }
    }

    #[test]
    fn the_prompt_renders_as_it_was_measured() {
        let rendered = render_string(
            PROMPT.1,
            &serde_json::json!({"messages": "user: hi\nassistant: hello"}),
        )
        .unwrap();
        assert!(rendered.starts_with("## Task Context"), "{rendered}");
        assert!(rendered.contains("user: hi\nassistant: hello"));
        assert!(
            !rendered.contains("giap"),
            "the mark stays out of the prompt"
        );
    }

    #[test]
    fn the_summary_keeps_four_fields_and_the_eight_most_recent_requests() {
        let intents: Vec<String> = (1..=14).map(|n| format!("request {n}")).collect();
        let response = format!(
            "```json\n{}\n```",
            serde_json::json!({
                "user_intent": intents,
                "technical_concepts": ["not asked for"],
                "files": [{"path": "a.rs", "summary": "not asked for"}],
                "pending_tasks": ["answer request 14"],
                "current_work": "request 14",
                "next_step": "wait for the next request",
            })
        );
        let summary = StructuredSummary::parse(&response).expect("goose reads the summary");
        let rendered = render_string(SUMMARY.1, &summary).unwrap();
        assert!(rendered.starts_with("# Conversation Summary"), "{rendered}");
        for n in 1..=6 {
            assert!(
                !rendered.contains(&format!("- request {n}\n")),
                "{rendered}"
            );
        }
        for n in 7..=14 {
            assert!(rendered.contains(&format!("- request {n}")), "{rendered}");
        }
        for heading in ["## Pending Tasks", "## Current Work", "## Next Step"] {
            assert!(rendered.contains(heading), "{rendered}");
        }
        for left_out in ["## Technical Concepts", "## Files", "not asked for", "giap"] {
            assert!(!rendered.contains(left_out), "{rendered}");
        }
    }
}
