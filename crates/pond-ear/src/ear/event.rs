//! What the ear tells the rest of the pond. Sample indices are absolute on the microphone's clock.

use super::State;

#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// The wake detector fired on the wake segment `start..end`. `who` is the id of the print Voice
    /// Match accepted on it; `score` the best raw score (NaN when nobody is enrolled).
    Wake {
        t: f64,
        p: f32,
        start: usize,
        end: usize,
        who: Option<String>,
        score: f32,
    },
    /// The ear changed state.
    State { t: f64, from: State, to: State },
    /// Voice activity turned on or off (reported outside Wait only).
    Speech { t: f64, on: bool },
    /// An utterance ended: `start..end` is what speech-to-text should read, wake phrase included.
    Utterance { t: f64, start: usize, end: usize },
    /// A wake, then no command.
    Silence { t: f64 },
    /// A stretch of speech outside Listen ended, with segments on: `start..end` is for the host's
    /// wake detector to read. `cut`: the segment cap ended it while the speech went on.
    Segment {
        t: f64,
        start: usize,
        end: usize,
        cut: bool,
    },
    /// While the reply plays, someone has spoken over it for the barge-in span, on the
    /// echo-cancelled signal. Whether that interrupts is the host's policy, not the ear's.
    BargeIn {
        t: f64,
        start: usize,
        end: usize,
        who: Option<String>,
        score: f32,
    },
}

impl Event {
    pub fn t(&self) -> f64 {
        match self {
            Event::Wake { t, .. }
            | Event::State { t, .. }
            | Event::Speech { t, .. }
            | Event::Utterance { t, .. }
            | Event::Silence { t }
            | Event::Segment { t, .. }
            | Event::BargeIn { t, .. } => *t,
        }
    }

    /// One JSON object, no trailing newline.
    pub fn json(&self) -> String {
        let who = |w: &Option<String>| w.as_deref().map(quote).unwrap_or_else(|| "null".into());
        match self {
            Event::Wake { t, p, start, end, who: w, score } => format!(
                "{{\"event\":\"wake\",\"t\":{},\"p\":{},\"start\":{start},\"end\":{end},\"who\":{},\"score\":{}}}",
                num(*t),
                num(*p as f64),
                who(w),
                num(*score as f64)
            ),
            Event::State { t, from, to } => format!(
                "{{\"event\":\"state\",\"t\":{},\"from\":\"{}\",\"to\":\"{}\"}}",
                num(*t),
                from.name(),
                to.name()
            ),
            Event::Speech { t, on } => {
                format!("{{\"event\":\"speech\",\"t\":{},\"on\":{on}}}", num(*t))
            }
            Event::Utterance { t, start, end } => format!(
                "{{\"event\":\"utterance\",\"t\":{},\"start\":{start},\"end\":{end}}}",
                num(*t)
            ),
            Event::Silence { t } => format!("{{\"event\":\"silence\",\"t\":{}}}", num(*t)),
            Event::Segment { t, start, end, cut } => format!(
                "{{\"event\":\"segment\",\"t\":{},\"start\":{start},\"end\":{end},\"cut\":{cut}}}",
                num(*t)
            ),
            Event::BargeIn { t, start, end, who: w, score } => format!(
                "{{\"event\":\"barge_in\",\"t\":{},\"start\":{start},\"end\":{end},\"who\":{},\"score\":{}}}",
                num(*t),
                who(w),
                num(*score as f64)
            ),
        }
    }
}

fn num(v: f64) -> String {
    if !v.is_finite() {
        return "null".into();
    }
    let s = format!("{v:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-" || s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

fn quote(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_is_compact_and_escaped() {
        let e = Event::Wake {
            t: 1.25,
            p: 0.5,
            start: 10,
            end: 20,
            who: Some("Ana \"A\"".into()),
            score: f32::NAN,
        };
        assert_eq!(
            e.json(),
            r#"{"event":"wake","t":1.25,"p":0.5,"start":10,"end":20,"who":"Ana \"A\"","score":null}"#
        );
        assert_eq!(
            Event::Silence { t: 2.0 }.json(),
            r#"{"event":"silence","t":2}"#
        );
        assert_eq!(
            Event::Segment {
                t: 3.5,
                start: 100,
                end: 900,
                cut: false
            }
            .json(),
            r#"{"event":"segment","t":3.5,"start":100,"end":900,"cut":false}"#
        );
        assert_eq!(
            Event::Speech {
                t: -0.00001,
                on: true
            }
            .json(),
            r#"{"event":"speech","t":0,"on":true}"#
        );
    }
}
