//! Deep research a member asks for from the chat: the research task it starts, the `task_start`
//! call the turn's stream opens with, and what the prime is told about it.
use crate::mcp::domain::tool_group::TASKS_EXTENSION;
use crate::shared::domain::agent::{AgentStreamEvent, DeepResearchTurn};
use crate::shared::domain::long_task::{
    AnswerShape, LongTask, LongTaskBudget, LongTaskError, LongTaskKind,
};
use crate::user_data::domain::profile::ProfileScope;
use chrono::{DateTime, Utc};

/// Longest research question kept as a goal; it reaches every step's brief.
pub const GOAL_CHARS: usize = 2_000;

/// Gathering, a step per section and the summary need more room than the usual twelve steps.
pub const RESEARCH_STEPS: u32 = 24;

/// A member's deep research: a research task they own, reported back to `session_id`.
pub fn research_task(
    member: &str,
    question: &str,
    session_id: &str,
    now: DateTime<Utc>,
) -> Result<LongTask, LongTaskError> {
    let goal: String = question.trim().chars().take(GOAL_CHARS).collect();
    Ok(LongTask::start(
        ProfileScope::Owner(member.to_string()),
        None,
        &goal,
        Some(session_id.to_string()),
        LongTaskBudget::new(RESEARCH_STEPS, None)?,
        now,
    )?
    .with_kind(LongTaskKind::Research))
}

/// The `task_start` call and result a deep research turn opens with, spelled as the prime's own
/// call reaches the stream, so no client tells the two apart.
pub fn opening_events(question: &str, task_id: &str) -> [AgentStreamEvent; 2] {
    let id = format!("deep-research-{task_id}");
    let tool = format!("{TASKS_EXTENSION}__task_start");
    [
        AgentStreamEvent::ToolCall {
            id: id.clone(),
            tool: tool.clone(),
            input: Some(serde_json::json!({ "goal": question.trim() })),
        },
        AgentStreamEvent::ToolResult {
            id,
            tool,
            content: format!(
                "Started task {task_id}. The pond researches it while the household is quiet and \
                 tells the person on their phone when the report is ready."
            ),
            card: None,
            keep: None,
        },
    ]
}

/// What the prime is told on a turn whose question the pond already handed to deep research, so
/// its reply is a short acknowledgement and never a second, unresearched answer.
pub fn turn_note(turn: &DeepResearchTurn) -> String {
    let waiting = turn
        .waiting
        .as_deref()
        .map(|line| format!(" {line}"))
        .unwrap_or_default();
    let work = match turn.answer {
        AnswerShape::Short => "find the answer and put it here, in a few sentences",
        AnswerShape::Report => "search, compare sources and write a report",
        AnswerShape::Auto => "look into it, with a short answer or a report as the question needs",
    };
    // No tool: the task has only just started, and a look at it costs the turn a round trip.
    format!(
        "<deep-research>\nThe person asked for deep research on their message, and the pond has \
         started task {} on it: helpers will {work}, and the person is told on their phone when \
         it is done.{waiting} The helpers read any page the message links, so do not read it \
         yourself. Reply in one or two sentences saying what will be looked into. Do not answer \
         the question yourself, and do not call any tool.\n</deep-research>",
        turn.task_id
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    #[test]
    fn a_research_task_belongs_to_its_member_and_reports_to_the_conversation() {
        let task = research_task("liz", "  compare heat pumps  ", "s-1", t(0)).unwrap();
        assert_eq!(task.kind(), LongTaskKind::Research);
        assert_eq!(task.owner_profile_id(), Some("liz"));
        assert_eq!(task.goal(), "compare heat pumps");
        assert_eq!(task.origin_session_id(), Some("s-1"));
        assert_eq!(task.budget().max_steps(), RESEARCH_STEPS);

        let long = "q".repeat(GOAL_CHARS + 50);
        assert_eq!(
            research_task("liz", &long, "s-1", t(0))
                .unwrap()
                .goal()
                .chars()
                .count(),
            GOAL_CHARS
        );
        assert!(research_task("liz", "   ", "s-1", t(0)).is_err());
    }

    #[test]
    fn the_stream_opens_with_a_task_start_call_a_client_cannot_tell_from_the_primes() {
        let [call, result] = opening_events("compare heat pumps", "task-7");
        let AgentStreamEvent::ToolCall { id, tool, input } = call else {
            panic!("the call comes first");
        };
        assert_eq!(tool, "giap-tasks__task_start");
        assert_eq!(input.unwrap()["goal"], "compare heat pumps");
        let AgentStreamEvent::ToolResult {
            id: result_id,
            tool: result_tool,
            content,
            ..
        } = result
        else {
            panic!("then its result");
        };
        assert_eq!((result_id, result_tool), (id, tool));
        assert!(
            content.starts_with("Started task task-7."),
            "the clients find the task by this sentence"
        );
    }

    #[test]
    fn the_turn_note_names_its_task_and_keeps_the_prime_from_answering() {
        let note = turn_note(&DeepResearchTurn {
            task_id: "task-7".to_string(),
            waiting: None,
            answer: AnswerShape::Short,
        });
        assert!(
            note.contains("in a few sentences"),
            "a short answer is promised as one"
        );
        assert!(note.contains("task-7"));
        assert!(note.contains("Do not answer the question yourself"));
        assert!(
            note.contains("do not call"),
            "a second task_start would start it twice"
        );
        assert!(
            note.contains("do not read it yourself"),
            "the prime read a linked page twice, a 39 s reply, then the helper read it again"
        );
        assert!(note.starts_with("<deep-research>") && note.ends_with("</deep-research>"));
        assert!(!note.contains("in line"));

        let queued = turn_note(&DeepResearchTurn {
            task_id: "task-7".to_string(),
            waiting: Some("It is number 2 in line.".to_string()),
            answer: AnswerShape::Auto,
        });
        assert!(
            queued.contains("done. It is number 2 in line. The helpers read"),
            "{queued}"
        );
    }
}
