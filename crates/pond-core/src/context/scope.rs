//! Which context rows a [`ProfileScope`] may see; `sqlite_context.rs` mirrors it in SQL.
//! Its test cross-checks the two. No `OR profile_id IS NULL` limb: `profile_id` is `NOT NULL`.

use crate::context::domain::{ContextItem, ContextSource};
use crate::user_data::domain::profile::ProfileScope;

/// Whether `owner` is visible to `scope`.
pub fn owner_is_visible(owner: &str, scope: &ProfileScope) -> bool {
    match scope {
        // A Guest session sees no context items; this arm must never grow an exception.
        ProfileScope::Guest => false,
        ProfileScope::Household => true,
        ProfileScope::Owner(id) => id == owner,
    }
}

pub fn item_is_visible(item: &ContextItem, scope: &ProfileScope) -> bool {
    owner_is_visible(item.profile_id(), scope)
}

pub fn source_is_visible(source: &ContextSource, scope: &ProfileScope) -> bool {
    owner_is_visible(source.profile_id(), scope)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user_data::domain::profile::{EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID};

    #[test]
    fn a_guest_sees_no_row_whoever_owns_it() {
        for owner in [EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID, "", "anyone"] {
            assert!(
                !owner_is_visible(owner, &ProfileScope::Guest),
                "a Guest could see a row owned by {owner:?}"
            );
        }
    }

    #[test]
    fn an_owner_sees_only_their_own() {
        let jerry = ProfileScope::Owner(EXEMPLAR_OWNER_ID.to_string());
        assert!(owner_is_visible(EXEMPLAR_OWNER_ID, &jerry));
        assert!(!owner_is_visible(SECOND_EXEMPLAR_OWNER_ID, &jerry));
        assert_ne!(
            EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID,
            "the fixture's two members are the same person, so the line above compares a scope \
             with itself"
        );
    }

    #[test]
    fn the_household_sees_everything() {
        for owner in [EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID] {
            assert!(owner_is_visible(owner, &ProfileScope::Household));
        }
    }

    /// Vacuity control: the predicate is not a constant in either direction.
    #[test]
    fn the_predicate_refuses_a_specific_number_of_pairs() {
        let owners = [EXEMPLAR_OWNER_ID, SECOND_EXEMPLAR_OWNER_ID];
        let mut scopes = ProfileScope::every_shape();
        scopes.push(ProfileScope::Owner(SECOND_EXEMPLAR_OWNER_ID.to_string()));

        let visible = scopes
            .iter()
            .flat_map(|s| owners.iter().map(move |o| (s, *o)))
            .filter(|(s, o)| owner_is_visible(o, s))
            .count();
        // Household sees both owners (2), each Owner itself (2), Guest neither: 4 of 8 pairs.
        assert_eq!(
            visible, 4,
            "the context visibility rule changed shape; if that was deliberate, say which pair \
             moved and why it is not a widening"
        );
    }
}
