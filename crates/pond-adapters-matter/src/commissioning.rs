//! [`MatterCommissioner`], the [`DeviceCommissioningPort`] over a live controller connection.
//! QR payloads, manual codes and passcodes all go to one `commission` op.

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
    describe, matter_device_id, matter_node_id, setup_code_kind, CommissionResult, DiscoverResult,
    CODE_NOTHING_PAIRABLE,
};

/// Discovery, attestation and fabric join often take over a minute on a busy network.
const COMMISSION_TIMEOUT: Duration = Duration::from_secs(180);

/// Unpairing an unreachable node first waits out a ~15-30 s CHIP timeout, past the default 15 s.
const DECOMMISSION_TIMEOUT: Duration = Duration::from_secs(90);

/// The mDNS pre-flight answers in under a second; kept low so a hung probe can't add much wait.
const DISCOVER_TIMEOUT: Duration = Duration::from_secs(15);

/// Matter devices advertise `_matterc._udp` only for ~15 minutes after boot.
const NOTHING_IN_PAIRING_MODE: &str =
    "No device found in pairing mode. Put the device into pairing mode and try again — a Matter \
     device stops accepting new connections about 15 minutes after it starts.";

pub struct MatterCommissioner {
    client: Arc<MatterClient>,
    notifier: MatterNotifier,
}

impl MatterCommissioner {
    pub fn new(client: Arc<MatterClient>, notifier: MatterNotifier) -> Self {
        Self { client, notifier }
    }

    /// Refuse early and legibly when nothing is in pairing mode, which the controller would report
    /// as a discovery timeout. A probe that itself fails never blocks commissioning.
    async fn refuse_when_nothing_is_pairable(&self) -> Result<()> {
        // Unboxed devices advertise over BLE only, which this mDNS probe can't see.
        if self.client.has_ble() {
            tracing::debug!(
                target: "giap::trace",
                kind = "matter_pairing_probe_skipped",
                reason = "ble_active",
                "matter: BLE is active, so an mDNS probe cannot say nothing is pairable"
            );
            return Ok(());
        }

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
                // Controller wording, except where we have better advice than "it failed".
                let told = if code == CODE_NOTHING_PAIRABLE {
                    NOTHING_IN_PAIRING_MODE.to_string()
                } else {
                    describe(&e)
                };
                self.notifier.pairing_failed(&told).await;
                // Return only `told`: the dialog then matches the notification, and the wire
                // code stays out of `pond-api`'s `{e:#}` rendering.
                return Err(anyhow::anyhow!("{told}"));
            }
        };

        let CommissionResult { device } = serde_json::from_value(result)
            .context("the controller did not return a commissioned device")?;
        let node_id = matter_node_id(&device.id).with_context(|| {
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
        // No notification here: the bridge announces the device once, however it arrived.

        Ok(CommissionedDevice {
            device_id: device.id.clone(),
            name: name.unwrap_or_else(|| device.name.clone()),
            node_id,
            device_type: device.device_type.clone(),
            capabilities: device.capabilities.clone(),
        })
    }

    async fn decommission(&self, node_id: u64) -> Result<()> {
        let device_id = matter_device_id(node_id, None);
        // Before the op: `device_removed` can reach the bridge before this call returns.
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
