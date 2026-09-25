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
}
