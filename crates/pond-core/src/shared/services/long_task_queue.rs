//! The line open long tasks wait in. One slot runs them, so the runner, the chat card, the phone
//! and the prime all read the same order from here, and none of them can disagree about it.

use crate::shared::domain::long_task::{LongTask, LongTaskState};

const GOAL_CHARS: usize = 80;

/// Open tasks in the order the runner takes them: the order they were asked for. The runner
/// finishes the task at the front before it starts the next, so the first answer comes as soon as
/// one slot can give it, and a member can see where a task stands and stop what they no longer need.
pub fn run_order(tasks: &[LongTask]) -> Vec<&LongTask> {
    let mut open: Vec<&LongTask> = tasks
        .iter()
        .filter(|t| matches!(t.state(), LongTaskState::Planning | LongTaskState::Running))
        .collect();
    open.sort_by(|a, b| {
        a.created_at()
            .cmp(&b.created_at())
            .then_with(|| a.id().cmp(b.id()))
    });
    open
}

/// Whether `viewer` may see a task: their own, or the household's. `None` is a caller who is no
/// member, who reaches only the household's.
pub fn reaches(viewer: Option<&str>, task: &LongTask) -> bool {
    match task.owner_profile_id() {
        None => true,
        Some(owner) => viewer == Some(owner),
    }
}

/// A task ahead in the line, as one viewer may know it: another member's goal is theirs alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ahead<'a> {
    Reachable(&'a LongTask),
    AnotherMember,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueuePlace<'a> {
    /// 1 is the task being worked on.
    pub position: usize,
    pub of: usize,
    pub ahead: Vec<Ahead<'a>>,
}

impl QueuePlace<'_> {
    pub fn is_waiting(&self) -> bool {
        self.position > 1
    }
}

/// Where `task_id` stands in `order`, as `viewer` may see it; `None` for a task not in the line.
pub fn place_of<'a>(
    order: &[&'a LongTask],
    task_id: &str,
    viewer: Option<&str>,
) -> Option<QueuePlace<'a>> {
    let index = order.iter().position(|t| t.id() == task_id)?;
    let ahead = order[..index]
        .iter()
        .map(|t| {
            if reaches(viewer, t) {
                Ahead::Reachable(t)
            } else {
                Ahead::AnotherMember
            }
        })
        .collect();
    Some(QueuePlace {
        position: index + 1,
        of: order.len(),
        ahead,
    })
}

/// What the prime tells the person when a task it just started waits behind others; `None` at the
/// front of the line, where there is nothing to say.
pub fn waiting_line(place: &QueuePlace<'_>) -> Option<String> {
    if !place.is_waiting() {
        return None;
    }
    let theirs: Vec<String> = place
        .ahead
        .iter()
        .filter_map(|a| match a {
            Ahead::Reachable(task) => Some(format!("\"{}\"", clip(task.goal()))),
            Ahead::AnotherMember => None,
        })
        .collect();
    let others = place.ahead.len() - theirs.len();
    let mut ahead = Vec::new();
    if !theirs.is_empty() {
        ahead.push(theirs.join(", "));
    }
    match others {
        0 => {}
        1 => ahead.push("one task for another member".to_string()),
        n => ahead.push(format!("{n} tasks for other members")),
    }
    let mut line = format!(
        "It is number {} in line, so it starts once {} {}.",
        place.position,
        ahead.join(" and "),
        if place.ahead.len() == 1 {
            "finishes"
        } else {
            "finish"
        }
    );
    if !theirs.is_empty() {
        line.push_str(
            " Tell the person it waits its turn, and that the task's card in the chat lets them \
             stop any of their tasks they no longer need.",
        );
    } else {
        line.push_str(" Tell the person it waits its turn.");
    }
    Some(line)
}

fn clip(goal: &str) -> String {
    let goal = goal.split_whitespace().collect::<Vec<_>>().join(" ");
    if goal.chars().count() <= GOAL_CHARS {
        return goal;
    }
    let cut: String = goal.chars().take(GOAL_CHARS).collect();
    format!("{}...", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::domain::long_task::LongTaskBudget;
    use crate::user_data::domain::profile::ProfileScope;
    use chrono::{DateTime, Utc};

    fn at(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_790_000_000 + secs, 0).unwrap()
    }

    fn task(owner: Option<&str>, goal: &str, asked_at: i64) -> LongTask {
        let scope = match owner {
            Some(id) => ProfileScope::Owner(id.to_string()),
            None => ProfileScope::Household,
        };
        LongTask::start(
            scope,
            None,
            goal,
            None,
            LongTaskBudget::new(12, None).unwrap(),
            at(asked_at),
        )
        .unwrap()
    }

    #[test]
    fn the_line_is_the_order_tasks_were_asked_for_and_only_open_ones_wait() {
        let first = task(Some("liz"), "plan the trip", 0);
        let mut done = task(Some("liz"), "compare heat pumps", 5);
        done.transition_to(LongTaskState::Cancelled, at(6)).unwrap();
        let mut second = task(Some("sam"), "find a plumber", 10);
        // A state change later does not move a task back in the line.
        second
            .transition_to(LongTaskState::Running, at(30))
            .unwrap();
        let third = task(Some("liz"), "a packing list", 20);

        let all = [third.clone(), done, second.clone(), first.clone()];
        let order: Vec<&str> = run_order(&all).iter().map(|t| t.id()).collect();
        assert_eq!(order, vec![first.id(), second.id(), third.id()]);
    }

    #[test]
    fn a_member_sees_their_own_goals_ahead_and_only_that_another_member_has_one() {
        let first = task(Some("liz"), "plan the trip", 0);
        let second = task(Some("sam"), "find a plumber", 10);
        let household = task(None, "the shopping list", 15);
        let mine = task(Some("liz"), "a packing list", 20);
        let all = [first.clone(), second, household.clone(), mine.clone()];
        let order = run_order(&all);

        let place = place_of(&order, mine.id(), Some("liz")).unwrap();
        assert_eq!((place.position, place.of), (4, 4));
        assert_eq!(
            place.ahead,
            vec![
                Ahead::Reachable(&first),
                Ahead::AnotherMember,
                Ahead::Reachable(&household)
            ]
        );
        let front = place_of(&order, first.id(), Some("liz")).unwrap();
        assert!(!front.is_waiting());
        assert!(front.ahead.is_empty());
        assert_eq!(place_of(&order, "not-in-line", Some("liz")), None);

        let nobody = place_of(&order, household.id(), None).unwrap();
        assert_eq!(
            nobody.ahead,
            vec![Ahead::AnotherMember, Ahead::AnotherMember]
        );
    }

    #[test]
    fn the_prime_names_the_members_own_tasks_ahead_and_never_another_members() {
        let first = task(Some("liz"), "plan the trip", 0);
        let second = task(Some("sam"), "find a plumber who works on Sundays", 10);
        let mine = task(Some("liz"), "a packing list", 20);
        let all = [first.clone(), second, mine.clone()];
        let order = run_order(&all);

        let line = waiting_line(&place_of(&order, mine.id(), Some("liz")).unwrap()).unwrap();
        assert!(line.starts_with("It is number 3 in line"), "{line}");
        assert!(
            line.contains("\"plan the trip\" and one task for another member finish"),
            "{line}"
        );
        assert!(line.contains("stop any of their tasks"), "{line}");
        assert!(
            !line.contains("plumber"),
            "another member's goal is theirs: {line}"
        );

        assert_eq!(
            waiting_line(&place_of(&order, first.id(), Some("liz")).unwrap()),
            None
        );

        let behind_others =
            waiting_line(&place_of(&order, mine.id(), Some("ola")).unwrap()).unwrap();
        assert!(
            behind_others.contains("2 tasks for other members finish"),
            "{behind_others}"
        );
        assert!(!behind_others.contains("stop any"), "{behind_others}");
    }

    #[test]
    fn a_long_goal_is_clipped_in_the_line() {
        let first = task(Some("liz"), &"word ".repeat(40), 0);
        let mine = task(Some("liz"), "a packing list", 20);
        let all = [first, mine.clone()];
        let order = run_order(&all);
        let line = waiting_line(&place_of(&order, mine.id(), Some("liz")).unwrap()).unwrap();
        assert!(line.contains("...\" finishes"), "{line}");
        assert!(line.len() < 400, "{line}");
    }
}
