//! [`MatterCommissioner`] — the [`DeviceCommissioningPort`] over a live
//! controller connection.
//!
//! Both setup-code forms go to one `commission` op: the controller decodes the
//! payload and decides how to find the device, which is knowledge that belongs
//! next to matter.js rather than here.
//!
//! `decommission` removes the node from the fabric, which the delete path uses
//! so a removed device does not re-announce itself on the next `subscribe`.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use async_trait::async_trait;
use pond_core::user_data::ports::device_commissioning::{
    CommissionedDevice, DeviceCommissioningPort, SetupCode,
};
use serde_json::json;

use crate::client::{code_of, MatterClient};
use crate::notify::MatterNotifier;
use crate::protocol::{
    describe, device_id_for_node, node_id_from_device_id, setup_code_kind, CommissionResult,
    DiscoverResult, CODE_NOTHING_PAIRABLE,
};

/// Commissioning is slow: discovery, attestation, and fabric join, often over a
/// minute on a busy network. Well past the default op timeout.
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(180);

/// Removal is slow too: the controller first tries to unpair from the device,
/// which for an unreachable node waits out an mDNS/CHIP timeout (~15-30s) before
/// removing the node from its own storage. The default 15s timeout is shorter
/// than that, so GIAP would give up while the controller was still finishing —
/// reporting a failure for a removal that actually happened.
const DECOMMISSION_TIMEOUT: Duration = Duration::from_secs(90);

/// The pre-flight probe is a local mDNS browse, so it answers in well under a
/// second when anything is advertising. Bounded low on purpose: its whole value
/// is being cheaper than the discovery timeout it saves, and a probe that hangs
/// must not add to the wait.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(15);

/// What the user is told when nothing is advertising itself for pairing. The 15
/// minutes is the Matter commissioning window: a device advertises
/// `_matterc._udp` for roughly that long after it boots and then stops, which
/// makes "it was pairable earlier" the normal way to arrive here.
const NOTHING_IN_PAIRING_MODE: &str =
    "No device found in pairing mode. Put the device into pairing mode and try again — a Matter \
     device stops accepting new connections about 15 minutes after it starts.";

/// What the user is told when the controller rejects the setup code itself
/// (matter.js decodes the manual pairing code's built-in check digit before
/// any network activity, so this comes back in a couple of milliseconds —
/// fast enough that it is easy to mistake for a system fault rather than a
/// mistyped digit). GIAP's own `parse_setup_code` only checks length and
/// character set, not the check digit, so a code that passes that gate can
/// still fail here.
const SETUP_CODE_REJECTED: &str =
    "That setup code isn't valid — a single mistyped or misread digit anywhere in the code will \
     cause this. Double-check it against the device's label, or scan its QR code instead of \
     typing the manual code by hand.";

/// Whether `e` is matter.js rejecting the setup code's own shape (bad check
/// digit, malformed QR payload) rather than a discovery/attestation/network
/// failure. `commission_failed` on the controller side is a catch-all — see
/// the comment at its call site — so this matches on the message text
/// matter.js actually produces, the same way `CODE_NOTHING_PAIRABLE` is
/// matched on its dedicated code.
fn is_setup_code_rejection(e: &anyhow::Error) -> bool {
    let msg = describe(e).to_lowercase();
    msg.contains("invalid pairing code")
        || msg.contains("invalid manual pairing code")
        || msg.contains("invalid qr code")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_check_digit_is_recognised_as_a_setup_code_rejection() {
        // The exact wording matter.js produces for a manual pairing code whose
        // check digit doesn't match — this is what a mistyped/misread digit
        // anywhere in the code looks like on the wire.
        let e = anyhow::anyhow!("commission_failed").context("Invalid pairing code");
        assert!(is_setup_code_rejection(&e));
    }

    #[test]
    fn a_malformed_qr_payload_is_also_recognised() {
        let e = anyhow::anyhow!("commission_failed").context("Invalid QR code");
        assert!(is_setup_code_rejection(&e));
    }

    #[test]
    fn a_genuine_reachability_failure_is_not_mistaken_for_a_bad_code() {
        // Same generic `commission_failed` code as a rejected setup code, but a
        // completely different cause — this must NOT get the "check your code"
        // advice.
        let e = anyhow::anyhow!("commission_failed")
            .context("Operative reconnection with device failed: Peer has been unreachable");
        assert!(!is_setup_code_rejection(&e));
    }
}

pub struct MatterCommissioner {
    client: Arc<MatterClient>,
    notifier: MatterNotifier,
}

impl MatterCommissioner {
    pub fn new(client: Arc<MatterClient>, notifier: MatterNotifier) -> Self {
        Self { client, notifier }
    }

    /// Refuse early, and legibly, when nothing is in pairing mode.
    ///
    /// Commissioning finds the device over mDNS, so "nothing is advertising"
    /// settles the whole call. Left to the controller it does not look like
    /// that: it waits out a discovery timeout and answers "commissioning
    /// failed", with the actual reason buried in its own output. That is the
    /// most common way commissioning fails, because a device stops advertising
    /// ~15 minutes after it boots, and it is the one failure a user can fix in
    /// ten seconds — if anything tells them what it is.
    ///
    /// A probe that itself fails proves nothing, so it never blocks the attempt:
    /// an unexpected payload or a slow controller falls through to the real
    /// commission rather than inventing a reason to refuse.
    async fn refuse_when_nothing_is_pairable(&self) -> Result<()> {
        let found = match self
            .client
            .send_with_timeout("discover", json!({}), DISCOVER_TIMEOUT)
            .await
        {
            Ok(result) => serde_json::from_value::<DiscoverResult>(result)
                .ok()
                .map(|r| r.commissionable),
            Err(e) => {
                tracing::warn!(error = %e, "matter: could not probe for commissionable devices");
                None
            }
        };

        if found == Some(0) {
            anyhow::bail!(NOTHING_IN_PAIRING_MODE);
        }
        Ok(())
    }
}

#[async_trait]
impl DeviceCommissioningPort for MatterCommissioner {
    async fn commission(
        &self,
        code: SetupCode,
        name: Option<String>,
    ) -> Result<CommissionedDevice> {
        // Before the discovery wait, not after it: the answer is already known.
        if let Err(e) = self.refuse_when_nothing_is_pairable().await {
            self.notifier.pairing_failed(NOTHING_IN_PAIRING_MODE).await;
            return Err(e);
        }

        // The code itself never reaches a log; only which kind it is.
        let raw = match &code {
            SetupCode::PairingCode(code) => code.clone(),
            SetupCode::Passcode(pin) => pin.to_string(),
        };
        tracing::info!(
            target: "giap::trace",
            kind = "matter_commission_started",
            code_kind = setup_code_kind(&raw),
            named = name.is_some(),
            "matter: commissioning a device"
        );

        let mut params = json!({ "code": raw });
        if let Some(name) = &name {
            params["name"] = json!(name);
        }

        let result = match self
            .client
            .send_with_timeout("commission", params, COMMISSION_TIMEOUT)
            .await
        {
            Ok(result) => result,
            Err(e) => {
                let code = code_of(&e).unwrap_or("none").to_string();
                tracing::warn!(
                    target: "giap::trace",
                    kind = "matter_commission_failed",
                    error_code = %code,
                    error = %describe(&e),
                    "matter: commissioning failed"
                );
                // The controller's own wording, except for the failures that have
                // better advice than "it failed". `commission_failed` is a
                // catch-all on the controller side (matter.js does not give GIAP
                // a distinct code for "the check digit doesn't match"), so a
                // rejected setup code is told apart from a real discovery/network
                // failure by matching on the controller's message text instead.
                let told = if code == CODE_NOTHING_PAIRABLE {
                    NOTHING_IN_PAIRING_MODE.to_string()
                } else if is_setup_code_rejection(&e) {
                    SETUP_CODE_REJECTED.to_string()
                } else {
                    describe(&e)
                };
                self.notifier.pairing_failed(&told).await;
                // `told` is also what the HTTP caller sees (`{e:#}` in
                // `commission_device`, `pond-api/src/routes.rs`) — without this,
                // the friendlier wording only ever reached the push notifier and
                // the synchronous commission-form error stayed on the raw
                // controller text.
                return Err(anyhow::anyhow!(told)).context("commissioning failed");
            }
        };

        let CommissionResult { device } = serde_json::from_value(result)
            .context("the controller did not return a commissioned device")?;
        let node_id = node_id_from_device_id(&device.id).with_context(|| {
            format!(
                "the controller returned '{}', which is not a Matter device id",
                device.id
            )
        })?;

        tracing::info!(
            target: "giap::trace",
            kind = "matter_commission_succeeded",
            device = %device.id,
            device_type = %device.device_type,
            "matter: device joined the fabric"
        );
        // The pairing notification is raised by the bridge when the device
        // reaches the registry, so a device is announced once however it arrived
        // — through this call, or through the `device_added` event that follows.

        Ok(CommissionedDevice {
            device_id: device.id.clone(),
            name: name.unwrap_or_else(|| device.name.clone()),
            node_id,
            device_type: device.device_type.clone(),
            capabilities: device.capabilities.clone(),
        })
    }

    async fn decommission(&self, node_id: u64) -> Result<()> {
        let device_id = device_id_for_node(node_id);
        // Before the op, not after: the controller's `device_removed` event can
        // reach the bridge while this call is still returning.
        self.notifier.expect_removal(&device_id).await;
        match self
            .client
            .send_with_timeout(
                "decommission",
                json!({ "device_id": device_id }),
                DECOMMISSION_TIMEOUT,
            )
            .await
        {
            Ok(_) => {
                tracing::info!(
                    target: "giap::trace",
                    kind = "matter_node_removed",
                    device = %device_id,
                    "matter: node removed from the fabric"
                );
                Ok(())
            }
            Err(e) => Err(e).context("removing the node from the fabric failed"),
        }
    }
}
