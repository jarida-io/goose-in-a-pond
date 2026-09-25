//! Driven port: which household member a device belongs to; an unattributed device is nobody's.
//! Not on `DeviceRegistry`, whose many stub impls would answer these security questions.

use anyhow::{anyhow, Result};
use async_trait::async_trait;

use crate::user_data::domain::push_token::PushToken;

/// The `target` that means "every connected device". Keep this the only spelling (infra
/// re-exports it); device ids are caller-supplied, so targeted paths must refuse it.
pub const RESERVED_BROADCAST_TARGET: &str = "broadcast";

/// Why a targeted delivery reached nobody. Every variant is a refusal, never a fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Undeliverable {
    /// The id is blank or whitespace, which [`checked_profile_id`] refuses.
    NotAMember,
    /// The attribution read failed; access narrows, never widens to a broadcast.
    AttributionUnavailable(String),
    /// The member has no device of their own (unclaimed devices are never substituted).
    NoAttributedDevice,
    /// Every device attributed to this member is the reserved broadcast sentinel.
    ReservedTargetOnly,
}

impl Undeliverable {
    /// Short, stable label for structured logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::NotAMember => "not_a_member",
            Self::AttributionUnavailable(_) => "attribution_unavailable",
            Self::NoAttributedDevice => "no_attributed_device",
            Self::ReservedTargetOnly => "reserved_target_only",
        }
    }
}

/// Where a member's targeted notification goes. Deliberately has no broadcast variant.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TargetedDelivery {
    /// One copy per device id; non-empty, never [`RESERVED_BROADCAST_TARGET`].
    ToDevices(Vec<String>),
    /// Deliver to nobody, for this reason.
    Undeliverable(Undeliverable),
}

impl TargetedDelivery {
    /// Turn a member id plus the attribution read into a plan. Takes the `Result` so a
    /// storage error can't be `unwrap_or_default`ed into "owns no phone".
    pub fn plan(profile_id: &str, attributed_devices: Result<Vec<String>>) -> Self {
        if checked_profile_id(profile_id).is_err() {
            return Self::Undeliverable(Undeliverable::NotAMember);
        }
        let devices = match attributed_devices {
            Ok(devices) => devices,
            Err(e) => {
                return Self::Undeliverable(Undeliverable::AttributionUnavailable(e.to_string()))
            }
        };
        if devices.is_empty() {
            return Self::Undeliverable(Undeliverable::NoAttributedDevice);
        }
        let addressable: Vec<String> = devices
            .into_iter()
            .filter(|id| id != RESERVED_BROADCAST_TARGET)
            .collect();
        if addressable.is_empty() {
            return Self::Undeliverable(Undeliverable::ReservedTargetOnly);
        }
        Self::ToDevices(addressable)
    }

    /// The device ids to deliver to, empty when this plan delivers to nobody.
    pub fn devices(&self) -> &[String] {
        match self {
            Self::ToDevices(ids) => ids,
            Self::Undeliverable(_) => &[],
        }
    }
}

/// Reject a blank or whitespace-only profile id: stored, `ON DELETE SET NULL` never clears
/// it; read, it looks like "owns no devices".
pub fn checked_profile_id(profile_id: &str) -> Result<&str> {
    if profile_id.trim().is_empty() {
        return Err(anyhow!(
            "a blank profile id is not a household member; pass the member's id, \
             or None to leave the device unattributed"
        ));
    }
    Ok(profile_id)
}

/// Driven port: the device <-> household-member association.
#[async_trait]
pub trait DeviceAttribution: Send + Sync {
    /// Bind a device to a member (`None` releases it); an unregistered device is an error.
    async fn set_device_profile(&self, device_id: &str, profile_id: Option<&str>) -> Result<()>;

    /// Which member owns this device; `None` means nobody has claimed it, never everybody.
    async fn device_profile(&self, device_id: &str) -> Result<Option<String>>;

    /// Ids of this member's devices, newest registration first; never an unattributed one.
    async fn devices_for_profile(&self, profile_id: &str) -> Result<Vec<String>>;

    /// Push tokens of this member's devices; empty means "not delivered", never a broadcast.
    async fn push_tokens_for_profile(&self, profile_id: &str) -> Result<Vec<PushToken>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_real_profile_id_passes_through_unchanged() {
        assert_eq!(checked_profile_id("liz").unwrap(), "liz");
        // Not trimmed: rewriting it would make the lookup disagree with the write.
        assert_eq!(checked_profile_id(" liz ").unwrap(), " liz ");
    }

    #[test]
    fn a_blank_profile_id_is_refused_rather_than_answered() {
        for blank in ["", " ", "\t", "\n  "] {
            let err = checked_profile_id(blank).unwrap_err().to_string();
            assert!(
                err.contains("blank profile id"),
                "a blank id must be refused by name, got: {err}"
            );
        }
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    /// Vacuity control for the refusal tests below.
    #[test]
    fn a_member_with_devices_is_delivered_to_each_of_them() {
        let plan = TargetedDelivery::plan("liz", Ok(ids(&["phone-liz", "watch-liz"])));
        assert_eq!(
            plan,
            TargetedDelivery::ToDevices(ids(&["phone-liz", "watch-liz"])),
            "a member with two devices gets two targets, in the order the port returned"
        );
        assert_eq!(plan.devices().len(), 2);
    }

    #[test]
    fn a_failed_attribution_read_delivers_to_nobody_and_says_which_failure_it_was() {
        let plan = TargetedDelivery::plan("liz", Err(anyhow!("database is locked")));
        match &plan {
            TargetedDelivery::Undeliverable(Undeliverable::AttributionUnavailable(why)) => {
                assert!(
                    why.contains("database is locked"),
                    "the storage error must survive into the plan, got: {why}"
                );
            }
            other => panic!(
                "a failed attribution read must be AttributionUnavailable, not {other:?}. \
                 Answering it with an empty device list makes a broken database read, in a log, \
                 exactly like a member who has never paired a phone"
            ),
        }
        assert!(
            plan.devices().is_empty(),
            "nothing is delivered when the read failed"
        );
    }

    #[test]
    fn a_member_with_no_attributed_device_is_unreachable_not_broadcast() {
        let plan = TargetedDelivery::plan("liz", Ok(Vec::new()));
        assert_eq!(
            plan,
            TargetedDelivery::Undeliverable(Undeliverable::NoAttributedDevice)
        );
        assert!(plan.devices().is_empty());
    }

    #[test]
    fn the_reserved_sentinel_is_never_a_delivery_target() {
        let plan =
            TargetedDelivery::plan("liz", Ok(ids(&[RESERVED_BROADCAST_TARGET, "phone-liz"])));
        assert_eq!(
            plan,
            TargetedDelivery::ToDevices(ids(&["phone-liz"])),
            "the sentinel is dropped and the member's real device still gets it"
        );

        let only = TargetedDelivery::plan("liz", Ok(ids(&[RESERVED_BROADCAST_TARGET])));
        assert_eq!(
            only,
            TargetedDelivery::Undeliverable(Undeliverable::ReservedTargetOnly),
            "and when the sentinel is all there is, the answer is nobody -- not everybody"
        );
    }

    #[test]
    fn a_blank_profile_id_produces_a_plan_rather_than_an_error_to_swallow() {
        for blank in ["", "   "] {
            assert_eq!(
                TargetedDelivery::plan(blank, Ok(ids(&["phone-liz"]))),
                TargetedDelivery::Undeliverable(Undeliverable::NotAMember),
                "a blank id must not be allowed to reach a device list that was fetched for \
                 somebody else"
            );
        }
    }

    /// Compile-time tripwire: keep the match wildcard-free so a new variant fails to build.
    #[test]
    fn the_delivery_plan_cannot_express_a_broadcast() {
        let plan = TargetedDelivery::plan("liz", Ok(ids(&["phone-liz"])));
        let described = match &plan {
            TargetedDelivery::ToDevices(devices) => {
                assert!(
                    !devices.iter().any(|d| d == RESERVED_BROADCAST_TARGET),
                    "ToDevices must never carry the sentinel: {devices:?}"
                );
                "to devices"
            }
            TargetedDelivery::Undeliverable(reason) => match reason {
                Undeliverable::NotAMember
                | Undeliverable::AttributionUnavailable(_)
                | Undeliverable::NoAttributedDevice
                | Undeliverable::ReservedTargetOnly => "to nobody",
            },
        };
        assert_eq!(described, "to devices");
    }

    #[test]
    fn every_undeliverable_reason_has_its_own_log_label() {
        let labels = [
            Undeliverable::NotAMember.as_str(),
            Undeliverable::AttributionUnavailable("x".into()).as_str(),
            Undeliverable::NoAttributedDevice.as_str(),
            Undeliverable::ReservedTargetOnly.as_str(),
        ];
        let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(
            unique.len(),
            labels.len(),
            "two reasons sharing a label make the two failures indistinguishable in the one \
             place an operator looks: {labels:?}"
        );
    }
}
