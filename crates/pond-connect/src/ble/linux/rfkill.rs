//! Whether a radio is switched off by rfkill, read from sysfs so it needs no privilege, and the
//! peripheral can say so instead of reporting BlueZ's vaguer error.

use std::fs;
use std::path::Path;

pub(super) const SYSFS_RFKILL: &str = "/sys/class/rfkill";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Block {
    /// `rfkill unblock` lifts it.
    Soft,
    /// A switch or the firmware holds it; software cannot lift it.
    Hard,
}

impl Block {
    pub(super) fn reason(self, radio: &str) -> String {
        match self {
            Self::Soft => format!("{radio} is switched off by rfkill (rfkill unblock {radio})"),
            Self::Hard => format!("{radio} is switched off by a hardware rfkill switch"),
        }
    }
}

/// The strongest block on any radio of this `kind` (`bluetooth`, `wlan`), or `None` when every one
/// is free or the host has none.
pub(super) fn blocked(root: &Path, kind: &str) -> Option<Block> {
    let entries = fs::read_dir(root).ok()?;
    let mut strongest = None;
    for entry in entries.flatten() {
        let dir = entry.path();
        if read(&dir.join("type")).as_deref() != Some(kind) {
            continue;
        }
        if read(&dir.join("hard")).as_deref() == Some("1") {
            return Some(Block::Hard);
        }
        if read(&dir.join("soft")).as_deref() == Some("1") {
            strongest = Some(Block::Soft);
        }
    }
    strongest
}

fn read(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .map(|text| text.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn radio(root: &Path, name: &str, kind: &str, soft: &str, hard: &str) {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("type"), format!("{kind}\n")).unwrap();
        fs::write(dir.join("soft"), format!("{soft}\n")).unwrap();
        fs::write(dir.join("hard"), format!("{hard}\n")).unwrap();
    }

    #[test]
    fn a_radio_is_blocked_only_by_its_own_kind_and_hard_beats_soft() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(blocked(root.path(), "bluetooth"), None, "no radios at all");

        radio(root.path(), "rfkill0", "wlan", "1", "0");
        radio(root.path(), "rfkill1", "bluetooth", "0", "0");
        assert_eq!(blocked(root.path(), "bluetooth"), None);
        assert_eq!(blocked(root.path(), "wlan"), Some(Block::Soft));

        radio(root.path(), "rfkill2", "bluetooth", "1", "0");
        assert_eq!(blocked(root.path(), "bluetooth"), Some(Block::Soft));
        radio(root.path(), "rfkill3", "bluetooth", "0", "1");
        assert_eq!(blocked(root.path(), "bluetooth"), Some(Block::Hard));

        assert!(blocked(&root.path().join("missing"), "bluetooth").is_none());
    }

    #[test]
    fn the_reason_names_the_fix_when_there_is_one() {
        assert_eq!(
            Block::Soft.reason("bluetooth"),
            "bluetooth is switched off by rfkill (rfkill unblock bluetooth)"
        );
        assert!(!Block::Hard.reason("bluetooth").contains("unblock"));
    }
}
