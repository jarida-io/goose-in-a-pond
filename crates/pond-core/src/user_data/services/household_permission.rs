//! Who may remove a device, change the model, enroll remote access, reset the Pond, or control
//! a device — pure decisions over a resolved [`Role`], so a caller can render the refusal
//! (a spoken or HTTP one) without its own copy of the policy.

use crate::user_data::domain::profile::Role;
use chrono::{DateTime, Utc};
use std::collections::HashSet;

/// `Denied` carries enough to answer "why" and "who can fix it" without failing silently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PermissionDecision {
    Allowed,
    Denied {
        reason: &'static str,
        grantable_by: Role,
    },
}

impl PermissionDecision {
    pub fn is_allowed(&self) -> bool {
        matches!(self, PermissionDecision::Allowed)
    }
}

pub const REASON_OWNER_ONLY_REMOVE: &str = "only the owner may remove a paired device";
pub const REASON_OWNER_ONLY_MODEL: &str = "only the owner may change the model";
pub const REASON_OWNER_ONLY_REMOTE_ACCESS: &str =
    "only the owner may enroll this Pond in remote access";
pub const REASON_OWNER_ONLY_RESET: &str = "only the owner may reset this Pond";
pub const REASON_OWNER_ONLY_ROLES: &str =
    "only the owner may change a household member's role or device access";
pub const REASON_GUEST_DEVICE_NOT_ALLOWED: &str =
    "this guest has not been given access to this device";
pub const REASON_GUEST_ACCESS_EXPIRED: &str = "this guest's access to this device has expired";

fn owner_only(role: Option<Role>, reason: &'static str) -> PermissionDecision {
    if role == Some(Role::Owner) {
        PermissionDecision::Allowed
    } else {
        PermissionDecision::Denied {
            reason,
            grantable_by: Role::Owner,
        }
    }
}

pub fn may_remove_device(role: Option<Role>) -> PermissionDecision {
    owner_only(role, REASON_OWNER_ONLY_REMOVE)
}

pub fn may_change_model(role: Option<Role>) -> PermissionDecision {
    owner_only(role, REASON_OWNER_ONLY_MODEL)
}

pub fn may_enroll_remote_access(role: Option<Role>) -> PermissionDecision {
    owner_only(role, REASON_OWNER_ONLY_REMOTE_ACCESS)
}

pub fn may_reset_pond(role: Option<Role>) -> PermissionDecision {
    owner_only(role, REASON_OWNER_ONLY_RESET)
}

/// Granting a role or a guest's device access is itself a grant of trust — only the owner may
/// make it, same as the other owner-only actions above.
pub fn may_manage_household_roles(role: Option<Role>) -> PermissionDecision {
    owner_only(role, REASON_OWNER_ONLY_ROLES)
}

/// Owner, Member and Developer are fully trusted for device control. A Guest is restricted to
/// its own allow-list, and further to before `guest_until` when the guest's access is time-
/// limited. `role: None` (no resolved profile — an anonymous, unidentified voice) is denied the
/// same as an un-allow-listed guest: the least-privileged outcome, never assumed trust.
pub fn may_control_device(
    role: Option<Role>,
    device_id: &str,
    guest_allowed_devices: &HashSet<String>,
    guest_until: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> PermissionDecision {
    match role {
        Some(Role::Owner) | Some(Role::Member) | Some(Role::Developer) => {
            PermissionDecision::Allowed
        }
        Some(Role::Guest) | None => {
            if !guest_allowed_devices.contains(device_id) {
                return PermissionDecision::Denied {
                    reason: REASON_GUEST_DEVICE_NOT_ALLOWED,
                    grantable_by: Role::Owner,
                };
            }
            if guest_until.is_some_and(|until| now >= until) {
                return PermissionDecision::Denied {
                    reason: REASON_GUEST_ACCESS_EXPIRED,
                    grantable_by: Role::Owner,
                };
            }
            PermissionDecision::Allowed
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn allowed(d: PermissionDecision) {
        assert_eq!(d, PermissionDecision::Allowed);
    }

    fn denied_with(d: PermissionDecision, reason: &'static str) {
        assert_eq!(
            d,
            PermissionDecision::Denied {
                reason,
                grantable_by: Role::Owner,
            }
        );
    }

    #[test]
    fn only_the_owner_may_remove_a_device() {
        allowed(may_remove_device(Some(Role::Owner)));
        for role in [Some(Role::Member), Some(Role::Guest), Some(Role::Developer), None] {
            denied_with(may_remove_device(role), REASON_OWNER_ONLY_REMOVE);
        }
    }

    #[test]
    fn only_the_owner_may_change_the_model() {
        allowed(may_change_model(Some(Role::Owner)));
        denied_with(may_change_model(Some(Role::Member)), REASON_OWNER_ONLY_MODEL);
    }

    #[test]
    fn only_the_owner_may_enroll_remote_access() {
        allowed(may_enroll_remote_access(Some(Role::Owner)));
        denied_with(
            may_enroll_remote_access(Some(Role::Guest)),
            REASON_OWNER_ONLY_REMOTE_ACCESS,
        );
    }

    #[test]
    fn only_the_owner_may_reset_the_pond() {
        allowed(may_reset_pond(Some(Role::Owner)));
        denied_with(may_reset_pond(None), REASON_OWNER_ONLY_RESET);
    }

    #[test]
    fn only_the_owner_may_manage_household_roles() {
        allowed(may_manage_household_roles(Some(Role::Owner)));
        for role in [Some(Role::Member), Some(Role::Guest), Some(Role::Developer), None] {
            denied_with(may_manage_household_roles(role), REASON_OWNER_ONLY_ROLES);
        }
    }

    #[test]
    fn owner_member_and_developer_always_control_any_device() {
        let now = Utc::now();
        for role in [Role::Owner, Role::Member, Role::Developer] {
            allowed(may_control_device(
                Some(role),
                "lamp-1",
                &HashSet::new(),
                None,
                now,
            ));
        }
    }

    #[test]
    fn a_guest_may_control_only_an_allow_listed_device() {
        let now = Utc::now();
        let allowed_devices = HashSet::from(["lamp-1".to_string()]);
        allowed(may_control_device(
            Some(Role::Guest),
            "lamp-1",
            &allowed_devices,
            None,
            now,
        ));
        denied_with(
            may_control_device(Some(Role::Guest), "lamp-2", &allowed_devices, None, now),
            REASON_GUEST_DEVICE_NOT_ALLOWED,
        );
    }

    #[test]
    fn an_anonymous_unidentified_voice_is_denied_like_an_un_allow_listed_guest() {
        let now = Utc::now();
        denied_with(
            may_control_device(None, "lamp-1", &HashSet::new(), None, now),
            REASON_GUEST_DEVICE_NOT_ALLOWED,
        );
    }

    #[test]
    fn a_guests_access_lapses_at_its_end_date() {
        let now = Utc::now();
        let allowed_devices = HashSet::from(["lamp-1".to_string()]);
        let still_future = now + chrono::Duration::hours(1);
        let already_past = now - chrono::Duration::hours(1);

        allowed(may_control_device(
            Some(Role::Guest),
            "lamp-1",
            &allowed_devices,
            Some(still_future),
            now,
        ));
        denied_with(
            may_control_device(
                Some(Role::Guest),
                "lamp-1",
                &allowed_devices,
                Some(already_past),
                now,
            ),
            REASON_GUEST_ACCESS_EXPIRED,
        );
    }

    #[test]
    fn a_guest_with_no_end_date_never_expires() {
        let now = Utc::now();
        let allowed_devices = HashSet::from(["lamp-1".to_string()]);
        allowed(may_control_device(
            Some(Role::Guest),
            "lamp-1",
            &allowed_devices,
            None,
            now,
        ));
    }
}
