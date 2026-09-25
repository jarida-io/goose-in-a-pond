//! Reading a GGUF file's own account of itself.
//!
//! A model dropped into the models folder by hand arrives with nothing but a
//! filename. The catalogue has slots for what it is — architecture, context
//! window, quantisation, parameter count — and every one of them was `None`,
//! so the page showed "(detected on disk)" beside a size and stopped.
//!
//! All of that is in the file. GGUF opens with a key/value header describing
//! the model, and it is the first thing in the file, so answering these
//! questions costs one short read rather than loading several gigabytes.
//!
//! # Shape of the header
//!
//! ```text
//! magic "GGUF"   u32
//! version        u32
//! tensor_count   u64
//! kv_count       u64
//! kv_count × { key: string, type: u32, value: <type> }
//! ```
//!
//! Strings are a `u64` length followed by that many bytes. Arrays are an
//! element type, a `u64` count, then the elements. Everything is
//! little-endian.
//!
//! This parser is deliberately total: every read is bounds-checked and any
//! malformed field ends the walk and returns what was understood so far. A
//! file in the models folder is arbitrary bytes from the internet, and the
//! worst outcome of a truncated or hostile header should be a card with less
//! on it — never a panic in a filesystem scan.

/// What a GGUF file says about itself. Every field is optional: headers vary
/// by architecture and by the tool that wrote them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GgufInfo {
    /// `general.architecture` — "llama", "gemma3", "qwen3", …
    pub architecture: Option<String>,
    /// `general.name` — the name the publisher gave it.
    pub name: Option<String>,
    /// Quantisation, resolved from `general.file_type`. "Q4_K_M", "F16", …
    pub quantization: Option<String>,
    /// `{arch}.context_length` — the window the weights were trained for.
    pub context_length: Option<u32>,
    /// `{arch}.embedding_length` — embedding width.
    pub embedding_length: Option<u32>,
    /// `{arch}.block_count` — transformer layers.
    pub block_count: Option<u32>,
    /// `general.parameter_count`, when the writer recorded it.
    pub parameter_count: Option<u64>,
    /// Tensors in the file. Always present in a well-formed header.
    pub tensor_count: Option<u64>,

    // ── Attention geometry, for KV-cache arithmetic ─────────────────────────
    //
    // These sit in the first ~2 KB of every file measured (offsets 924-1,829
    // on gemma-4-E2B), i.e. long before `tokenizer.ggml.tokens`, so a short
    // head read reaches all of them.
    /// `{arch}.attention.head_count_kv` — KV heads, the multiplier on cache size.
    pub head_count_kv: Option<u32>,
    /// `{arch}.attention.key_length` — K width per head, full-attention layers.
    pub key_length: Option<u32>,
    /// `{arch}.attention.value_length` — V width per head, full-attention layers.
    pub value_length: Option<u32>,
    /// `{arch}.attention.key_length_swa` — K width on sliding-window layers,
    /// where present. Gemma 4 halves it (256 against 512).
    pub key_length_swa: Option<u32>,
    /// `{arch}.attention.value_length_swa`, where present.
    pub value_length_swa: Option<u32>,
    /// `tokenizer.chat_template` — the model's own Jinja template, and the
    /// only ground truth about what it can be asked to do. Whether it renders
    /// tool declarations, and whether it gates reasoning, are properties of
    /// this string and of nothing in the filename.
    ///
    /// Only populated by a walk that reaches it: it sits 3.8-15 MB into the
    /// files measured, after `tokenizer.ggml.tokens`. `parse_gguf_header` on a
    /// short slice leaves it `None`; [`parse_gguf_file`] finds it.
    pub chat_template: Option<String>,
    /// `{arch}.attention.shared_kv_layers` — layers that share another layer's
    /// KV and therefore allocate none of their own. Gemma 4 E4B shares 18 of
    /// 42; E2B shares 20 of 35.
    pub shared_kv_layers: Option<u32>,
}

impl GgufInfo {
    /// Did the header yield anything worth showing?
    pub fn is_empty(&self) -> bool {
        *self == GgufInfo::default()
    }

    /// Bytes of KV cache this model needs per token of context, computed from
    /// its own header.
    ///
    /// # Why this is worth having
    ///
    /// `jetson_context_size` divides the memory budget by a per-token KV cost
    /// to decide a context window, and that constant carries a comment saying
    /// it "moves on a measurement from the Orin and nothing less" -- because
    /// getting it wrong OOM-killed the board once, and because a figure
    /// measured on a Mac understated the real cost by roughly three times.
    ///
    /// It does not have to be measured. It is arithmetic over four keys that
    /// sit in the first two kilobytes of the file, and it reproduces both
    /// device measurements exactly (see the tests): E2B 18 KiB/token, E4B 56.
    /// That turns "measure every new model on the hardware or risk the board"
    /// into something answerable before the weights are read.
    ///
    /// # The shape of the sum
    ///
    /// Only layers that own KV allocate any: `block_count - shared_kv_layers`.
    /// Of those, sliding-window layers use the narrower `*_swa` widths where
    /// the architecture declares them. Each layer stores K and V for every KV
    /// head at two bytes an element (f16, the default cache type).
    ///
    /// # The part that is inferred rather than read
    ///
    /// The split between full-attention and sliding-window layers is **not**
    /// in these headers. Both Gemma 4 models measured 1 global to 5 SWA
    /// (E4B 4+20 of 24, E2B 3+12 of 15), and that ratio is assumed here via
    /// `swa_per_global`. An architecture with a different pattern needs its own
    /// value, so this returns `None` rather than guessing when the widths that
    /// would make the answer wrong are absent.
    ///
    /// Returns `None` when the header lacks what the sum needs -- callers keep
    /// their conservative fallback rather than receiving a confident wrong
    /// number.
    pub fn kv_bytes_per_token(&self, swa_per_global: u32) -> Option<u64> {
        const BYTES_PER_ELEMENT: u64 = 2; // f16 cache

        let blocks = self.block_count?;
        let kv_heads = u64::from(self.head_count_kv?);
        let k = u64::from(self.key_length?);
        let v = u64::from(self.value_length?);

        let owning = blocks.saturating_sub(self.shared_kv_layers.unwrap_or(0));
        if owning == 0 || kv_heads == 0 {
            return None;
        }

        // No SWA widths declared: every owning layer pays the full width.
        let (Some(k_swa), Some(v_swa)) = (
            self.key_length_swa.or(self.key_length),
            self.value_length_swa.or(self.value_length),
        ) else {
            return Some(u64::from(owning) * (k + v) * kv_heads * BYTES_PER_ELEMENT);
        };

        let group = swa_per_global.saturating_add(1);
        let (global, swa) = if group <= 1 || self.key_length_swa.is_none() {
            (owning, 0)
        } else {
            let g = owning.div_ceil(group);
            (g, owning.saturating_sub(g))
        };

        let per_global = (k + v) * kv_heads * BYTES_PER_ELEMENT;
        let per_swa = (u64::from(k_swa) + u64::from(v_swa)) * kv_heads * BYTES_PER_ELEMENT;
        Some(u64::from(global) * per_global + u64::from(swa) * per_swa)
    }

    /// [`Self::kv_bytes_per_token`] in KiB, which is the unit the context
    /// arithmetic actually works in.
    pub fn kv_kib_per_token(&self, swa_per_global: u32) -> Option<u64> {
        self.kv_bytes_per_token(swa_per_global).map(|b| b / 1024)
    }

    /// A one-line description, in the order a person reads a model name.
    ///
    /// "Gemma3 · 4.3B · Q4_K_M · 8192 ctx". Parts that the header did not
    /// carry are simply absent rather than filled with "unknown" — a
    /// description made mostly of the word unknown is worse than a short one.
    pub fn summary(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        if let Some(a) = &self.architecture {
            parts.push(title_case(a));
        }
        if let Some(p) = self.parameter_count {
            parts.push(format_params(p));
        }
        if let Some(q) = &self.quantization {
            parts.push(q.clone());
        }
        if let Some(c) = self.context_length {
            parts.push(format!("{} ctx", format_thousands(c)));
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join(" · "))
        }
    }
}

/// `general.file_type` → the quantisation name people recognise.
///
/// The numbering is llama.cpp's `LLAMA_FTYPE_*`. Unknown values return `None`
/// rather than a guess: a wrong quantisation label is worse than no label,
/// because it is the number people use to predict speed and quality.
fn file_type_name(v: u32) -> Option<&'static str> {
    Some(match v {
        0 => "F32",
        1 => "F16",
        2 => "Q4_0",
        3 => "Q4_1",
        7 => "Q8_0",
        8 => "Q5_0",
        9 => "Q5_1",
        10 => "Q2_K",
        11 => "Q3_K_S",
        12 => "Q3_K_M",
        13 => "Q3_K_L",
        14 => "Q4_K_S",
        15 => "Q4_K_M",
        16 => "Q5_K_S",
        17 => "Q5_K_M",
        18 => "Q6_K",
        19 => "IQ2_XXS",
        20 => "IQ2_XS",
        21 => "Q2_K_S",
        22 => "IQ3_XS",
        23 => "IQ3_XXS",
        24 => "IQ1_S",
        25 => "IQ4_NL",
        26 => "IQ3_S",
        27 => "IQ3_M",
        28 => "IQ2_S",
        29 => "IQ2_M",
        30 => "IQ4_XS",
        31 => "IQ1_M",
        32 => "BF16",
        _ => return None,
    })
}

/// Where a GGUF walk gets its bytes.
///
/// The parser used to take a `&[u8]`, which is fine for the geometry keys —
/// they sit in the first two kilobytes — and useless for the chat template,
/// which sits **3.8 to 15 MB in**, immediately after `tokenizer.ggml.tokens`.
///
/// It is tempting to reach it by computing where the token array ends and
/// reading from there. That does not work: the array is variable-length
/// strings, so the only way past it is to walk its per-element length
/// prefixes. There is no offset to seek to.
///
/// What a source buys instead is that **skipping stops requiring the bytes**.
/// A scalar or a whole string is stepped over with position arithmetic and no
/// read at all, and a string array costs one 8-byte length read per element
/// rather than materialising a `String` for each of a quarter-million tokens.
pub trait GgufSource {
    /// Fill `out` from `offset`. `false` if the source cannot satisfy it in
    /// full — a short read is indistinguishable from truncation here, and both
    /// end the walk.
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> bool;
}

impl GgufSource for &[u8] {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> bool {
        let Ok(start) = usize::try_from(offset) else {
            return false;
        };
        let Some(end) = start.checked_add(out.len()) else {
            return false;
        };
        match self.get(start..end) {
            Some(src) => {
                out.copy_from_slice(src);
                true
            }
            None => false,
        }
    }
}

/// A file read through a small sliding window.
///
/// The walk is essentially sequential, so a modest buffer turns a quarter of a
/// million 8-byte length reads into a few hundred real ones.
pub struct FileSource {
    file: std::fs::File,
    buf: Vec<u8>,
    /// Absolute offset of `buf[0]`.
    base: u64,
    /// Valid bytes in `buf`.
    len: usize,
}

impl FileSource {
    const WINDOW: usize = 64 * 1024;

    /// Open a file for walking. `None` if it cannot be opened at all.
    pub fn open(path: &std::path::Path) -> Option<Self> {
        Some(Self {
            file: std::fs::File::open(path).ok()?,
            buf: vec![0u8; Self::WINDOW],
            base: 0,
            len: 0,
        })
    }

    fn refill(&mut self, offset: u64) -> bool {
        use std::io::{Read as _, Seek as _, SeekFrom};
        if self.file.seek(SeekFrom::Start(offset)).is_err() {
            return false;
        }
        let mut filled = 0;
        while filled < self.buf.len() {
            match self.file.read(&mut self.buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return false,
            }
        }
        self.base = offset;
        self.len = filled;
        filled > 0
    }
}

impl GgufSource for FileSource {
    fn read_at(&mut self, offset: u64, out: &mut [u8]) -> bool {
        use std::io::{Read as _, Seek as _, SeekFrom};
        if out.len() > Self::WINDOW {
            return self.file.seek(SeekFrom::Start(offset)).is_ok()
                && self.file.read_exact(out).is_ok();
        }
        let end = match offset.checked_add(out.len() as u64) {
            Some(e) => e,
            None => return false,
        };
        let hit = offset >= self.base && end <= self.base + self.len as u64;
        if !hit && !self.refill(offset) {
            return false;
        }
        let Ok(start) = usize::try_from(offset.saturating_sub(self.base)) else {
            return false;
        };
        let Some(src) = self.buf.get(start..start.saturating_add(out.len())) else {
            return false;
        };
        out.copy_from_slice(src);
        true
    }
}

/// A cursor that refuses to read past the end.
struct Reader<S> {
    src: S,
    pos: u64,
}

/// The widest string this parser will materialise.
///
/// A length field is 64 bits wide and comes from the file; refusing an absurd
/// one is what stops a corrupt header asking for a 16 EB allocation. Chat
/// templates run to about 19 KB, so 4 MiB is generous cover.
const MAX_STRING_BYTES: u64 = 4 * 1024 * 1024;

impl<S: GgufSource> Reader<S> {
    fn take(&mut self, n: usize) -> Option<Vec<u8>> {
        let mut out = vec![0u8; n];
        if !self.src.read_at(self.pos, &mut out) {
            return None;
        }
        self.pos = self.pos.checked_add(n as u64)?;
        Some(out)
    }
    /// Step over `n` bytes without reading them. This is the point of the
    /// source abstraction: the token array costs position arithmetic.
    fn skip(&mut self, n: u64) -> Option<()> {
        self.pos = self.pos.checked_add(n)?;
        Some(())
    }
    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.take(4)?.try_into().ok()?))
    }
    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }
    fn string(&mut self) -> Option<String> {
        let len = self.u64()?;
        if len > MAX_STRING_BYTES {
            return None;
        }
        let bytes = self.take(usize::try_from(len).ok()?)?;
        Some(String::from_utf8_lossy(&bytes).into_owned())
    }
    /// A string we do not want: read its length, step over its bytes.
    fn skip_string(&mut self) -> Option<()> {
        let len = self.u64()?;
        self.skip(len)
    }
}

/// Fixed widths for the scalar value types, so a value we do not care about
/// can be stepped over without interpreting it.
fn scalar_width(kind: u32) -> Option<u64> {
    Some(match kind {
        0 | 1 | 7 => 1, // u8, i8, bool
        2 | 3 => 2,     // u16, i16
        4..=6 => 4,     // u32, i32, f32
        10..=12 => 8,   // u64, i64, f64
        _ => return None,
    })
}

/// Read a value, returning it only when it is a kind we can use.
enum Value {
    U32(u32),
    U64(u64),
    Bool(bool),
    Str(String),
    Other,
}

fn read_value<S: GgufSource>(r: &mut Reader<S>, kind: u32, want_string: bool) -> Option<Value> {
    match kind {
        8 if want_string => Some(Value::Str(r.string()?)),
        8 => {
            r.skip_string()?;
            Some(Value::Other)
        }
        4 => Some(Value::U32(r.u32()?)),
        10 => Some(Value::U64(r.u64()?)),
        7 => Some(Value::Bool(r.take(1)?[0] != 0)),
        9 => {
            // Array: element type, count, elements. Stepped over rather than
            // collected — nothing here needs one, and skipping keeps the walk
            // going so later keys are still read.
            let elem = r.u32()?;
            let count = r.u64()?;
            if elem == 8 {
                // Variable-length: the only way past is one length per element.
                for _ in 0..count {
                    r.skip_string()?;
                }
            } else {
                r.skip(scalar_width(elem)?.checked_mul(count)?)?;
            }
            Some(Value::Other)
        }
        other => {
            r.skip(scalar_width(other)?)?;
            Some(Value::Other)
        }
    }
}

/// Read what a GGUF header says about its model, from any source.
///
/// The walk is identical whatever the bytes come from; only how far it gets
/// differs. A short slice stops when it runs out and returns what it
/// understood, which is the geometry. A file source reaches everything.
fn walk<S: GgufSource>(src: S) -> Option<GgufInfo> {
    let mut r = Reader { src, pos: 0 };
    if r.take(4)? != b"GGUF" {
        return None;
    }
    let _version = r.u32()?;
    let tensor_count = r.u64()?;
    let kv_count = r.u64()?;

    let mut info = GgufInfo {
        tensor_count: Some(tensor_count),
        ..Default::default()
    };

    // Bounded by the header's own count AND by a ceiling, so a corrupt count
    // cannot spin this loop.
    for _ in 0..kv_count.min(4096) {
        let Some(key) = r.string() else { break };
        let Some(kind) = r.u32() else { break };
        // Only materialise the strings we actually keep. Everything else is
        // stepped over, which matters most for the token array.
        let want_string = matches!(
            key.as_str(),
            "general.architecture" | "general.name" | "tokenizer.chat_template"
        );
        let Some(value) = read_value(&mut r, kind, want_string) else {
            break;
        };

        match (key.as_str(), value) {
            ("general.architecture", Value::Str(s)) => info.architecture = Some(s),
            ("general.name", Value::Str(s)) => info.name = Some(s),
            ("tokenizer.chat_template", Value::Str(s)) => info.chat_template = Some(s),
            ("general.file_type", Value::U32(v)) => {
                info.quantization = file_type_name(v).map(str::to_string)
            }
            ("general.parameter_count", Value::U64(v)) => info.parameter_count = Some(v),
            ("general.parameter_count", Value::U32(v)) => info.parameter_count = Some(u64::from(v)),
            // Architecture-scoped keys: "gemma3.context_length",
            // "llama.block_count". Matched by suffix rather than by building
            // the prefix from `general.architecture`, because the two do not
            // always agree and the suffix is unambiguous either way.
            (k, Value::U32(v)) if k.ends_with(".context_length") => info.context_length = Some(v),
            (k, Value::U32(v)) if k.ends_with(".embedding_length") => {
                info.embedding_length = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".block_count") => info.block_count = Some(v),
            (k, Value::U32(v)) if k.ends_with(".attention.head_count_kv") => {
                info.head_count_kv = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".attention.key_length_swa") => {
                info.key_length_swa = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".attention.value_length_swa") => {
                info.value_length_swa = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".attention.key_length") => info.key_length = Some(v),
            (k, Value::U32(v)) if k.ends_with(".attention.value_length") => {
                info.value_length = Some(v)
            }
            (k, Value::U32(v)) if k.ends_with(".attention.shared_kv_layers") => {
                info.shared_kv_layers = Some(v)
            }
            _ => {}
        }
    }

    Some(info)
}

/// Read what a GGUF header says about its model, from a slice.
///
/// `head` need only be the first slice of the file. The geometry keys live in
/// the first two kilobytes, so a small read answers everything the context
/// arithmetic needs — but **not** `chat_template`, which is megabytes in. Use
/// [`parse_gguf_file`] when that matters.
///
/// Returns `None` when the bytes are not GGUF at all, so a caller can tell
/// "not this kind of file" from "a header with little in it".
pub fn parse_gguf_header(head: &[u8]) -> Option<GgufInfo> {
    walk(head)
}

/// Read a GGUF file's header from disk, walking far enough to reach every key
/// including the chat template.
///
/// Costs a few hundred kilobytes of real reading regardless of model size,
/// because everything between the keys is stepped over rather than read.
pub fn parse_gguf_file(path: &std::path::Path) -> Option<GgufInfo> {
    walk(FileSource::open(path)?)
}

// ── Encoder layout: the tensor table, and how long a complete file is ───────

/// What an encoder GGUF (an `mmproj`, `general.architecture = "clip"`) says
/// about its vision tower, and how many bytes a complete copy must have.
///
/// # Why the length has to come from the header
///
/// A file that stops early is still a valid-looking GGUF: the key/value
/// header and the tensor table sit in the first ~85 KB and describe every
/// tensor's offset and shape, so the head of a 64%-present encoder parses
/// exactly like the head of a whole one. The Orin carried one for weeks. The
/// only way to tell them apart without hashing a gigabyte is to add up what
/// the table says is there and compare it with the file's length, which is
/// what [`Self::data_end`] is for.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GgufLayout {
    /// `general.architecture`. `"clip"` for every mmproj llama.cpp writes.
    pub architecture: Option<String>,
    /// `clip.has_vision_encoder`. The Gemma 4 encoders carry an audio tower
    /// as well, so this is a flag rather than implied by the architecture.
    pub has_vision_encoder: Option<bool>,
    /// `clip.vision.projector_type`, falling back to the older single-modality
    /// `clip.projector_type`. `"gemma4v"` pairs with Gemma 4 E2B/E4B.
    pub vision_projector_type: Option<String>,
    /// `clip.vision.projection_dim`: the width the projector writes into, which
    /// must equal the chat model's `embedding_length`.
    pub vision_projection_dim: Option<u32>,
    /// Tensors the header declares.
    pub tensor_count: u64,
    /// `general.alignment`, or the format's default of 32.
    pub alignment: u64,
    /// One past the last byte of tensor data the header describes: the length
    /// a complete file must reach. `None` when the tensor table could not be
    /// read in full (a cut-off header, a type this parser does not size, a
    /// corrupt count), which a caller must treat as "cannot prove complete".
    pub data_end: Option<u64>,
}

/// GGUF's default tensor-data alignment, used when `general.alignment` is absent.
const DEFAULT_ALIGNMENT: u64 = 32;
/// More tensors than any shipped model or encoder declares (Gemma 4 E4B's
/// encoder has 1,411). A larger count is corruption, and walking it would
/// spin.
const MAX_TENSORS: u64 = 1 << 16;
/// ggml's `GGML_MAX_DIMS`.
const MAX_DIMS: u32 = 4;
/// More key/value pairs than any real header carries; see [`walk`].
const MAX_KVS: u64 = 4096;

/// `(block size, bytes per block)` for a ggml tensor type, from
/// `ggml/src/ggml.c`'s type traits in the vendored llama.cpp.
///
/// Only the types an encoder or a common quantisation can carry. An
/// unlisted type returns `None`, which makes [`GgufLayout::data_end`] `None`:
/// an extent this parser cannot compute must not be reported as a short one.
fn ggml_type_size(kind: u32) -> Option<(u64, u64)> {
    Some(match kind {
        0 => (1, 4),      // F32
        1 => (1, 2),      // F16
        2 => (32, 18),    // Q4_0
        3 => (32, 20),    // Q4_1
        6 => (32, 22),    // Q5_0
        7 => (32, 24),    // Q5_1
        8 => (32, 34),    // Q8_0
        10 => (256, 84),  // Q2_K
        11 => (256, 110), // Q3_K
        12 => (256, 144), // Q4_K
        13 => (256, 176), // Q5_K
        14 => (256, 210), // Q6_K
        24 => (1, 1),     // I8
        25 => (1, 2),     // I16
        26 => (1, 4),     // I32
        27 => (1, 8),     // I64
        28 => (1, 8),     // F64
        30 => (1, 2),     // BF16
        _ => return None,
    })
}

/// Walk the key/value header and then the tensor table, from any source.
fn walk_layout<S: GgufSource>(src: S) -> Option<GgufLayout> {
    let mut r = Reader { src, pos: 0 };
    if r.take(4)? != b"GGUF" {
        return None;
    }
    let version = r.u32()?;
    let tensor_count = r.u64()?;
    let kv_count = r.u64()?;

    let mut out = GgufLayout {
        tensor_count,
        alignment: DEFAULT_ALIGNMENT,
        ..Default::default()
    };

    // Version 1 used 32-bit counts and is not produced by anything current;
    // its counts were just misread, so nothing past this point is trustworthy.
    if version < 2 {
        return Some(out);
    }

    let mut legacy_projector: Option<String> = None;
    let mut kvs_complete = kv_count <= MAX_KVS;
    for _ in 0..kv_count.min(MAX_KVS) {
        let Some(key) = r.string() else {
            kvs_complete = false;
            break;
        };
        let Some(kind) = r.u32() else {
            kvs_complete = false;
            break;
        };
        let want_string = matches!(
            key.as_str(),
            "general.architecture" | "clip.vision.projector_type" | "clip.projector_type"
        );
        let Some(value) = read_value(&mut r, kind, want_string) else {
            kvs_complete = false;
            break;
        };
        match (key.as_str(), value) {
            ("general.architecture", Value::Str(s)) => out.architecture = Some(s),
            ("general.alignment", Value::U32(v)) => out.alignment = u64::from(v),
            ("clip.has_vision_encoder", Value::Bool(b)) => out.has_vision_encoder = Some(b),
            ("clip.vision.projector_type", Value::Str(s)) => out.vision_projector_type = Some(s),
            ("clip.projector_type", Value::Str(s)) => legacy_projector = Some(s),
            ("clip.vision.projection_dim", Value::U32(v)) => out.vision_projection_dim = Some(v),
            _ => {}
        }
    }
    if out.vision_projector_type.is_none() {
        out.vision_projector_type = legacy_projector;
    }

    // The tensor table follows the last key directly, so an incomplete key
    // walk leaves the cursor somewhere meaningless.
    if kvs_complete && tensor_count <= MAX_TENSORS {
        out.data_end = tensor_data_end(&mut r, tensor_count, out.alignment);
    }
    Some(out)
}

/// Read `count` tensor infos from the cursor and return where their data ends.
///
/// Each info is `name: string, n_dims: u32, dims: [u64; n_dims], type: u32,
/// offset: u64`, the offset relative to the data section, which starts at the
/// first `alignment` boundary after the table.
fn tensor_data_end<S: GgufSource>(r: &mut Reader<S>, count: u64, alignment: u64) -> Option<u64> {
    if alignment == 0 || !alignment.is_power_of_two() {
        return None;
    }
    let mut end_rel: u64 = 0;
    for _ in 0..count {
        let name_len = r.u64()?;
        if name_len > MAX_STRING_BYTES {
            return None;
        }
        r.skip(name_len)?;
        let n_dims = r.u32()?;
        if n_dims == 0 || n_dims > MAX_DIMS {
            return None;
        }
        let mut elements: u64 = 1;
        for _ in 0..n_dims {
            elements = elements.checked_mul(r.u64()?)?;
        }
        let (block, block_bytes) = ggml_type_size(r.u32()?)?;
        let offset = r.u64()?;
        if !elements.is_multiple_of(block) {
            return None;
        }
        let bytes = (elements / block).checked_mul(block_bytes)?;
        end_rel = end_rel.max(offset.checked_add(bytes)?);
    }
    let data_start = r.pos.checked_add(alignment - 1)? & !(alignment - 1);
    data_start.checked_add(end_rel)
}

/// Read an encoder's layout from a slice.
///
/// Real encoders keep their whole table in the first ~85 KB, so a head read of
/// a few hundred KB reaches [`GgufLayout::data_end`]; a slice that stops inside
/// the table leaves it `None`. `None` overall means the bytes are not GGUF.
pub fn parse_gguf_layout(head: &[u8]) -> Option<GgufLayout> {
    walk_layout(head)
}

/// Read an encoder's layout from disk. Costs the header and the tensor table
/// and nothing past them: about 85 KB for a Gemma 4 encoder, whatever its size.
pub fn parse_gguf_layout_file(path: &std::path::Path) -> Option<GgufLayout> {
    walk_layout(FileSource::open(path)?)
}

/// "4.3B", "270M" — parameter counts as they are spoken.
fn format_params(n: u64) -> String {
    if n >= 1_000_000_000 {
        let b = n as f64 / 1_000_000_000.0;
        if b >= 100.0 {
            format!("{b:.0}B")
        } else {
            format!("{b:.1}B")
        }
    } else if n >= 1_000_000 {
        format!("{}M", n / 1_000_000)
    } else {
        n.to_string()
    }
}

/// 131072 → "131,072". Context windows are long enough that the grouping is
/// what makes them readable at a glance.
fn format_thousands(n: u32) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// "gemma3" → "Gemma3". Only the first letter: the rest of an architecture
/// string is often deliberately cased ("qwen2moe").
fn title_case(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// A GGUF writer for tests in this crate: key/values of the kinds the parsers
/// read, and a tensor table, laid out the way llama.cpp's writer lays them out.
/// Tests elsewhere in `models::domain` build headers with it so they exercise
/// the format rather than a transcription of it.
#[cfg(test)]
pub(crate) mod test_gguf {
    /// ggml type ids used by the tests.
    pub(crate) const F32: u32 = 0;
    pub(crate) const BF16: u32 = 30;

    #[derive(Default)]
    pub(crate) struct GgufWriter {
        kvs: Vec<u8>,
        kv_count: u64,
        tensors: Vec<u8>,
        tensor_count: u64,
        /// Relative end of the tensor data so far, for [`Self::build_complete`].
        data_end_rel: u64,
        alignment: u64,
    }

    impl GgufWriter {
        pub(crate) fn new() -> Self {
            Self {
                alignment: 32,
                ..Default::default()
            }
        }
        fn raw_string(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        fn key(&mut self, k: &str, kind: u32) {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&kind.to_le_bytes());
            self.kv_count += 1;
        }
        pub(crate) fn str(mut self, k: &str, v: &str) -> Self {
            self.key(k, 8);
            Self::raw_string(&mut self.kvs, v);
            self
        }
        pub(crate) fn u32(mut self, k: &str, v: u32) -> Self {
            self.key(k, 4);
            self.kvs.extend_from_slice(&v.to_le_bytes());
            if k == "general.alignment" {
                self.alignment = u64::from(v);
            }
            self
        }
        pub(crate) fn bool(mut self, k: &str, v: bool) -> Self {
            self.key(k, 7);
            self.kvs.push(u8::from(v));
            self
        }
        /// A tensor of `dims` elements of ggml type `kind`, placed after the
        /// previous one at the next alignment boundary, as the writer does.
        pub(crate) fn tensor(mut self, name: &str, dims: &[u64], kind: u32) -> Self {
            let per = match kind {
                F32 => 4,
                BF16 => 2,
                other => panic!("test writer does not size ggml type {other}"),
            };
            let bytes: u64 = dims.iter().product::<u64>() * per;
            let a = self.alignment;
            let offset = self.data_end_rel.div_ceil(a) * a;
            Self::raw_string(&mut self.tensors, name);
            self.tensors
                .extend_from_slice(&(dims.len() as u32).to_le_bytes());
            for d in dims {
                self.tensors.extend_from_slice(&d.to_le_bytes());
            }
            self.tensors.extend_from_slice(&kind.to_le_bytes());
            self.tensors.extend_from_slice(&offset.to_le_bytes());
            self.tensor_count += 1;
            self.data_end_rel = offset + bytes;
            self
        }
        /// The header and tensor table, with no tensor data.
        pub(crate) fn build_header(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(b"GGUF");
            out.extend_from_slice(&3u32.to_le_bytes());
            out.extend_from_slice(&self.tensor_count.to_le_bytes());
            out.extend_from_slice(&self.kv_count.to_le_bytes());
            out.extend_from_slice(&self.kvs);
            out.extend_from_slice(&self.tensors);
            out
        }
        /// The whole file: header, padding to the data section, and exactly
        /// the tensor bytes the table describes (zeroes).
        pub(crate) fn build_complete(&self) -> Vec<u8> {
            let mut out = self.build_header();
            let a = self.alignment as usize;
            let start = out.len().div_ceil(a) * a;
            out.resize(start + self.data_end_rel as usize, 0);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two Gemma 4 models as their headers describe them, read off the
    /// real files on 2026-08-16 (`gemma4.attention.*`, offsets 924-1,829).
    fn gemma_e2b() -> GgufInfo {
        GgufInfo {
            architecture: Some("gemma4".into()),
            block_count: Some(35),
            head_count_kv: Some(1),
            key_length: Some(512),
            value_length: Some(512),
            key_length_swa: Some(256),
            value_length_swa: Some(256),
            shared_kv_layers: Some(20),
            ..Default::default()
        }
    }

    fn gemma_e4b() -> GgufInfo {
        GgufInfo {
            architecture: Some("gemma4".into()),
            block_count: Some(42),
            head_count_kv: Some(2),
            key_length: Some(512),
            value_length: Some(512),
            key_length_swa: Some(256),
            value_length_swa: Some(256),
            shared_kv_layers: Some(18),
            ..Default::default()
        }
    }

    /// The claim this whole function exists to make: the header alone
    /// reproduces what the device measured, so the constant in
    /// `jetson_context_size` does not have to be measured per model.
    ///
    /// Measured on the Orin 2026-08-12 by reading llama.cpp's own
    /// `llama_kv_cache ... size = N MiB (C cells, L layers)` lines:
    /// E2B 96 + 192 MiB at n_ctx 16384 = 18 KiB/token; E4B 128 + 320 MiB at
    /// n_ctx 8192 = 56 KiB/token. Both caches carry `n_ctx` cells, so the cost
    /// is linear with no constant term.
    #[test]
    fn kv_cost_reproduces_the_device_measurements() {
        assert_eq!(
            gemma_e2b().kv_kib_per_token(5),
            Some(18),
            "E2B: 15 owning layers (35 - 20), 3 global at 2 KiB + 12 SWA at 1 KiB"
        );
        assert_eq!(
            gemma_e4b().kv_kib_per_token(5),
            Some(56),
            "E4B: 24 owning layers (42 - 18), 4 global at 4 KiB + 20 SWA at 2 KiB"
        );
    }

    /// The same claim, against real files rather than transcribed numbers.
    ///
    /// `#[ignore]` and env-gated, following the convention the local-inference
    /// crate uses: the GGUFs are gigabytes and are not in the repo. Run with
    ///
    /// ```text
    /// GIAP_TEST_GGUF_DIR="$HOME/Library/Application Support/goose-in-a-pond/models/gguf" \
    ///   cargo test -p pond-core --lib gguf -- --ignored --nocapture
    /// ```
    ///
    /// Transcribing header values into a fixture and asserting on the
    /// transcription proves the arithmetic, not the reading. This proves both.
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn kv_cost_from_the_real_files_on_disk() {
        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        // Geometry lives in the first ~2 KB, long before the token array.
        const HEAD: usize = 64 * 1024;
        let expected = [
            ("gemma-4-E2B-it-Q4_K_M.gguf", 18u64),
            ("gemma-4-E4B-it-Q4_K_M.gguf", 56),
        ];

        let mut checked = 0;
        for (file, want) in expected {
            let path = std::path::Path::new(&dir).join(file);
            let Ok(bytes) = std::fs::read(&path) else {
                eprintln!("skip (absent): {}", path.display());
                continue;
            };
            let head = &bytes[..bytes.len().min(HEAD)];
            let info = parse_gguf_header(head).expect("real GGUF should parse");
            let got = info.kv_kib_per_token(5);
            eprintln!(
                "{file}: blocks={:?} shared={:?} kv_heads={:?} k={:?} k_swa={:?} -> {got:?} KiB/token",
                info.block_count, info.shared_kv_layers, info.head_count_kv,
                info.key_length, info.key_length_swa
            );
            assert_eq!(got, Some(want), "{file} KV cost");
            checked += 1;
        }
        assert!(checked > 0, "no model files found under {dir}");
    }

    /// The seek fix, against real files: the walk must reach a key that lives megabytes past
    /// the token array. Point `GIAP_TEST_GGUF_DIR` at the gguf models directory and run with
    /// `--ignored`.
    #[test]
    #[ignore = "needs real GGUF files; set GIAP_TEST_GGUF_DIR"]
    fn reaches_the_chat_template_past_the_token_array() {
        let Ok(dir) = std::env::var("GIAP_TEST_GGUF_DIR") else {
            eprintln!("GIAP_TEST_GGUF_DIR unset");
            return;
        };
        let mut checked = 0;
        let mut with_template = 0;
        for entry in std::fs::read_dir(&dir).expect("dir") {
            let path = entry.expect("entry").path();
            if path.extension().and_then(|e| e.to_str()) != Some("gguf") {
                continue;
            }
            let Some(info) = parse_gguf_file(&path) else {
                continue;
            };
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            let tpl = info.chat_template.as_deref().unwrap_or("");
            eprintln!(
                "{name}: arch={:?} ctx={:?} template={} chars kv={:?} KiB/tok",
                info.architecture,
                info.context_length,
                tpl.len(),
                info.kv_kib_per_token(5),
            );
            // Geometry must agree between the two walks; the slice must NOT
            // have reached the template, which is the whole reason the file
            // walk exists.
            if let Ok(all) = std::fs::read(&path) {
                let head = &all[..all.len().min(65536)];
                let short = parse_gguf_header(head).expect("parses");
                assert!(
                    short.chat_template.is_none(),
                    "{name}: a 64 KiB slice reached the template, so this test proves nothing"
                );
                assert_eq!(
                    short.block_count, info.block_count,
                    "{name}: geometry must agree between the slice and file walks"
                );
            }
            // A template is not universal: `gemma-4-E4B-it-assistant.Q8_0` is a
            // 95 MB draft model and carries none at all. That is a real state
            // the capability probe has to represent (a model with no template
            // cannot be asked to render tools), not a parse failure.
            if !tpl.is_empty() {
                with_template += 1;
            }
            checked += 1;
        }
        assert!(checked > 0, "no GGUF files under {dir}");
        assert!(
            with_template >= 3,
            "only {with_template} of {checked} models yielded a chat template; the walk is \
             not getting past the token array"
        );
    }

    /// Shared layers allocate nothing, and forgetting that is a 1.75x
    /// overestimate on E4B -- which reads as "this model does not fit" and
    /// silently costs context.
    #[test]
    fn shared_layers_allocate_no_cache() {
        let mut all_owning = gemma_e4b();
        all_owning.shared_kv_layers = None;
        let shared = gemma_e4b().kv_bytes_per_token(5).expect("computed");
        let unshared = all_owning.kv_bytes_per_token(5).expect("computed");
        assert!(
            unshared > shared,
            "ignoring shared_kv_layers must cost more, got {unshared} vs {shared}"
        );
    }

    /// An architecture with no sliding window pays full width on every layer.
    /// This is the conservative direction, which is the right one to be wrong in.
    #[test]
    fn no_swa_widths_means_full_width_everywhere() {
        let dense = GgufInfo {
            block_count: Some(28),
            head_count_kv: Some(2),
            key_length: Some(128),
            value_length: Some(128),
            ..Default::default()
        };
        // 28 layers x (128+128) x 2 heads x 2 bytes = 28,672 bytes = 28 KiB.
        assert_eq!(dense.kv_kib_per_token(5), Some(28));
    }

    /// A header missing what the sum needs must yield nothing, so the caller
    /// keeps its measured fallback instead of acting on a confident guess.
    /// This is the constant that can OOM a board.
    #[test]
    fn incomplete_headers_refuse_rather_than_guess() {
        assert_eq!(GgufInfo::default().kv_kib_per_token(5), None);

        let no_heads = GgufInfo {
            block_count: Some(35),
            key_length: Some(512),
            value_length: Some(512),
            ..Default::default()
        };
        assert_eq!(no_heads.kv_kib_per_token(5), None);

        let zero_heads = GgufInfo {
            block_count: Some(35),
            head_count_kv: Some(0),
            key_length: Some(512),
            value_length: Some(512),
            ..Default::default()
        };
        assert_eq!(zero_heads.kv_kib_per_token(5), None);
    }

    use super::*;

    /// Build a GGUF header the way a writer would, so the parser is tested
    /// against the format rather than against itself.
    struct HeaderBuilder {
        kvs: Vec<u8>,
        count: u64,
    }

    impl HeaderBuilder {
        fn new() -> Self {
            Self {
                kvs: Vec::new(),
                count: 0,
            }
        }
        fn raw_string(out: &mut Vec<u8>, s: &str) {
            out.extend_from_slice(&(s.len() as u64).to_le_bytes());
            out.extend_from_slice(s.as_bytes());
        }
        fn str_kv(mut self, k: &str, v: &str) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&8u32.to_le_bytes());
            Self::raw_string(&mut self.kvs, v);
            self.count += 1;
            self
        }
        fn u32_kv(mut self, k: &str, v: u32) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&4u32.to_le_bytes());
            self.kvs.extend_from_slice(&v.to_le_bytes());
            self.count += 1;
            self
        }
        fn u64_kv(mut self, k: &str, v: u64) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&10u32.to_le_bytes());
            self.kvs.extend_from_slice(&v.to_le_bytes());
            self.count += 1;
            self
        }
        /// A string array — the shape `tokenizer.ggml.tokens` takes, and the
        /// one that has to be stepped over rather than read.
        fn str_array_kv(mut self, k: &str, items: &[&str]) -> Self {
            Self::raw_string(&mut self.kvs, k);
            self.kvs.extend_from_slice(&9u32.to_le_bytes());
            self.kvs.extend_from_slice(&8u32.to_le_bytes());
            self.kvs
                .extend_from_slice(&(items.len() as u64).to_le_bytes());
            for it in items {
                Self::raw_string(&mut self.kvs, it);
            }
            self.count += 1;
            self
        }
        fn build(self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(b"GGUF");
            out.extend_from_slice(&3u32.to_le_bytes());
            out.extend_from_slice(&291u64.to_le_bytes()); // tensor_count
            out.extend_from_slice(&self.count.to_le_bytes());
            out.extend_from_slice(&self.kvs);
            out
        }
    }

    fn gemma_header() -> Vec<u8> {
        HeaderBuilder::new()
            .str_kv("general.architecture", "gemma3")
            .str_kv("general.name", "Gemma 3 4B It")
            .u32_kv("general.file_type", 15) // Q4_K_M
            .u64_kv("general.parameter_count", 4_300_000_000)
            .u32_kv("gemma3.context_length", 8192)
            .u32_kv("gemma3.embedding_length", 2560)
            .u32_kv("gemma3.block_count", 34)
            .build()
    }

    #[test]
    fn reads_what_the_file_says_about_itself() {
        let info = parse_gguf_header(&gemma_header()).expect("is gguf");
        assert_eq!(info.architecture.as_deref(), Some("gemma3"));
        assert_eq!(info.name.as_deref(), Some("Gemma 3 4B It"));
        assert_eq!(info.quantization.as_deref(), Some("Q4_K_M"));
        assert_eq!(info.parameter_count, Some(4_300_000_000));
        assert_eq!(info.context_length, Some(8192));
        assert_eq!(info.embedding_length, Some(2560));
        assert_eq!(info.block_count, Some(34));
        assert_eq!(info.tensor_count, Some(291));
    }

    #[test]
    fn summarises_in_the_order_a_model_name_is_read() {
        let info = parse_gguf_header(&gemma_header()).unwrap();
        assert_eq!(
            info.summary().as_deref(),
            Some("Gemma3 · 4.3B · Q4_K_M · 8,192 ctx")
        );
    }

    /// Real headers carry a token vocabulary — tens of thousands of strings
    /// between the keys we want. Stepping over it has to work or everything
    /// after it is lost.
    #[test]
    fn steps_over_the_arrays_it_does_not_need() {
        let bytes = HeaderBuilder::new()
            .str_kv("general.architecture", "llama")
            .str_array_kv("tokenizer.ggml.tokens", &["<s>", "</s>", "hello", "world"])
            .u32_kv("llama.context_length", 131072)
            .build();

        let info = parse_gguf_header(&bytes).expect("is gguf");
        assert_eq!(info.architecture.as_deref(), Some("llama"));
        // The key AFTER the array is what proves the skip landed correctly.
        assert_eq!(info.context_length, Some(131072));
    }

    #[test]
    fn matches_architecture_scoped_keys_whatever_the_prefix() {
        for arch in ["llama", "qwen3", "phi3", "gemma3"] {
            let bytes = HeaderBuilder::new()
                .u32_kv(&format!("{arch}.context_length"), 4096)
                .build();
            assert_eq!(
                parse_gguf_header(&bytes).unwrap().context_length,
                Some(4096)
            );
        }
    }

    #[test]
    fn says_when_the_bytes_are_not_gguf_at_all() {
        assert!(parse_gguf_header(b"ONNX....").is_none());
        assert!(parse_gguf_header(b"").is_none());
        assert!(parse_gguf_header(b"GGU").is_none());
    }

    /// A model file is arbitrary bytes from the internet. The worst a broken
    /// header should cost is a card with less on it.
    #[test]
    fn survives_a_header_cut_off_mid_field() {
        let full = gemma_header();
        for cut in 0..full.len() {
            let _ = parse_gguf_header(&full[..cut]); // must not panic
        }
        // Truncated right after the counts: valid GGUF, nothing learned.
        let info = parse_gguf_header(&full[..24]).expect("still gguf");
        assert_eq!(info.architecture, None);
        assert_eq!(info.tensor_count, Some(291));
    }

    #[test]
    fn refuses_an_absurd_string_length_rather_than_allocating_it() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GGUF");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&1u64.to_le_bytes());
        // A key claiming to be 16 exabytes long.
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());

        let info = parse_gguf_header(&bytes).expect("header itself is valid");
        assert!(info.architecture.is_none());
    }

    #[test]
    fn a_corrupt_kv_count_cannot_spin_the_walk() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"GGUF");
        bytes.extend_from_slice(&3u32.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(&u64::MAX.to_le_bytes()); // kv_count lies
        assert!(parse_gguf_header(&bytes).is_some());
    }

    #[test]
    fn leaves_an_unrecognised_quantisation_unnamed() {
        let bytes = HeaderBuilder::new()
            .u32_kv("general.file_type", 9999)
            .build();
        assert_eq!(parse_gguf_header(&bytes).unwrap().quantization, None);
    }

    #[test]
    fn spells_parameter_counts_and_windows_the_way_they_are_spoken() {
        assert_eq!(format_params(4_300_000_000), "4.3B");
        assert_eq!(format_params(270_000_000), "270M");
        assert_eq!(format_params(120_000_000_000), "120B");
        assert_eq!(format_thousands(131072), "131,072");
        assert_eq!(format_thousands(8192), "8,192");
        assert_eq!(format_thousands(512), "512");
    }

    #[test]
    fn a_header_with_nothing_useful_says_so() {
        let info = parse_gguf_header(&HeaderBuilder::new().build()).unwrap();
        assert!(info.summary().is_none());
    }

    // ── Encoder layout ──────────────────────────────────────────────────────

    use super::test_gguf::{GgufWriter, BF16, F32};

    /// A small encoder shaped like the real ones: the same keys, a vision and
    /// an audio tensor, F32 and BF16 mixed.
    fn encoder() -> GgufWriter {
        GgufWriter::new()
            .str("general.architecture", "clip")
            .str("general.type", "mmproj")
            .bool("clip.has_vision_encoder", true)
            .bool("clip.has_audio_encoder", true)
            .str("clip.vision.projector_type", "gemma4v")
            .u32("clip.vision.projection_dim", 1536)
            .str("clip.audio.projector_type", "gemma4a")
            .tensor("v.patch_embd.weight", &[16, 16, 3, 7], BF16)
            .tensor("a.conv1d.0.bias", &[33], F32)
            .tensor("mm.input_projection.weight", &[8, 5], BF16)
    }

    #[test]
    fn reads_what_an_encoder_says_about_its_vision_tower() {
        let layout = parse_gguf_layout(&encoder().build_complete()).expect("is gguf");
        assert_eq!(layout.architecture.as_deref(), Some("clip"));
        assert_eq!(layout.has_vision_encoder, Some(true));
        assert_eq!(
            layout.vision_projector_type.as_deref(),
            Some("gemma4v"),
            "the audio projector type must not overwrite the vision one"
        );
        assert_eq!(layout.vision_projection_dim, Some(1536));
        assert_eq!(layout.tensor_count, 3);
        assert_eq!(layout.alignment, 32);
    }

    /// The whole point: the extent the table describes is the length of the
    /// complete file, computed by a writer that knows nothing of the parser.
    #[test]
    fn the_tensor_table_gives_the_length_of_a_complete_file() {
        let full = encoder().build_complete();
        let layout = parse_gguf_layout(&full).unwrap();
        assert_eq!(layout.data_end, Some(full.len() as u64));
    }

    /// A cut-off file parses its head exactly like a whole one, and still
    /// reports the full extent, which is how a short file is recognised.
    #[test]
    fn a_short_file_still_reports_the_length_it_should_have() {
        let full = encoder().build_complete();
        let header_len = encoder().build_header().len();
        let head = &full[..header_len + 10];
        let layout = parse_gguf_layout(head).unwrap();
        assert_eq!(layout.data_end, Some(full.len() as u64));
    }

    /// A table the bytes stop inside cannot be summed, and must say so
    /// rather than report whatever it had added up.
    #[test]
    fn a_table_cut_off_mid_entry_has_no_extent() {
        let header = encoder().build_header();
        for cut in [header.len() - 1, header.len() - 9, header.len() - 30] {
            let layout = parse_gguf_layout(&header[..cut]).expect("still gguf");
            assert_eq!(layout.data_end, None, "cut at {cut}");
        }
        for cut in 0..header.len() {
            let _ = parse_gguf_layout(&header[..cut]); // must not panic
        }
    }

    #[test]
    fn honours_a_declared_alignment() {
        let w = GgufWriter::new()
            .u32("general.alignment", 64)
            .str("general.architecture", "clip")
            .tensor("a", &[3], F32)
            .tensor("b", &[5], F32);
        let full = w.build_complete();
        let layout = parse_gguf_layout(&full).unwrap();
        assert_eq!(layout.alignment, 64);
        assert_eq!(layout.data_end, Some(full.len() as u64));
    }

    /// The older single-modality key still names the projector.
    #[test]
    fn falls_back_to_the_legacy_projector_key() {
        let bytes = GgufWriter::new()
            .str("general.architecture", "clip")
            .str("clip.projector_type", "mlp")
            .build_complete();
        assert_eq!(
            parse_gguf_layout(&bytes)
                .unwrap()
                .vision_projector_type
                .as_deref(),
            Some("mlp")
        );
    }

    /// A tensor type this parser cannot size makes the extent unknown, never
    /// a smaller number that would read as a truncation.
    #[test]
    fn an_unsized_tensor_type_leaves_the_extent_unknown() {
        let mut header = GgufWriter::new()
            .str("general.architecture", "clip")
            .tensor("t", &[4], F32)
            .build_header();
        // The type id sits 12 bytes from the end (type u32, offset u64).
        let at = header.len() - 12;
        header[at..at + 4].copy_from_slice(&999u32.to_le_bytes());
        assert_eq!(parse_gguf_layout(&header).unwrap().data_end, None);
    }

    /// Adding the bool kind must not disturb the model-header walk, which
    /// used to step over it as an opaque byte.
    #[test]
    fn a_bool_key_does_not_derail_the_model_header_walk() {
        let bytes = GgufWriter::new()
            .bool("general.some_flag", true)
            .u32("gemma4.embedding_length", 2560)
            .build_header();
        assert_eq!(
            parse_gguf_header(&bytes).unwrap().embedding_length,
            Some(2560)
        );
    }

    #[test]
    fn a_model_is_not_mistaken_for_an_encoder() {
        let layout = parse_gguf_layout(&gemma_header()).unwrap();
        assert_eq!(layout.architecture.as_deref(), Some("gemma3"));
        assert_eq!(layout.has_vision_encoder, None);
        assert!(parse_gguf_layout(b"ONNX....").is_none());
    }
}
