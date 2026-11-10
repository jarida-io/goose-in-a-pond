//! How long each kind of item is kept and what deleting its source does to it, declared once
//! here (context-space.md section 4.2) and enforced by one sweep.

use super::{Audience, Kind};

/// How long an item of a kind lives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kept {
    /// As long as the conversation it belongs to.
    WithConversation,
    /// Until a newer item supersedes it. Meanwhile an unused fact decays to `archived`.
    UntilSuperseded,
    /// As long as its source's own retention setting says.
    PerSourceSetting,
    /// This many days after it happened, unless another item cites it.
    DaysUnlessCited(u32),
    /// Until `valid_until` plus this many days.
    PastValidUntil(u32),
    /// Until a member deletes it.
    UntilDeleted,
    /// Until a member decides it, or `valid_until` passes.
    UntilDecided,
}

/// What deleting an item's source does to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnSourceDeleted {
    /// It is a source, or part of one.
    NotApplicable,
    /// Deleted once the last item that is `evidence_for` it is gone.
    DeletedWithLastEvidence,
    /// Deleted once every item it is `derived_from` is gone.
    DeletedWithEverySource,
    /// Dismissed rather than deleted: the member may still want to see what was offered.
    Withdrawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Lifecycle {
    /// The audience a writer gets when it does not choose one.
    pub default_audience: Audience,
    pub kept: Kept,
    pub on_source_deleted: OnSourceDeleted,
}

/// Days a home event outlives itself unless cited (section 4.2).
pub const HOME_EVENT_DAYS: u32 = 7;
/// Days an event stays past its `valid_until` (section 4.2).
pub const EVENT_GRACE_DAYS: u32 = 30;

impl Kind {
    pub fn lifecycle(self) -> Lifecycle {
        use Audience::{Household, Private};
        let (default_audience, kept, on_source_deleted) = match self {
            Kind::Message | Kind::Conversation => (
                Private,
                Kept::WithConversation,
                OnSourceDeleted::NotApplicable,
            ),
            Kind::Fact | Kind::Entity | Kind::Trait => (
                Private,
                Kept::UntilSuperseded,
                OnSourceDeleted::DeletedWithLastEvidence,
            ),
            Kind::Media | Kind::Notification | Kind::Mail | Kind::Visit => (
                Private,
                Kept::PerSourceSetting,
                OnSourceDeleted::NotApplicable,
            ),
            Kind::HomeEvent => (
                Household,
                Kept::DaysUnlessCited(HOME_EVENT_DAYS),
                OnSourceDeleted::NotApplicable,
            ),
            Kind::Event => (
                Private,
                Kept::PastValidUntil(EVENT_GRACE_DAYS),
                OnSourceDeleted::DeletedWithEverySource,
            ),
            Kind::Report => (
                Private,
                Kept::UntilDeleted,
                OnSourceDeleted::DeletedWithEverySource,
            ),
            Kind::Suggestion => (Private, Kept::UntilDecided, OnSourceDeleted::Withdrawn),
        };
        Lifecycle {
            default_audience,
            kept,
            on_source_deleted,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_home_events_default_to_the_household() {
        for kind in Kind::ALL {
            let expected = if kind == Kind::HomeEvent {
                Audience::Household
            } else {
                Audience::Private
            };
            assert_eq!(kind.lifecycle().default_audience, expected, "{kind}");
        }
    }

    #[test]
    fn learned_items_go_with_their_evidence_and_sources_are_never_cascaded() {
        for kind in [Kind::Fact, Kind::Entity, Kind::Trait] {
            assert_eq!(
                kind.lifecycle().on_source_deleted,
                OnSourceDeleted::DeletedWithLastEvidence
            );
        }
        for kind in [Kind::Mail, Kind::Media, Kind::Notification, Kind::Visit] {
            assert_eq!(
                kind.lifecycle().on_source_deleted,
                OnSourceDeleted::NotApplicable
            );
            assert_eq!(kind.lifecycle().kept, Kept::PerSourceSetting);
        }
    }
}
