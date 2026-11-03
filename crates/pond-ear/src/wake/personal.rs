//! A household member's own wake phrase, matched against templates they taught: subsequence DTW over
//! the ear's fbank frames inside a Sakoe-Chiba band, cosine frame distance, normalised by path length.
//! A match must be close to the member's templates and clearly closer than to any non-wake reference
//! (after subvox's per-user word templates).

use crate::engine::fbank::NUM_BINS;

/// A template is cut to this many frames (1.5 s), which bounds what one match costs.
pub const MAX_TEMPLATE_FRAMES: usize = 150;
/// The phrase may start this far into a segment: its 200 ms pre-roll, plus a breath. Seeking it
/// anywhere in a segment's first 6 s found no more wakes in babble on the bench, at nine times the
/// cost, since babble had already spoiled the frames.
pub const START_WINDOW: usize = 60;
/// The band's half-width as a share of the template, so the phrase may be said 40% faster or slower.
pub const BAND: f32 = 0.4;
/// The frames from a segment's start a match can reach.
pub const QUERY_FRAMES: usize = START_WINDOW + MAX_TEMPLATE_FRAMES * 7 / 5;
/// A taught phrase shorter or longer than these was timed wrong, and its template is not kept.
pub const MIN_PHRASE_S: f32 = 0.25;
pub const MAX_PHRASE_S: f32 = 1.5;
/// No threshold is ever looser than this, however few or noisy the takes behind it.
pub const MAX_DISTANCE: f32 = 0.5;
/// Where the threshold sits between the member's worst take and the nearest impostor: nearer the
/// takes, since a false wake costs more than saying the phrase again.
pub const GAP_SHARE: f32 = 0.35;

/// Each frame less its own mean over the bins, scaled to unit length: a log-mel frame's gain is an
/// offset across its bins, so the match ignores microphone gain. Not CMN over the span, which a
/// query's other words would shift away from the template's.
fn normalise(frames: &[f32]) -> Vec<f32> {
    let n = frames.len() / NUM_BINS;
    let mut out = frames[..n * NUM_BINS].to_vec();
    for f in out.chunks_exact_mut(NUM_BINS) {
        let mean = f.iter().sum::<f32>() / NUM_BINS as f32;
        f.iter_mut().for_each(|v| *v -= mean);
        let norm = f.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-6);
        f.iter_mut().for_each(|v| *v /= norm);
    }
    out
}

#[derive(Clone, Debug, PartialEq)]
pub struct Template {
    frames: Vec<f32>,
}

impl Template {
    /// Raw fbank frames (`[frame][bin]`) of the phrase alone.
    pub fn from_fbank(frames: &[f32]) -> Template {
        let n = (frames.len() / NUM_BINS).min(MAX_TEMPLATE_FRAMES);
        Template {
            frames: normalise(&frames[..n * NUM_BINS]),
        }
    }

    /// As stored: already normalised.
    pub fn from_normalised(frames: Vec<f32>) -> Template {
        Template { frames }
    }

    pub fn normalised(&self) -> &[f32] {
        &self.frames
    }

    pub fn len(&self) -> usize {
        self.frames.len() / NUM_BINS
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// A segment's opening, ready to match: the frames a phrase starting in it could span.
#[derive(Clone, Debug)]
pub struct Query {
    frames: Vec<f32>,
}

impl Query {
    /// Raw fbank frames from a segment's start; only what the longest template could reach is kept.
    pub fn from_fbank(frames: &[f32]) -> Query {
        let n = (frames.len() / NUM_BINS).min(QUERY_FRAMES);
        Query {
            frames: normalise(&frames[..n * NUM_BINS]),
        }
    }

    pub fn len(&self) -> usize {
        self.frames.len() / NUM_BINS
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }
}

/// One template's best alignment inside a query.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Match {
    /// Mean cosine distance along the path, 0 (same) to 2.
    pub distance: f32,
    pub start: usize,
    pub end: usize,
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// Subsequence DTW of `t` inside `q`: the path starts in the query's first `START_WINDOW` frames and
/// ends anywhere, keeps within the band around its own diagonal, and weighs a diagonal step twice
/// (symmetric2), so dividing by template plus matched length makes long and short paths comparable.
/// Only cells a banded path can reach are computed.
pub fn align(q: &Query, t: &Template) -> Option<Match> {
    let (nq, nt) = (q.len(), t.len());
    if nq == 0 || nt == 0 {
        return None;
    }
    let band = ((nt as f32 * BAND).ceil() as isize).max(4);
    let inf = f32::INFINITY;
    let mut prev = vec![inf; nq];
    let mut prev_start = vec![0usize; nq];
    let mut cur = vec![inf; nq];
    let mut cur_start = vec![0usize; nq];
    let d = |i: usize, j: usize| {
        1.0 - dot(
            &t.frames[i * NUM_BINS..(i + 1) * NUM_BINS],
            &q.frames[j * NUM_BINS..(j + 1) * NUM_BINS],
        )
    };
    let inside = |i: usize, j: usize, start: usize| -> bool {
        let off = j as isize - start as isize - i as isize;
        j >= start && off.abs() <= band
    };
    let first = nq.min(START_WINDOW);
    for j in 0..first {
        prev[j] = d(0, j);
        prev_start[j] = j;
    }
    for i in 1..nt {
        let lo = (i as isize - band).max(0) as usize;
        let hi = ((first + i) as isize + band).clamp(0, nq as isize) as usize;
        if lo >= hi {
            break;
        }
        cur.fill(inf);
        for j in lo..hi {
            let diag = (j > 0 && prev[j - 1].is_finite() && inside(i, j, prev_start[j - 1]))
                .then(|| (prev[j - 1], prev_start[j - 1]));
            let up = (prev[j].is_finite() && inside(i, j, prev_start[j]))
                .then(|| (prev[j], prev_start[j]));
            let left = (j > 0 && cur[j - 1].is_finite() && inside(i, j, cur_start[j - 1]))
                .then(|| (cur[j - 1], cur_start[j - 1]));
            if diag.is_none() && up.is_none() && left.is_none() {
                continue;
            }
            let dij = d(i, j);
            let (mut best, mut start) = (inf, 0);
            for (cost, from) in [
                diag.map(|(c, s)| (c + 2.0 * dij, s)),
                up.map(|(c, s)| (c + dij, s)),
                left.map(|(c, s)| (c + dij, s)),
            ]
            .into_iter()
            .flatten()
            {
                if cost < best {
                    best = cost;
                    start = from;
                }
            }
            cur[j] = best;
            cur_start[j] = start;
        }
        std::mem::swap(&mut prev, &mut cur);
        std::mem::swap(&mut prev_start, &mut cur_start);
    }
    (0..nq)
        .filter(|&j| prev[j].is_finite())
        .map(|j| Match {
            distance: prev[j] / (nt + j + 1 - prev_start[j]) as f32,
            start: prev_start[j],
            end: j + 1,
        })
        .min_by(|a, b| a.distance.total_cmp(&b.distance))
}

/// The best of many templates.
pub fn best(q: &Query, templates: &[Template]) -> Option<Match> {
    templates
        .iter()
        .filter_map(|t| align(q, t))
        .min_by(|a, b| a.distance.total_cmp(&b.distance))
}

/// The embedded non-wake references: short phrases that sound near the wake ("he goes", "a goose",
/// "hey guys") and ordinary ones, rendered once with Kokoro; provenance in models/wake-references.md.
pub const REFERENCES: &[u8] = include_bytes!("../../models/wake-references.bin");
const REFERENCES_MAGIC: &[u8; 4] = b"PWR1";

/// Normalised frames as the reference file keeps them: each value times 127, as `i8`.
pub fn encode_references(templates: &[Template]) -> Vec<u8> {
    let mut out = REFERENCES_MAGIC.to_vec();
    out.extend((templates.len() as u32).to_le_bytes());
    for t in templates {
        out.extend((t.len() as u32).to_le_bytes());
        out.extend(
            t.frames
                .iter()
                .map(|v| (v * 127.0).round().clamp(-127.0, 127.0) as i8 as u8),
        );
    }
    out
}

/// The references in `bytes` (`encode_references`), each frame back to unit length.
pub fn decode_references(bytes: &[u8]) -> Result<Vec<Template>, String> {
    let u32_at = |i: usize| -> Result<usize, String> {
        bytes
            .get(i..i + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(|| "the reference file ends early".to_string())
    };
    if bytes.get(..4) != Some(&REFERENCES_MAGIC[..]) {
        return Err("not a reference file".into());
    }
    let count = u32_at(4)?;
    let mut at = 8;
    let mut out = Vec::with_capacity(count);
    for _ in 0..count {
        let n = u32_at(at)?;
        at += 4;
        let body = bytes
            .get(at..at + n * NUM_BINS)
            .ok_or("the reference file ends early")?;
        at += n * NUM_BINS;
        let raw: Vec<f32> = body.iter().map(|&b| b as i8 as f32).collect();
        out.push(Template {
            frames: normalise(&raw),
        });
    }
    Ok(out)
}

/// The embedded references, decoded.
pub fn references() -> Vec<Template> {
    decode_references(REFERENCES).unwrap_or_default()
}

/// How close a member's phrase must come, and by how much it must beat every non-wake reference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thresholds {
    pub distance: f32,
    pub margin: f32,
}

#[derive(Clone, Debug)]
pub struct Member {
    pub id: String,
    pub templates: Vec<Template>,
    pub thresholds: Thresholds,
}

/// What one segment's opening matched.
#[derive(Clone, Debug, PartialEq)]
pub struct Verdict {
    /// The member whose phrase it was, when it passed.
    pub accepted: Option<String>,
    /// The nearest member and their distance, passed or not.
    pub nearest: Option<(String, f32)>,
    /// The nearest non-wake reference's distance minus the nearest member's; NaN when the
    /// references were not needed.
    pub margin: f32,
    /// The nearest member's thresholds.
    pub thresholds: Option<Thresholds>,
    /// The query frame the nearest member's phrase ended at.
    pub end: usize,
}

/// The household's wake templates and the non-wake references every match is held against.
#[derive(Clone, Debug, Default)]
pub struct PersonalWake {
    pub members: Vec<Member>,
    pub references: Vec<Template>,
}

impl PersonalWake {
    pub fn is_empty(&self) -> bool {
        self.members.iter().all(|m| m.templates.is_empty())
    }

    /// What the templates and references hold.
    pub fn bytes(&self) -> usize {
        let frames: usize = self
            .members
            .iter()
            .flat_map(|m| &m.templates)
            .chain(&self.references)
            .map(|t| t.frames.len())
            .sum();
        frames * std::mem::size_of::<f32>()
    }

    /// 1-NN over every member's templates, rejected unless close enough and far enough ahead of
    /// the nearest non-wake reference.
    pub fn judge(&self, q: &Query) -> Verdict {
        let nearest = self
            .members
            .iter()
            .filter_map(|m| best(q, &m.templates).map(|b| (m, b)))
            .min_by(|a, b| a.1.distance.total_cmp(&b.1.distance));
        let Some((member, found)) = nearest else {
            return Verdict {
                accepted: None,
                nearest: None,
                margin: f32::NAN,
                thresholds: None,
                end: 0,
            };
        };
        let (distance, t) = (found.distance, member.thresholds);
        let mut verdict = Verdict {
            accepted: None,
            nearest: Some((member.id.clone(), distance)),
            margin: f32::NAN,
            thresholds: Some(t),
            end: found.end,
        };
        // Most segments are other talk: too far from every template, they need no references.
        if distance > t.distance {
            return verdict;
        }
        let reference = best(q, &self.references).map_or(f32::INFINITY, |b| b.distance);
        verdict.margin = reference - distance;
        if verdict.margin >= t.margin {
            verdict.accepted = Some(member.id.clone());
        }
        verdict
    }
}

/// A member's thresholds from their own takes: each take's phrase against the other takes'
/// templates (leave one out) for what a wake scores, and `impostors` (their own non-wake words, a
/// generic negative set) for what speech that is not the phrase scores. Both are queries.
pub fn calibrate(
    takes: &[(Query, Template)],
    impostors: &[Query],
    references: &[Template],
) -> Thresholds {
    let score = |q: &Query, templates: &[Template]| -> Option<(f32, f32)> {
        let d = best(q, templates)?.distance;
        let r = best(q, references).map_or(f32::INFINITY, |b| b.distance);
        Some((d, r - d))
    };
    let all: Vec<Template> = takes.iter().map(|(_, t)| t.clone()).collect();
    let genuine: Vec<(f32, f32)> = (0..takes.len())
        .filter_map(|k| {
            let others: Vec<Template> = all
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != k)
                .map(|(_, t)| t.clone())
                .collect();
            score(&takes[k].0, &others)
        })
        .collect();
    let impostor: Vec<f32> = impostors
        .iter()
        .filter_map(|q| score(q, &all).map(|s| s.0))
        .collect();
    let worst_genuine = genuine.iter().map(|g| g.0).fold(0.0f32, f32::max);
    let best_impostor = impostor.iter().copied().fold(f32::INFINITY, f32::min);
    // Between the two when they are apart; when they overlap, no impostor is let through.
    let distance = if worst_genuine < best_impostor {
        if best_impostor.is_finite() {
            worst_genuine + (best_impostor - worst_genuine) * GAP_SHARE
        } else {
            worst_genuine * 1.2
        }
    } else {
        best_impostor * 0.98
    }
    .min(MAX_DISTANCE);
    let least_margin = genuine
        .iter()
        .map(|g| g.1)
        .filter(|m| m.is_finite())
        .fold(f32::INFINITY, f32::min);
    let margin = if least_margin.is_finite() {
        (least_margin / 2.0).max(0.0)
    } else {
        0.0
    };
    Thresholds { distance, margin }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word-like run of frames: each of `units` sounds held for `hold` frames, with a pinch of
    /// seeded noise so no two renditions are identical.
    fn word(units: &[usize], hold: usize, seed: u64) -> Vec<f32> {
        let mut s = seed.max(1);
        let mut rnd = || {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            (s as f64 / u64::MAX as f64) as f32 - 0.5
        };
        let mut out = Vec::new();
        for &u in units {
            for _ in 0..hold {
                for b in 0..NUM_BINS {
                    let shape = ((b as f32 * 0.21 * (u as f32 + 1.0)).sin() * 3.0) + u as f32;
                    out.push(shape + 0.3 * rnd());
                }
            }
        }
        out
    }

    const HEY_GOOSE: &[usize] = &[1, 4, 2, 7, 3];
    const HE_GOES: &[usize] = &[1, 5, 2, 6, 0];

    fn take(seed: u64, hold: usize) -> Vec<f32> {
        word(HEY_GOOSE, hold, seed)
    }

    #[test]
    fn a_phrase_is_found_after_the_segment_start_whatever_its_pace() {
        let t = Template::from_fbank(&take(1, 10));
        for (hold, lead) in [(10, 0), (8, 20), (13, 40)] {
            let mut q = word(&[9, 9], lead / 2, 5);
            q.extend(take(7 + hold as u64, hold));
            q.extend(word(&[8, 6, 5], 10, 9));
            let m = align(&Query::from_fbank(&q), &t).unwrap();
            assert!(m.distance < 0.1, "pace {hold}, lead {lead}: {m:?}");
            assert!(m.end > lead, "{m:?}");
        }
    }

    #[test]
    fn another_phrase_scores_far_and_a_late_one_is_not_sought() {
        let t = Template::from_fbank(&take(1, 10));
        let other = align(&Query::from_fbank(&word(HE_GOES, 10, 3)), &t).unwrap();
        let same = align(&Query::from_fbank(&take(2, 10)), &t).unwrap();
        assert!(
            other.distance > same.distance * 3.0,
            "{other:?} vs {same:?}"
        );

        let mut late = word(&[9, 8, 6, 5, 4, 3, 2], 12, 4);
        late.extend(take(3, 10));
        let m = align(&Query::from_fbank(&late), &t);
        assert!(m.is_none_or(|m| m.distance > same.distance * 3.0), "{m:?}");
    }

    #[test]
    fn calibrated_thresholds_accept_the_member_and_reject_look_alikes() {
        let takes: Vec<(Query, Template)> = (0..5)
            .map(|k| {
                let f = take(10 + k, 9 + k as usize % 3);
                (Query::from_fbank(&f), Template::from_fbank(&f))
            })
            .collect();
        let references = vec![Template::from_fbank(&word(HE_GOES, 10, 77))];
        let impostors: Vec<Query> = (0..6)
            .map(|k| Query::from_fbank(&word(&[k % 5, 6, 8, 2], 10, 40 + k as u64)))
            .collect();
        let thresholds = calibrate(&takes, &impostors, &references);
        assert!(thresholds.distance > 0.0 && thresholds.distance <= MAX_DISTANCE);
        let house = PersonalWake {
            members: vec![Member {
                id: "ana".into(),
                templates: takes.into_iter().map(|(_, t)| t).collect(),
                thresholds,
            }],
            references,
        };
        let v = house.judge(&Query::from_fbank(&take(99, 11)));
        assert_eq!(v.accepted.as_deref(), Some("ana"), "{v:?}");
        let v = house.judge(&Query::from_fbank(&word(HE_GOES, 10, 98)));
        assert_eq!(v.accepted, None, "{v:?}");
        let v = house.judge(&Query::from_fbank(&word(&[8, 8, 3, 1], 12, 97)));
        assert_eq!(v.accepted, None, "{v:?}");
    }

    #[test]
    fn nobody_taught_and_empty_audio_decide_nothing() {
        let house = PersonalWake::default();
        assert!(house.is_empty());
        let v = house.judge(&Query::from_fbank(&take(1, 10)));
        assert_eq!(v.accepted, None);
        assert!(align(&Query::from_fbank(&[]), &Template::from_fbank(&take(1, 10))).is_none());
    }

    #[test]
    fn the_embedded_references_load_and_survive_their_quantisation() {
        let refs = references();
        assert!(refs.len() >= 12, "{} references", refs.len());
        assert!(refs
            .iter()
            .all(|t| !t.is_empty() && t.len() <= MAX_TEMPLATE_FRAMES));
        let t = Template::from_fbank(&take(4, 10));
        let back = decode_references(&encode_references(std::slice::from_ref(&t))).unwrap();
        let q = Query::from_fbank(&take(5, 10));
        let (a, b) = (align(&q, &t).unwrap(), align(&q, &back[0]).unwrap());
        assert!((a.distance - b.distance).abs() < 0.01, "{a:?} {b:?}");
        assert!(decode_references(b"PWR1\x05\0\0\0").is_err());
    }
}
