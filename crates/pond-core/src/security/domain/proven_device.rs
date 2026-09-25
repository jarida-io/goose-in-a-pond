//! The device a request was proved to come from, the strongest resolver rung, so its id is
//! private and no constructor takes a caller string. A failed read is its own [`DeviceRung`].

use anyhow::Result;

use crate::security::ports::policy::Principal;

/// Deliberately not `Default`: "no device" must be spelled out as [`ProvenDevice::none`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProvenDevice(Option<String>);

impl ProvenDevice {
    /// The only constructor that can name a device. `Principal::device_id` comes solely from the
    /// auth middleware's `Handshake::caller_for_token` (the pairing token), never client input.
    pub fn from_principal(principal: &Principal) -> Self {
        Self(principal.device_id.clone())
    }

    /// No device (in-process, loopback bypass, or no `Principal`); lower rungs still apply.
    pub fn none() -> Self {
        Self(None)
    }

    /// The device id to look an attribution up by, if there is one.
    pub fn id(&self) -> Option<&str> {
        self.0.as_deref()
    }

    /// Rung for this request's attribution read; takes the `Result` so a failure can't be hidden.
    pub fn rung(&self, attribution: Result<Option<String>>) -> DeviceRung {
        if self.0.is_none() {
            // An attribution read for some *other* device must not speak for this request.
            return DeviceRung::NoDevice;
        }
        DeviceRung::from_attribution(attribution)
    }
}

/// The device rung's answer; matched without wildcards so a new variant must decide openly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceRung {
    /// Resolves to `Owner(id)` at `IdentificationSource::PairedDevice` strength.
    Member(String),
    /// No device on the request (loopback, in-process, adapter). Falls through.
    NoDevice,
    /// Registered but unclaimed: NULL means *I do not know*, never *everybody*. Falls through.
    Unattributed,
    /// The attribution read failed. Falls through loudly; access narrows, never assumes a member.
    Unavailable(String),
}

impl DeviceRung {
    /// Classify a read; nothing else may build [`DeviceRung::Member`] from one.
    pub fn from_attribution(attribution: Result<Option<String>>) -> Self {
        match attribution {
            Ok(Some(profile_id)) if !profile_id.trim().is_empty() => Self::Member(profile_id),
            // A blank id is unattributed (`checked_profile_id`'s rule): `Owner("")` would match no
            // rows and is not `Household` either.
            Ok(Some(_)) | Ok(None) => Self::Unattributed,
            Err(e) => Self::Unavailable(e.to_string()),
        }
    }

    /// The member for `identity_resolution::ResolutionInputs::paired_device_profile`, if any.
    pub fn profile_id(&self) -> Option<&str> {
        match self {
            Self::Member(id) => Some(id),
            Self::NoDevice | Self::Unattributed | Self::Unavailable(_) => None,
        }
    }

    /// Short, stable label for structured logs.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Member(_) => "member",
            Self::NoDevice => "no_device",
            Self::Unattributed => "unattributed",
            Self::Unavailable(_) => "unavailable",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::ports::policy::PrincipalKind;
    use crate::user_data::domain::profile::ProfileScope;
    use crate::user_data::domain::session::{IdentificationSource, SessionIdentity};
    use crate::user_data::services::identity_resolution::{resolve, ResolutionInputs};
    use anyhow::anyhow;

    /// Vacuity control for every refusal below.
    #[test]
    fn an_attributed_device_names_its_member() {
        let device =
            ProvenDevice::from_principal(&Principal::token("phone-liz").with_device("phone-liz"));
        let rung = device.rung(Ok(Some("liz".to_string())));
        assert_eq!(rung, DeviceRung::Member("liz".to_string()));
        assert_eq!(rung.profile_id(), Some("liz"));
    }

    #[test]
    fn an_unattributed_device_falls_through_rather_than_resolving_anybody() {
        let device =
            ProvenDevice::from_principal(&Principal::token("tablet").with_device("tablet"));
        let rung = device.rung(Ok(None));
        assert_eq!(
            rung,
            DeviceRung::Unattributed,
            "a NULL devices.profile_id is 'no household member has claimed this'. Reading it as \
             a member is the wrong-attribution failure PAI-1 exists to prevent."
        );
        assert_eq!(
            rung.profile_id(),
            None,
            "an unclaimed device must resolve to nobody -- not to the last member who paired"
        );
    }

    #[test]
    fn a_blank_profile_id_is_not_a_member() {
        for blank in ["", "   ", "\t"] {
            let device =
                ProvenDevice::from_principal(&Principal::token("phone").with_device("phone"));
            assert_eq!(
                device.rung(Ok(Some(blank.to_string()))),
                DeviceRung::Unattributed,
                "a blank profile id must not become Owner({blank:?})"
            );
        }
    }

    /// The privacy-critical direction: an unreadable store must never fall back to permissive.
    #[test]
    fn a_failed_attribution_read_narrows_and_says_which_failure_it_was() {
        let device = ProvenDevice::from_principal(&Principal::token("phone").with_device("phone"));
        let rung = device.rung(Err(anyhow!("database is locked")));
        match &rung {
            DeviceRung::Unavailable(why) => assert!(
                why.contains("database is locked"),
                "the storage error must survive into the rung, got: {why}"
            ),
            other => panic!(
                "a failed attribution read must be Unavailable, not {other:?}. Answering it with \
                 a member assumes an identity the pond could not read, and answering it with \
                 Unattributed makes a broken database look exactly like a shared tablet"
            ),
        }
        assert_eq!(
            rung.profile_id(),
            None,
            "a failed read must resolve nobody at PairedDevice strength"
        );
    }

    #[test]
    fn a_request_with_no_device_resolves_no_member_however_the_read_went() {
        let device = ProvenDevice::none();
        assert_eq!(device.id(), None);
        assert_eq!(
            device.rung(Ok(Some("liz".to_string()))),
            DeviceRung::NoDevice
        );
        assert_eq!(device.rung(Ok(None)), DeviceRung::NoDevice);
        assert_eq!(device.rung(Err(anyhow!("boom"))), DeviceRung::NoDevice);
    }

    #[test]
    fn a_principal_without_a_device_names_none() {
        for principal in [
            Principal::loopback(),
            Principal::internal(),
            Principal::token("gotg-1"),
        ] {
            assert_eq!(
                ProvenDevice::from_principal(&principal).id(),
                None,
                "{:?} carries no device and must not invent one",
                principal.kind
            );
        }
    }

    /// No `From<String>`, `new(&str)` or `Default`: a handler cannot honour `X-Device-Id`.
    #[test]
    fn the_only_way_to_name_a_device_is_from_a_principal() {
        let from_client_input = "attacker-chosen-device-id";
        // The only two ways to build one, exhaustively:
        assert_eq!(ProvenDevice::none().id(), None);
        assert_eq!(
            ProvenDevice::from_principal(&Principal::token("c").with_device(from_client_input))
                .id(),
            Some(from_client_input),
            "and even this one can only echo what the auth middleware put on the Principal"
        );
        // The middleware side is guarded by `pond-infra/tests/device_rung_wiring.rs`.
    }

    /// Every rung must be distinguishable in the one place an operator looks.
    #[test]
    fn every_rung_has_its_own_log_label() {
        let labels = [
            DeviceRung::Member("liz".into()).as_str(),
            DeviceRung::NoDevice.as_str(),
            DeviceRung::Unattributed.as_str(),
            DeviceRung::Unavailable("x".into()).as_str(),
        ];
        let unique: std::collections::BTreeSet<&str> = labels.iter().copied().collect();
        assert_eq!(
            unique.len(),
            labels.len(),
            "two rungs sharing a label make two different failures indistinguishable: {labels:?}"
        );
    }

    #[test]
    fn an_attributed_device_outranks_the_session_binding() {
        let device = ProvenDevice::from_principal(&Principal::token("p").with_device("p"));
        let rung = device.rung(Ok(Some("liz".to_string())));

        let mut session = SessionIdentity::unknown();
        session.profile_id = Some("jerry".to_string());
        session.source = IdentificationSource::Explicit;

        let resolved = resolve(&ResolutionInputs {
            paired_device_profile: rung.profile_id(),
            session: &session,
            household_has_multiple_members: true,
        });
        assert_eq!(resolved.scope, ProfileScope::Owner("liz".to_string()));
        assert_eq!(resolved.source, IdentificationSource::PairedDevice);
    }

    #[test]
    fn an_unattributed_device_leaves_the_next_rung_standing() {
        let device = ProvenDevice::from_principal(&Principal::token("t").with_device("t"));
        let rung = device.rung(Ok(None));

        let mut session = SessionIdentity::unknown();
        session.profile_id = Some("jerry".to_string());
        session.source = IdentificationSource::Explicit;

        let resolved = resolve(&ResolutionInputs {
            paired_device_profile: rung.profile_id(),
            session: &session,
            household_has_multiple_members: true,
        });
        assert_eq!(
            resolved.scope,
            ProfileScope::Owner("jerry".to_string()),
            "an unattributed device must not override the member this session was bound to; \
             falling through means leaving the next rung standing, not answering over it"
        );
        assert_eq!(resolved.source, IdentificationSource::Explicit);
    }

    /// Not evidence either way: a failed read neither promotes a guest nor demotes a member.
    #[test]
    fn a_failed_read_leaves_an_unidentified_speaker_exactly_where_it_found_them() {
        let device = ProvenDevice::from_principal(&Principal::token("p").with_device("p"));
        let rung = device.rung(Err(anyhow!("disk gone")));
        let resolved = resolve(&ResolutionInputs {
            paired_device_profile: rung.profile_id(),
            session: &SessionIdentity::unknown(),
            household_has_multiple_members: true,
        });
        assert_eq!(
            resolved.scope,
            ProfileScope::Guest,
            "an unreadable attribution store must leave a stranger a stranger"
        );
        assert_eq!(resolved.source, IdentificationSource::Unknown);
    }

    #[test]
    fn a_token_principal_keeps_its_kind_when_a_device_is_attached() {
        let principal = Principal::token("gotg-7").with_device("gotg-7");
        assert_eq!(principal.kind, PrincipalKind::Token("gotg-7".to_string()));
        assert_eq!(principal.device_id.as_deref(), Some("gotg-7"));
        assert_eq!(
            principal.proven_profile_id, None,
            "attaching a device must not, by itself, prove a member"
        );
    }
}
