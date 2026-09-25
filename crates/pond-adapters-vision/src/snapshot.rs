//! JPEG snapshots behind `camera_events.snapshot_path`, pruned to the newest
//! [`SnapshotConfig::keep`] per camera on every write.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use pond_core::user_data::domain::vision::Frame;

#[derive(Debug, Clone)]
pub struct SnapshotConfig {
    /// Directory snapshots are written into (created on first write).
    pub dir: PathBuf,
    /// Newest snapshots kept per camera; older ones are deleted after each write.
    pub keep: usize,
}

impl SnapshotConfig {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, keep: 100 }
    }
}

/// `camera_id` comes from settings and must not steer the path: non-`[A-Za-z0-9_-]` becomes `-`.
fn sanitize_camera_id(camera_id: &str) -> String {
    let cleaned: String = camera_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "camera".to_string()
    } else {
        cleaned
    }
}

/// Save `frame` as a JPEG named `<camera>-<UTC ms timestamp>.jpg` (sorts by time) and prune.
pub fn write_snapshot(cfg: &SnapshotConfig, camera_id: &str, frame: &Frame) -> Result<PathBuf> {
    if !frame.is_well_formed() {
        anyhow::bail!("malformed frame ({}x{})", frame.width, frame.height);
    }
    fs::create_dir_all(&cfg.dir)
        .with_context(|| format!("creating snapshot dir {}", cfg.dir.display()))?;

    let image = image::RgbImage::from_raw(frame.width, frame.height, frame.rgb.clone())
        .context("frame buffer does not match its dimensions")?;
    let camera = sanitize_camera_id(camera_id);
    let path = cfg.dir.join(format!(
        "{camera}-{}.jpg",
        frame.captured_at.format("%Y%m%dT%H%M%S%3fZ")
    ));
    image
        .save_with_format(&path, image::ImageFormat::Jpeg)
        .with_context(|| format!("writing snapshot {}", path.display()))?;

    prune_snapshots(&cfg.dir, &camera, cfg.keep)?;
    Ok(path)
}

/// Delete all but the newest `keep` snapshots for `camera`, by [`write_snapshot`]'s names.
fn prune_snapshots(dir: &Path, camera: &str, keep: usize) -> Result<()> {
    let prefix = format!("{camera}-");
    let mut mine: Vec<PathBuf> = fs::read_dir(dir)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.extension().is_some_and(|e| e == "jpg")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with(&prefix))
        })
        .collect();
    if mine.len() <= keep {
        return Ok(());
    }
    mine.sort(); // timestamp-named → chronological
    let excess = mine.len() - keep;
    for stale in mine.into_iter().take(excess) {
        if let Err(e) = fs::remove_file(&stale) {
            tracing::warn!(path = %stale.display(), error = %e, "failed to prune snapshot");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, Utc};

    fn frame_at(offset_ms: i64) -> Frame {
        Frame {
            width: 8,
            height: 6,
            rgb: vec![128; Frame::expected_len(8, 6)],
            captured_at: Utc::now() + Duration::milliseconds(offset_ms),
        }
    }

    #[test]
    fn writes_a_decodable_jpeg_and_returns_its_path() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SnapshotConfig::new(tmp.path().to_path_buf());

        let path = write_snapshot(&cfg, "camera-1", &frame_at(0)).unwrap();
        assert!(path.exists());
        let decoded = image::open(&path).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (8, 6));
    }

    #[test]
    fn prunes_to_keep_newest_per_camera_only() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SnapshotConfig {
            dir: tmp.path().to_path_buf(),
            keep: 2,
        };

        for i in 0..4 {
            write_snapshot(&cfg, "cam-a", &frame_at(i * 10)).unwrap();
        }
        // Another camera's snapshots must not count against cam-a's budget.
        let other = write_snapshot(&cfg, "cam-b", &frame_at(0)).unwrap();

        let mut cam_a: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.starts_with("cam-a-"))
            .collect();
        cam_a.sort();
        assert_eq!(cam_a.len(), 2, "only the newest 2 kept: {cam_a:?}");
        assert!(other.exists(), "other camera untouched");
    }

    #[test]
    fn camera_id_cannot_escape_the_snapshot_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SnapshotConfig::new(tmp.path().join("snaps"));

        let path = write_snapshot(&cfg, "../../etc/passwd", &frame_at(0)).unwrap();
        assert!(
            path.starts_with(tmp.path().join("snaps")),
            "path escaped: {}",
            path.display()
        );
        assert!(path.exists());
    }

    #[test]
    fn malformed_frames_are_rejected() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = SnapshotConfig::new(tmp.path().to_path_buf());
        let mut bad = frame_at(0);
        bad.rgb.truncate(5);
        assert!(write_snapshot(&cfg, "camera-1", &bad).is_err());
    }
}
