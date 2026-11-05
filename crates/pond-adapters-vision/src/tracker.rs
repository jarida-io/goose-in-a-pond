//! Follows people from one detector pass to the next by box overlap, so a person standing in view
//! is one presence rather than an event per check.

use std::time::{Duration, Instant};

use pond_core::user_data::domain::vision::FractionBox;

/// Overlap a detection needs with a track's last box to continue it; a person walking across
/// the frame at the detector's 5 fps still overlaps their last box by more.
const MIN_IOU: f32 = 0.2;

/// Passes in a row a new track needs before it counts, so one stray box raises nothing.
const CONFIRM_HITS: u32 = 2;

/// How long a person outlives their last sighting: a missed pass, or a moment turned away.
pub const LOST_AFTER: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq)]
pub struct Track {
    pub id: u64,
    pub bbox: FractionBox,
    pub confidence: f64,
    pub confirmed: bool,
    hits: u32,
    last_seen: Instant,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackChange {
    /// Seen on enough passes running: a person is here.
    Confirmed(u64),
    /// A confirmed person not seen for [`LOST_AFTER`].
    Lost(u64),
}

#[derive(Debug, Default)]
pub struct PersonTracker {
    tracks: Vec<Track>,
    next_id: u64,
}

impl PersonTracker {
    /// Fold one detector pass in; `people` are the person boxes it found with their confidence.
    pub fn update(&mut self, people: &[(FractionBox, f64)], now: Instant) -> Vec<TrackChange> {
        let mut changes = Vec::new();

        // Greedy on overlap: the closest pair first, each track and detection used once.
        let mut pairs: Vec<(f32, usize, usize)> = Vec::new();
        for (ti, track) in self.tracks.iter().enumerate() {
            for (di, (bbox, _)) in people.iter().enumerate() {
                let iou = track.bbox.iou(bbox);
                if iou >= MIN_IOU {
                    pairs.push((iou, ti, di));
                }
            }
        }
        pairs.sort_by(|a, b| b.0.total_cmp(&a.0));
        let mut track_taken = vec![false; self.tracks.len()];
        let mut person_taken = vec![false; people.len()];
        for (_, ti, di) in pairs {
            if track_taken[ti] || person_taken[di] {
                continue;
            }
            track_taken[ti] = true;
            person_taken[di] = true;
            let track = &mut self.tracks[ti];
            (track.bbox, track.confidence) = people[di];
            track.hits += 1;
            track.last_seen = now;
            if !track.confirmed && track.hits >= CONFIRM_HITS {
                track.confirmed = true;
                changes.push(TrackChange::Confirmed(track.id));
            }
        }

        // A track not seen this pass is dropped at once until confirmed, then after LOST_AFTER.
        self.tracks.retain(|track| {
            let gone = if track.confirmed {
                now.duration_since(track.last_seen) >= LOST_AFTER
            } else {
                track.last_seen != now
            };
            if gone && track.confirmed {
                changes.push(TrackChange::Lost(track.id));
            }
            !gone
        });

        for (di, &(bbox, confidence)) in people.iter().enumerate() {
            if !person_taken[di] {
                self.next_id += 1;
                self.tracks.push(Track {
                    id: self.next_id,
                    bbox,
                    confidence,
                    confirmed: false,
                    hits: 1,
                    last_seen: now,
                });
            }
        }
        changes
    }

    /// Whether anyone is being followed, confirmed or not; while so, the detector keeps checking.
    pub fn following(&self) -> bool {
        !self.tracks.is_empty()
    }

    pub fn confirmed(&self) -> impl Iterator<Item = &Track> {
        self.tracks.iter().filter(|t| t.confirmed)
    }

    pub fn get(&self, id: u64) -> Option<&Track> {
        self.tracks.iter().find(|t| t.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn person(x: f32, y: f32) -> (FractionBox, f64) {
        (
            FractionBox {
                x,
                y,
                w: 0.2,
                h: 0.6,
            },
            0.8,
        )
    }

    #[test]
    fn a_person_seen_on_two_passes_running_is_confirmed_once() {
        let mut tracker = PersonTracker::default();
        let t0 = Instant::now();
        assert!(tracker.update(&[person(0.1, 0.2)], t0).is_empty());
        let step = Duration::from_millis(200);
        assert_eq!(
            tracker.update(&[person(0.12, 0.2)], t0 + step),
            vec![TrackChange::Confirmed(1)]
        );
        assert!(tracker
            .update(&[person(0.14, 0.2)], t0 + step * 2)
            .is_empty());
        assert_eq!(tracker.confirmed().count(), 1);
        assert!(
            (tracker.get(1).unwrap().bbox.x - 0.14).abs() < 1e-6,
            "the box follows them"
        );
    }

    #[test]
    fn a_single_stray_box_never_counts() {
        let mut tracker = PersonTracker::default();
        let t0 = Instant::now();
        tracker.update(&[person(0.1, 0.2)], t0);
        let changes = tracker.update(&[], t0 + Duration::from_millis(200));
        assert!(changes.is_empty());
        assert!(!tracker.following(), "dropped as soon as it was missed");
    }

    #[test]
    fn two_people_are_two_tracks() {
        let mut tracker = PersonTracker::default();
        let t0 = Instant::now();
        tracker.update(&[person(0.05, 0.2), person(0.7, 0.2)], t0);
        let changes = tracker.update(
            &[person(0.71, 0.2), person(0.06, 0.2)],
            t0 + Duration::from_millis(200),
        );
        assert_eq!(
            changes,
            vec![TrackChange::Confirmed(1), TrackChange::Confirmed(2)]
        );
        assert!(
            (tracker.get(2).unwrap().bbox.x - 0.71).abs() < 1e-6,
            "matched by overlap, not order"
        );
    }

    #[test]
    fn a_person_out_of_sight_is_lost_after_the_grace_and_not_before() {
        let mut tracker = PersonTracker::default();
        let t0 = Instant::now();
        tracker.update(&[person(0.1, 0.2)], t0);
        tracker.update(&[person(0.1, 0.2)], t0 + Duration::from_millis(200));
        let seen = t0 + Duration::from_millis(200);
        assert!(tracker.update(&[], seen + LOST_AFTER / 2).is_empty());
        assert!(tracker.following(), "a missed pass keeps them");
        assert_eq!(
            tracker.update(&[], seen + LOST_AFTER),
            vec![TrackChange::Lost(1)]
        );
        assert!(!tracker.following());
    }

    #[test]
    fn someone_who_returns_after_being_lost_is_a_new_person() {
        let mut tracker = PersonTracker::default();
        let t0 = Instant::now();
        tracker.update(&[person(0.1, 0.2)], t0);
        tracker.update(&[person(0.1, 0.2)], t0 + Duration::from_millis(200));
        tracker.update(&[], t0 + LOST_AFTER * 2);
        tracker.update(&[person(0.1, 0.2)], t0 + LOST_AFTER * 3);
        let changes = tracker.update(
            &[person(0.1, 0.2)],
            t0 + LOST_AFTER * 3 + Duration::from_millis(200),
        );
        assert_eq!(changes, vec![TrackChange::Confirmed(2)]);
    }
}
