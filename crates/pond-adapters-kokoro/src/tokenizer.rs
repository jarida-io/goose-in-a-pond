//! Text -> espeak IPA -> Kokoro token ids. Kokoro was trained on misaki G2P; espeak IPA mostly
//! fits its vocab, and [`Vocab::encode`] drops and counts whatever doesn't.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;

/// espeak-ng keeps process-wide state and is not thread-safe: concurrent phonemizing segfaults
/// (after `N_VOICES_LIST` warnings). Every espeak call in this crate takes this lock.
static ESPEAK: Mutex<()> = Mutex::new(());

/// Kokoro's hard context limit, including the pad token at each end.
pub const MAX_CONTEXT: usize = 512;
/// Usable phoneme tokens per forward pass.
pub const MAX_PHONEME_TOKENS: usize = MAX_CONTEXT - 2;
/// Pad / boundary token id. Wraps every sequence.
pub const PAD: i64 = 0;

/// Kokoro's phoneme→id table, read from the model repo's `tokenizer.json`.
#[derive(Debug, Clone)]
pub struct Vocab {
    map: HashMap<char, i64>,
}

/// One chunk of tokenized text, small enough for a single forward pass.
#[derive(Debug, Clone, PartialEq)]
pub struct Chunk {
    /// The source text this chunk came from, for logging and per-sentence UI.
    pub text: String,
    /// Phonemes actually encoded (after the vocab filter).
    pub phonemes: String,
    /// Token ids WITHOUT the surrounding pad tokens.
    pub tokens: Vec<i64>,
}

impl Chunk {
    /// The full input sequence: pad, tokens, pad.
    pub fn padded(&self) -> Vec<i64> {
        let mut v = Vec::with_capacity(self.tokens.len() + 2);
        v.push(PAD);
        v.extend_from_slice(&self.tokens);
        v.push(PAD);
        v
    }
}

impl Vocab {
    /// Load from a `tokenizer.json` as shipped in the Kokoro ONNX repo.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read Kokoro tokenizer at {}", path.display()))?;
        Self::from_json(&raw)
    }

    /// Parse `{"model": {"vocab": {"<sym>": id, …}}}`.
    pub fn from_json(raw: &str) -> Result<Self> {
        let v: serde_json::Value =
            serde_json::from_str(raw).context("Kokoro tokenizer.json is not valid JSON")?;
        let obj = v
            .get("model")
            .and_then(|m| m.get("vocab"))
            .and_then(|x| x.as_object())
            .ok_or_else(|| anyhow!("Kokoro tokenizer.json has no model.vocab object"))?;

        let mut map = HashMap::with_capacity(obj.len());
        for (sym, id) in obj {
            // Entries are single chars; a multi-char one could never match, so it is skipped.
            let mut chars = sym.chars();
            let (Some(c), None) = (chars.next(), chars.next()) else {
                continue;
            };
            let id = id
                .as_i64()
                .ok_or_else(|| anyhow!("Kokoro vocab id for {sym:?} is not an integer"))?;
            map.insert(c, id);
        }
        if map.is_empty() {
            return Err(anyhow!("Kokoro vocab parsed to zero usable symbols"));
        }
        Ok(Self { map })
    }

    pub fn len(&self) -> usize {
        self.map.len()
    }

    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }

    pub fn contains(&self, c: char) -> bool {
        self.map.contains_key(&c)
    }

    /// Encode phonemes, dropping and counting anything outside the vocab. On English the count
    /// should be zero; non-zero means espeak emits something the model never learned.
    pub fn encode(&self, phonemes: &str) -> (Vec<i64>, String, usize) {
        let mut ids = Vec::with_capacity(phonemes.len());
        let mut kept = String::with_capacity(phonemes.len());
        let mut dropped = 0usize;
        for c in phonemes.chars() {
            match self.map.get(&c) {
                Some(&id) => {
                    ids.push(id);
                    kept.push(c);
                }
                None => dropped += 1,
            }
        }
        (ids, kept, dropped)
    }
}

/// Punctuation in Kokoro's vocab, kept because misaki (its reference G2P) keeps it as prosody.
/// `$` is omitted: `normalize_for_speech` spells out currency first.
const PROSODY_PUNCT: &[char] = &['.', ',', '!', '?', ';', ':', '"', '(', ')'];

/// Phonemize `text`, keeping the punctuation espeak throws away.
///
/// ## What espeak actually does
///
/// `espeak_rs::text_to_phonemes` returns **one** string with every clause
/// concatenated, no punctuation and — the part that matters more — no
/// separator at all:
///
/// ```text
/// "Hello, world! Are you sure?"  ->  ["həlˈoʊwˈɜːldɑːɹ juː ʃˈʊɹ"]
/// ```
///
/// `həlˈoʊwˈɜːld` is "hello" and "world" fused into one word. So the old code
/// was not merely losing prosody, it was handing Kokoro a different sentence
/// from the one it was given, at every clause boundary in every utterance.
///
/// (The docstring this replaces claimed espeak returned one string per
/// sentence and that the streaming path could rely on it as a sentence split.
/// It returns one element regardless of input. Nothing downstream depended on
/// the claim — `split_sentences` in `pond-voice` had already done the real
/// split — but it is worth naming, because it is the reason nobody looked
/// here.)
///
/// ## What this does instead
///
/// Cut the source into runs of speech and runs of punctuation, phonemize the
/// speech runs one at a time, and put the punctuation back between them.
/// espeak never sees a clause boundary, so it has nothing to swallow.
pub fn phonemize(text: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len() * 2);

    for segment in segments(text) {
        match segment {
            Segment::Speech {
                text: run,
                leading_space,
            } => {
                let spoken = {
                    // Ignore poisoning: the lock guards no data of ours.
                    let _guard = ESPEAK.lock().unwrap_or_else(|e| e.into_inner());
                    espeak_rs::text_to_phonemes(run, "en-us", None)
                        .map_err(|e| anyhow!("espeak phonemization failed: {e}"))?
                }
                .join(" ");
                let spoken = spoken.trim();
                if spoken.is_empty() {
                    continue;
                }
                // espeak trims, so restore the source's space between runs or the words fuse.
                if leading_space && !out.is_empty() && !out.ends_with(' ') {
                    out.push(' ');
                }
                out.push_str(spoken);
            }
            Segment::Punct(c) => out.push(c),
        }
    }

    Ok(out)
}

/// One run of the source: speech to be phonemized, or a mark to be kept.
enum Segment<'a> {
    Speech { text: &'a str, leading_space: bool },
    Punct(char),
}

/// Split into speech and `PROSODY_PUNCT` runs; apostrophes and hyphens stay in the word.
fn segments(text: &str) -> Vec<Segment<'_>> {
    let mut out = Vec::new();
    let mut run_start: Option<usize> = None;
    let mut leading_space = false;

    for (i, c) in text.char_indices() {
        if PROSODY_PUNCT.contains(&c) {
            if let Some(start) = run_start.take() {
                out.push(Segment::Speech {
                    text: &text[start..i],
                    leading_space,
                });
            }
            out.push(Segment::Punct(c));
            // Whitespace after the mark, if any, sets this again below.
            leading_space = false;
        } else if run_start.is_none() {
            if c.is_whitespace() {
                leading_space = true;
            } else {
                run_start = Some(i);
            }
        }
    }

    if let Some(start) = run_start {
        out.push(Segment::Speech {
            text: &text[start..],
            leading_space,
        });
    }
    out
}

/// Tokenize `text` into chunks of at most [`MAX_PHONEME_TOKENS`], split at phoneme spaces.
pub fn chunk(text: &str, vocab: &Vocab) -> Result<(Vec<Chunk>, usize)> {
    let mut out = Vec::new();

    let phonemized = phonemize(text)?;
    let (ids, kept, dropped) = vocab.encode(&phonemized);
    if ids.is_empty() {
        return Ok((out, dropped));
    }

    for (tokens, phonemes) in split_to_limit(&ids, &kept) {
        out.push(Chunk {
            // The whole input: a token-count split has no matching source substring.
            text: text.to_string(),
            phonemes,
            tokens,
        });
    }
    Ok((out, dropped))
}

/// Split at the last space before the limit. `encode` keeps `ids` and `phonemes` index-aligned.
fn split_to_limit(ids: &[i64], phonemes: &str) -> Vec<(Vec<i64>, String)> {
    let chars: Vec<char> = phonemes.chars().collect();
    debug_assert_eq!(chars.len(), ids.len());

    if ids.len() <= MAX_PHONEME_TOKENS {
        return vec![(ids.to_vec(), phonemes.to_string())];
    }

    let mut out = Vec::new();
    let mut start = 0usize;
    while start < ids.len() {
        let hard_end = (start + MAX_PHONEME_TOKENS).min(ids.len());
        // Prefer a word boundary; hard-cut a "word" that fills the whole window.
        let end = if hard_end == ids.len() {
            hard_end
        } else {
            chars[start..hard_end]
                .iter()
                .rposition(|c| *c == ' ')
                .map(|rel| start + rel + 1)
                .filter(|e| *e > start)
                .unwrap_or(hard_end)
        };
        out.push((
            ids[start..end].to_vec(),
            chars[start..end].iter().collect::<String>(),
        ));
        start = end;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny vocab in the shipped `tokenizer.json` shape.
    fn vocab() -> Vocab {
        Vocab::from_json(
            r#"{"model":{"vocab":{"$":0," ":16,"a":43,"b":44,"ð":81,"ə":82,"ˈ":156}}}"#,
        )
        .unwrap()
    }

    #[test]
    fn parses_the_repo_tokenizer_shape() {
        let v = vocab();
        assert_eq!(v.len(), 7);
        assert!(v.contains('ð'));
        assert!(!v.contains('ʣ'));
    }

    #[test]
    fn rejects_a_vocab_it_cannot_use() {
        assert!(Vocab::from_json(r#"{"model":{"vocab":{}}}"#).is_err());
        assert!(Vocab::from_json(r#"{"nope":1}"#).is_err());
        assert!(Vocab::from_json("not json").is_err());
    }

    #[test]
    fn encode_keeps_known_symbols_and_counts_the_rest() {
        let (ids, kept, dropped) = vocab().encode("ðəb ʒʒ");
        assert_eq!(ids, vec![81, 82, 44, 16]);
        assert_eq!(kept, "ðəb ");
        assert_eq!(dropped, 2, "the two ʒ are outside this vocab");
    }

    #[test]
    fn encode_reports_zero_drops_when_everything_is_known() {
        let (_, _, dropped) = vocab().encode("ðəbaˈ");
        assert_eq!(dropped, 0);
    }

    #[test]
    fn padding_wraps_the_sequence() {
        let c = Chunk {
            text: "hi".into(),
            phonemes: "ab".into(),
            tokens: vec![43, 44],
        };
        assert_eq!(c.padded(), vec![PAD, 43, 44, PAD]);
    }

    #[test]
    fn short_input_is_one_chunk() {
        let ids: Vec<i64> = (0..10).collect();
        let out = split_to_limit(&ids, &"a".repeat(10));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0.len(), 10);
    }

    #[test]
    fn over_long_input_splits_at_word_boundaries() {
        // 200 three-char "words" separated by spaces = 800 chars, well over 510.
        let word = "aba ";
        let phonemes = word.repeat(200);
        let ids: Vec<i64> = (0..phonemes.chars().count() as i64).collect();

        let out = split_to_limit(&ids, &phonemes);
        assert!(out.len() > 1, "should have split");
        for (tokens, text) in &out {
            assert!(
                tokens.len() <= MAX_PHONEME_TOKENS,
                "chunk of {} exceeds the {MAX_PHONEME_TOKENS} limit",
                tokens.len()
            );
            assert_eq!(tokens.len(), text.chars().count(), "ids and text drifted");
        }
        let total: usize = out.iter().map(|(t, _)| t.len()).sum();
        assert_eq!(total, ids.len());
        let rejoined: String = out.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(rejoined, phonemes);
    }

    #[test]
    fn unbroken_run_falls_back_to_a_hard_cut() {
        let phonemes = "a".repeat(MAX_PHONEME_TOKENS * 2 + 7);
        let ids: Vec<i64> = (0..phonemes.len() as i64).collect();
        let out = split_to_limit(&ids, &phonemes);
        assert_eq!(out.len(), 3);
        assert!(out.iter().all(|(t, _)| t.len() <= MAX_PHONEME_TOKENS));
        assert_eq!(out.iter().map(|(t, _)| t.len()).sum::<usize>(), ids.len());
    }

    // ── Punctuation ───────────────────────────────────────────────────────────

    #[test]
    fn punctuation_reaches_the_model() {
        let out = phonemize("Hello, world! Are you sure? Yes; really.").unwrap();
        for mark in ['.', ',', '!', '?', ';'] {
            assert!(out.contains(mark), "{mark:?} missing from {out:?}");
        }
    }

    #[test]
    fn words_either_side_of_a_mark_stay_separate() {
        let out = phonemize("Hello, world!").unwrap();
        let (before, after) = out.split_once(',').expect("comma survived");
        assert!(!before.is_empty(), "nothing before the comma in {out:?}");
        assert!(
            after.starts_with(' '),
            "no space after the comma, words will fuse: {out:?}"
        );
    }

    #[test]
    fn quotes_and_parens_survive_without_fusing_their_neighbours() {
        let quoted = phonemize("She said \"stop\" and left.").unwrap();
        assert_eq!(quoted.matches('"').count(), 2, "{quoted:?}");
        assert!(quoted.ends_with('.'));

        let parens = phonemize("One (two) three.").unwrap();
        assert!(parens.contains('(') && parens.contains(')'), "{parens:?}");
        // "one" and "two" must not have fused across the bracket.
        let inner = parens
            .split_once('(')
            .and_then(|(_, r)| r.split_once(')'))
            .map(|(inner, _)| inner.to_string())
            .expect("bracketed run");
        assert!(!inner.trim().is_empty(), "bracket swallowed its contents");
    }

    #[test]
    fn leading_and_repeated_marks_do_not_produce_stray_spaces() {
        let out = phonemize("Wait... what?").unwrap();
        assert!(!out.starts_with(' '), "leading space in {out:?}");
        assert!(out.contains("..."), "ellipsis collapsed: {out:?}");
        assert!(out.ends_with('?'));
        assert!(!out.contains("  "), "double space in {out:?}");
    }

    // Whether every PROSODY_PUNCT mark is in the real vocab is tested in `tests/live_synthesis.rs`;
    // `vocab_for` adds them, so it can't answer that.

    #[test]
    fn preserved_punctuation_is_not_counted_as_dropped() {
        let sample = "hˈɛloʊ, wˈɜːld!";
        let v = vocab_for(&[sample]);
        let (_, kept, dropped) = v.encode(sample);
        assert_eq!(dropped, 0, "kept {kept:?}");
        assert!(kept.contains(','));
        assert!(kept.ends_with('!'));
    }

    #[test]
    fn a_contraction_keeps_its_apostrophe_inside_the_word() {
        let out = phonemize("Don't stop.").unwrap();
        assert!(
            !out.contains('\''),
            "apostrophe leaked into phonemes: {out:?}"
        );
        assert!(out.ends_with('.'));
        // One word, not two runs fused or split: "doʊnt" stays whole.
        assert!(out.split(' ').count() >= 2, "{out:?}");
    }

    #[test]
    fn text_with_no_punctuation_is_unchanged_in_shape() {
        let out = phonemize("no punctuation here").unwrap();
        assert!(!out.is_empty());
        assert!(out.split(' ').count() >= 3, "words ran together: {out:?}");
    }

    #[test]
    fn a_chunk_carries_the_source_text_not_its_phonemes() {
        let v = vocab_for(&[&phonemize("Hello, world!").unwrap()]);
        let (chunks, _) = chunk("Hello, world!", &v).unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].text, "Hello, world!");
        assert_ne!(chunks[0].text, chunks[0].phonemes);
    }

    /// A vocab of exactly the symbols in `samples`, plus every `PROSODY_PUNCT` mark.
    fn vocab_for(samples: &[&str]) -> Vocab {
        let mut symbols: Vec<char> = samples.iter().flat_map(|s| s.chars()).collect();
        symbols.extend_from_slice(PROSODY_PUNCT);
        symbols.sort_unstable();
        symbols.dedup();

        let entries: Vec<String> = symbols
            .iter()
            .enumerate()
            .map(|(i, c)| {
                format!(
                    "{}: {}",
                    serde_json::to_string(&c.to_string()).unwrap(),
                    i + 1
                )
            })
            .collect();
        Vocab::from_json(&format!(
            "{{\"model\":{{\"vocab\":{{{}}}}}}}",
            entries.join(",")
        ))
        .expect("test vocab")
    }
}
