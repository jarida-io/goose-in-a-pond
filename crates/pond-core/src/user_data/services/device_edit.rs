//! Who may change a device row, and how a partial edit lands on it.

use crate::user_data::ports::device_registry::{Device, UpdateDeviceRequest};

/// The client types a pairing records as a row's `device_type`: such a row is that client's own.
pub const PAIRED_CLIENT_TYPES: [&str; 3] = ["gotg", "desktop", "cli"];

pub fn is_paired_client(device_type: &str) -> bool {
    PAIRED_CLIENT_TYPES.contains(&device_type)
}

/// A paired client's row is changed only by that client or the host, so one phone cannot rename
/// another member's. A shared row (a light, a hub) is the household's: any authenticated caller.
pub fn may_edit(row: &Device, caller_device: Option<&str>, caller_is_host: bool) -> bool {
    if caller_is_host || !is_paired_client(&row.device_type) {
        return true;
    }
    caller_device == Some(row.id.as_str())
}

/// A partial edit. `None` keeps the stored value; for hostname and room, `Some(None)` or a blank
/// string clears it, because the Configure panel sends an empty field to mean "none".
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DeviceEdit {
    pub name: Option<String>,
    pub hostname: Option<Option<String>>,
    pub room: Option<Option<String>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DeviceEditError {
    #[error("a device's name cannot be empty")]
    EmptyName,
}

pub fn merge(current: &Device, edit: DeviceEdit) -> Result<UpdateDeviceRequest, DeviceEditError> {
    let name = match edit.name {
        Some(name) if name.trim().is_empty() => return Err(DeviceEditError::EmptyName),
        Some(name) => name.trim().to_string(),
        None => current.name.clone(),
    };
    let field = |edited: Option<Option<String>>, stored: &Option<String>| match edited {
        None => stored.clone(),
        Some(value) => value
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()),
    };
    Ok(UpdateDeviceRequest {
        name,
        hostname: field(edit.hostname, &current.hostname),
        room: field(edit.room, &current.room),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, device_type: &str) -> Device {
        Device {
            id: id.into(),
            name: "Old".into(),
            device_type: device_type.into(),
            hostname: Some("old-host".into()),
            ip_address: None,
            capabilities: vec![],
            registered_at: "2026-10-17T00:00:00Z".into(),
            last_seen: None,
            is_online: true,
            room: Some("Kitchen".into()),
        }
    }

    #[test]
    fn a_phone_edits_its_own_row_and_not_another_phones() {
        let amina = row("install-amina", "gotg");
        assert!(may_edit(&amina, Some("install-amina"), false));
        assert!(!may_edit(&amina, Some("install-liz"), false));
        assert!(!may_edit(&amina, None, false));
    }

    #[test]
    fn the_host_edits_any_row() {
        assert!(may_edit(&row("install-amina", "gotg"), None, true));
        assert!(may_edit(
            &row("desktop-1", "desktop"),
            Some("install-liz"),
            true
        ));
    }

    #[test]
    fn a_shared_row_is_any_callers_to_edit() {
        for device_type in ["matter_light", "sensor", "pond", "smart_speaker"] {
            assert!(may_edit(
                &row("matter-7", device_type),
                Some("install-liz"),
                false
            ));
            assert!(may_edit(&row("matter-7", device_type), None, false));
        }
    }

    #[test]
    fn every_pairing_client_type_is_a_paired_client() {
        for device_type in PAIRED_CLIENT_TYPES {
            assert!(is_paired_client(device_type));
        }
        assert!(!is_paired_client("matter_light"));
    }

    #[test]
    fn omitted_fields_keep_their_stored_values() {
        let merged = merge(
            &row("d", "gotg"),
            DeviceEdit {
                name: Some("  Amina's iPhone ".into()),
                ..DeviceEdit::default()
            },
        )
        .unwrap();
        assert_eq!(merged.name, "Amina's iPhone");
        assert_eq!(merged.hostname.as_deref(), Some("old-host"));
        assert_eq!(merged.room.as_deref(), Some("Kitchen"));
    }

    #[test]
    fn a_null_or_blank_field_clears_it_and_no_name_keeps_the_name() {
        let merged = merge(
            &row("d", "gotg"),
            DeviceEdit {
                name: None,
                hostname: Some(None),
                room: Some(Some("  ".into())),
            },
        )
        .unwrap();
        assert_eq!(merged.name, "Old");
        assert_eq!(merged.hostname, None);
        assert_eq!(merged.room, None);
    }

    #[test]
    fn a_blank_name_is_refused() {
        let edit = DeviceEdit {
            name: Some("   ".into()),
            ..DeviceEdit::default()
        };
        assert_eq!(
            merge(&row("d", "gotg"), edit).unwrap_err(),
            DeviceEditError::EmptyName
        );
    }
}
