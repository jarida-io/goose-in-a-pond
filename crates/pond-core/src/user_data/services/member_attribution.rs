//! Attributing a thing to a household member when nobody said which one.
//!
//! Only a sole member is a default: pairing is deliberate, and with two, picking one is mere
//! creation order. Deliberately unlike [`identity_resolution`]: anonymous turns get no identity.
//!
//! [`identity_resolution`]: super::identity_resolution

/// The member a new pairing code binds to: `explicit` always wins, then
/// `unattributed_requested` (how a guest's phone is paired), then the sole member.
pub fn owner_for_new_code(
    explicit: Option<&str>,
    unattributed_requested: bool,
    member_ids: &[String],
) -> Option<String> {
    if let Some(named) = explicit {
        return Some(named.to_string());
    }
    if unattributed_requested {
        return None;
    }
    sole_member(member_ids)
}

/// The member the device that claimed the pond becomes once onboarding completes: the primary
/// member, and only while the device belongs to nobody, so an attribution made since is kept.
pub fn owner_for_claimant(
    claimant_owner: Option<&str>,
    primary_profile_id: Option<&str>,
    member_ids: &[String],
) -> Option<String> {
    if claimant_owner.is_some() {
        return None;
    }
    let primary = primary_profile_id.filter(|id| !id.trim().is_empty())?;
    member_ids
        .iter()
        .any(|member| member == primary)
        .then(|| primary.to_string())
}

/// The one member, when there is exactly one.
pub fn sole_member(member_ids: &[String]) -> Option<String> {
    match member_ids {
        [only] => Some(only.clone()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn a_named_member_always_wins() {
        assert_eq!(
            owner_for_new_code(Some("liz"), false, &ids(&["jerry", "liz"])),
            Some("liz".into())
        );
    }

    #[test]
    fn a_named_member_outranks_an_unattributed_request() {
        assert_eq!(
            owner_for_new_code(Some("liz"), true, &ids(&["jerry", "liz"])),
            Some("liz".into())
        );
    }

    #[test]
    fn a_sole_member_gets_the_device() {
        assert_eq!(
            owner_for_new_code(None, false, &ids(&["jerry"])),
            Some("jerry".into())
        );
    }

    #[test]
    fn a_sole_member_household_can_still_ask_for_nobody() {
        assert_eq!(owner_for_new_code(None, true, &ids(&["jerry"])), None);
    }

    #[test]
    fn two_members_is_not_a_default() {
        assert_eq!(
            owner_for_new_code(None, false, &ids(&["jerry", "liz"])),
            None
        );
    }

    #[test]
    fn an_empty_household_pairs_unattributed() {
        assert_eq!(owner_for_new_code(None, false, &[]), None);
    }

    #[test]
    fn an_unattributed_claimant_becomes_the_primary_members() {
        assert_eq!(
            owner_for_claimant(None, Some("jerry"), &ids(&["jerry", "liz"])),
            Some("jerry".into())
        );
    }

    #[test]
    fn a_claimant_attributed_since_is_never_rebound() {
        assert_eq!(
            owner_for_claimant(Some("liz"), Some("jerry"), &ids(&["jerry", "liz"])),
            None
        );
    }

    #[test]
    fn no_primary_member_binds_nobody() {
        for primary in [None, Some(""), Some("  ")] {
            assert_eq!(
                owner_for_claimant(None, primary, &ids(&["jerry"])),
                None,
                "{primary:?}"
            );
        }
    }

    #[test]
    fn a_primary_id_that_names_no_member_binds_nobody() {
        assert_eq!(
            owner_for_claimant(None, Some("deleted"), &ids(&["jerry"])),
            None
        );
        assert_eq!(owner_for_claimant(None, Some("jerry"), &[]), None);
    }
}
