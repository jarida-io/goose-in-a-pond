//! Decides whose turn this is: a session's evidence becomes the turn's [`ProfileScope`].
//!
//! Every caller still passes `paired_device_profile: None`; wiring it changes what an
//! unidentified speaker reaches, so `device_profile_rung_is_not_wired_yet` fails when it does.
//! Never infer the owner from `settings.primary_profile_id`: every phone would be one person's.

use crate::user_data::domain::profile::ProfileScope;
use crate::user_data::domain::session::{IdentificationSource, SessionIdentity};

/// Who is speaking, gathered per turn; no `Default`, as every field is an authorisation input.
pub struct ResolutionInputs<'a> {
    /// Owner of the paired device that authenticated this request; always `None` for now.
    pub paired_device_profile: Option<&'a str>,
    /// What the session row says, from [`SessionStorage::get_session_identity`].
    ///
    /// [`SessionStorage::get_session_identity`]: crate::user_data::ports::session_storage::SessionStorage::get_session_identity
    pub session: &'a SessionIdentity,
    /// Whether this pond has more than one household member; decides the fallback in [`resolve`].
    pub household_has_multiple_members: bool,
}

/// The decision, with the evidence that produced it.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedIdentity {
    pub scope: ProfileScope,
    /// What the decision rested on; kept so audits can tell a 0.6 face match from a paired token.
    pub source: IdentificationSource,
}

/// Resolve one turn's scope: paired device, explicit, then face give `Owner`. Otherwise
/// `Household` in a one-member pond (`Guest` would refuse to remember its only user), else `Guest`.
pub fn resolve(inputs: &ResolutionInputs<'_>) -> ResolvedIdentity {
    if let Some(profile) = inputs.paired_device_profile {
        return ResolvedIdentity {
            scope: ProfileScope::Owner(profile.to_string()),
            source: IdentificationSource::PairedDevice,
        };
    }

    // Explicit and Face both live on the row; `supersedes` already made the stronger one win.
    if let Some(profile) = inputs.session.profile_id.as_deref() {
        match inputs.session.source {
            IdentificationSource::PairedDevice
            | IdentificationSource::Explicit
            | IdentificationSource::Face => {
                return ResolvedIdentity {
                    scope: ProfileScope::Owner(profile.to_string()),
                    source: inputs.session.source,
                };
            }
            // An id with no source is not evidence (should be unreachable), so fall through.
            IdentificationSource::Unknown => {}
        }
    }

    ResolvedIdentity {
        scope: if inputs.household_has_multiple_members {
            ProfileScope::Guest
        } else {
            ProfileScope::Household
        },
        source: IdentificationSource::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(source: IdentificationSource, who: Option<&str>) -> SessionIdentity {
        SessionIdentity {
            profile_id: who.map(str::to_string),
            source,
            confidence: None,
        }
    }

    fn inputs<'a>(
        paired: Option<&'a str>,
        session: &'a SessionIdentity,
        multi: bool,
    ) -> ResolutionInputs<'a> {
        ResolutionInputs {
            paired_device_profile: paired,
            session,
            household_has_multiple_members: multi,
        }
    }

    #[test]
    fn a_paired_device_outranks_whatever_the_session_row_says() {
        let s = session(IdentificationSource::Face, Some("liz"));
        let resolved = resolve(&inputs(Some("jerry"), &s, true));
        assert_eq!(resolved.scope, ProfileScope::Owner("jerry".into()));
        assert_eq!(resolved.source, IdentificationSource::PairedDevice);
    }

    #[test]
    fn an_explicit_or_face_binding_resolves_to_its_owner_and_keeps_its_source() {
        for source in [IdentificationSource::Explicit, IdentificationSource::Face] {
            let s = session(source, Some("jerry"));
            let resolved = resolve(&inputs(None, &s, true));
            assert_eq!(resolved.scope, ProfileScope::Owner("jerry".into()));
            assert_eq!(
                resolved.source, source,
                "the source must survive resolution -- invariant 3"
            );
        }
    }

    #[test]
    fn an_unidentified_speaker_in_a_one_member_pond_still_sees_the_household() {
        let s = SessionIdentity::unknown();
        let resolved = resolve(&inputs(None, &s, false));
        assert_eq!(resolved.scope, ProfileScope::Household);
        assert_eq!(resolved.source, IdentificationSource::Unknown);
    }

    #[test]
    fn an_unidentified_speaker_in_a_shared_pond_is_a_guest() {
        let s = SessionIdentity::unknown();
        let resolved = resolve(&inputs(None, &s, true));
        assert_eq!(resolved.scope, ProfileScope::Guest);
        assert!(resolved.scope.excludes_everything());
    }

    #[test]
    fn a_profile_id_without_a_source_is_not_trusted() {
        let s = session(IdentificationSource::Unknown, Some("jerry"));
        let resolved = resolve(&inputs(None, &s, true));
        assert_eq!(resolved.scope, ProfileScope::Guest);
        assert_ne!(resolved.scope, ProfileScope::Owner("jerry".into()));
    }
}
