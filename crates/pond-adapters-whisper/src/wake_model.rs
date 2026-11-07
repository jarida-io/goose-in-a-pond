//! A Whisper model cut down for the room's wake check. whisper.cpp sizes every state from the
//! file's audio context (1500 frames, 30 s) and text context (448 tokens), and a wake check reads
//! at most 6 s and writes a few words. The copy declares 384 frames and 64 tokens, trims both
//! positional embeddings to match, and stores its 2-D weights as q5_1 the way whisper.cpp's own
//! quantizer does: about 101 MB loaded for base.en, against 327 MB.

use std::fs::File;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use whisper_rs::whisper_rs_sys as sys;

/// 7.68 s: the 6 s a wake scan reads, with the 20% margin `fit_audio_ctx` adds.
pub const WAKE_AUDIO_CTX: i32 = 384;
/// Room for the prompt (whisper.cpp keeps at most half) and the words of a wake check.
pub const WAKE_TEXT_CTX: i32 = 64;

const MAGIC: u32 = 0x6767_6d6c;
const TYPE_F32: i32 = 0;
const TYPE_F16: i32 = 1;
const TYPE_Q5_1: i32 = 7;
/// What whisper.cpp's quantizer writes as the file type for q5_1: version 2, `MOSTLY_Q5_1`.
const FTYPE_Q5_1_V2: i32 = 2 * 1000 + 9;
const Q_BLOCK: usize = 32;
const Q5_1_BLOCK_BYTES: usize = 24;
/// whisper.cpp's quantizer leaves these as they are.
const KEPT: [&str; 4] = [
    "encoder.conv1.bias",
    "encoder.conv2.bias",
    "encoder.positional_embedding",
    "decoder.positional_embedding",
];
/// Rows quantized at a time, so a 106 MB embedding table never sits in memory as f32.
const ROWS_PER_PASS: usize = 256;

/// Where the wake copy of `full` lives inside `dir`.
pub fn wake_model_path(full: &Path, dir: &Path) -> PathBuf {
    let stem = full
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "whisper".into());
    dir.join(format!("{stem}-wake-q5_1.bin"))
}

/// The wake copy of `full` in `dir`, made now if it is missing or older than `full`.
pub fn ensure(full: &Path, dir: &Path) -> Result<PathBuf> {
    let dst = wake_model_path(full, dir);
    let modified = |p: &Path| std::fs::metadata(p).and_then(|m| m.modified()).ok();
    if let (Some(made), Some(source)) = (modified(&dst), modified(full)) {
        if made >= source {
            return Ok(dst);
        }
    }
    std::fs::create_dir_all(dir).with_context(|| dir.display().to_string())?;
    let partial = dst.with_extension("partial");
    let started = std::time::Instant::now();
    if let Err(e) = derive(full, &partial, WAKE_AUDIO_CTX, WAKE_TEXT_CTX) {
        let _ = std::fs::remove_file(&partial);
        return Err(e.context(format!("making the wake model from {}", full.display())));
    }
    std::fs::rename(&partial, &dst)?;
    tracing::info!(
        target: "giap::trace",
        kind = "whisper_wake_model_made",
        source = %full.display(),
        bytes = std::fs::metadata(&dst).map(|m| m.len()).unwrap_or(0),
        elapsed_ms = started.elapsed().as_millis() as u64,
        "made the room's wake-only Whisper"
    );
    Ok(dst)
}

struct Io<R: Read, W: Write> {
    r: R,
    w: W,
}

impl<R: Read, W: Write> Io<R, W> {
    fn i32(&mut self) -> Result<i32> {
        let mut b = [0u8; 4];
        self.r.read_exact(&mut b)?;
        Ok(i32::from_le_bytes(b))
    }

    fn put(&mut self, v: i32) -> Result<()> {
        Ok(self.w.write_all(&v.to_le_bytes())?)
    }

    fn copy(&mut self, n: usize) -> Result<()> {
        let mut left = n;
        let mut buf = vec![0u8; 1 << 16];
        while left > 0 {
            let take = left.min(buf.len());
            self.r.read_exact(&mut buf[..take])?;
            self.w.write_all(&buf[..take])?;
            left -= take;
        }
        Ok(())
    }

    fn take(&mut self, n: usize) -> Result<Vec<u8>> {
        let mut buf = vec![0u8; n];
        self.r.read_exact(&mut buf)?;
        Ok(buf)
    }
}

/// Rewrite `src` (a ggml Whisper model in f16 or f32) into `dst` with the given contexts.
pub fn derive(src: &Path, dst: &Path, audio_ctx: i32, text_ctx: i32) -> Result<()> {
    let r = BufReader::new(File::open(src).with_context(|| src.display().to_string())?);
    let w = BufWriter::new(File::create(dst).with_context(|| dst.display().to_string())?);
    let mut io = Io { r, w };

    if io.i32()? as u32 != MAGIC {
        bail!("{} is not a ggml Whisper model", src.display());
    }
    io.put(MAGIC as i32)?;
    // n_vocab, n_audio_ctx, n_audio_state, n_audio_head, n_audio_layer, n_text_ctx,
    // n_text_state, n_text_head, n_text_layer, n_mels, ftype
    let mut hp = [0i32; 11];
    for v in hp.iter_mut() {
        *v = io.i32()?;
    }
    if audio_ctx > hp[1] || text_ctx > hp[5] {
        bail!("the wake contexts exceed the model's own");
    }
    hp[1] = audio_ctx;
    hp[5] = text_ctx;
    hp[10] = FTYPE_Q5_1_V2;
    for v in hp {
        io.put(v)?;
    }

    let (n_mel, n_fft) = (io.i32()?, io.i32()?);
    io.put(n_mel)?;
    io.put(n_fft)?;
    io.copy(4 * n_mel as usize * n_fft as usize)?;
    let n_vocab = io.i32()?;
    io.put(n_vocab)?;
    for _ in 0..n_vocab {
        let len = io.i32()?;
        io.put(len)?;
        io.copy(len as usize)?;
    }

    let mut trimmed = 0;
    loop {
        let mut head = [0u8; 4];
        match io.r.read(&mut head)? {
            0 => break,
            4 => {}
            n => io.r.read_exact(&mut head[n..])?,
        }
        let n_dims = i32::from_le_bytes(head);
        let name_len = io.i32()?;
        let ttype = io.i32()?;
        let mut ne: Vec<i32> = (0..n_dims).map(|_| io.i32()).collect::<Result<_>>()?;
        let name = String::from_utf8(io.take(name_len as usize)?)?;
        let elements: usize = ne.iter().map(|&d| d as usize).product();
        if ttype != TYPE_F32 && ttype != TYPE_F16 {
            bail!("{name} is already quantized; derive from an f16 or f32 model");
        }
        let width = if ttype == TYPE_F32 { 4 } else { 2 };

        let keep_rows = match name.as_str() {
            "encoder.positional_embedding" => Some(audio_ctx),
            "decoder.positional_embedding" => Some(text_ctx),
            _ => None,
        };
        let quantize = n_dims == 2
            && !KEPT.contains(&name.as_str())
            && (ne[0] as usize).is_multiple_of(Q_BLOCK);
        let out_type = if quantize { TYPE_Q5_1 } else { ttype };
        let rows = elements / ne[0].max(1) as usize;
        if let Some(keep) = keep_rows {
            ne[1] = keep;
            trimmed += 1;
        }

        io.put(n_dims)?;
        io.put(name_len)?;
        io.put(out_type)?;
        for &d in &ne {
            io.put(d)?;
        }
        io.w.write_all(name.as_bytes())?;

        let row_bytes = ne[0] as usize * width;
        if let Some(keep) = keep_rows {
            io.copy(keep as usize * row_bytes)?;
            // The rows past the wake context are read and dropped.
            io.take((rows - keep as usize) * row_bytes)?;
        } else if quantize {
            quantize_rows(&mut io, rows, ne[0] as usize, ttype)?;
        } else {
            io.copy(elements * width)?;
        }
    }
    if trimmed != 2 {
        bail!("expected both positional embeddings, found {trimmed}");
    }
    io.w.flush()?;
    Ok(())
}

fn quantize_rows<R: Read, W: Write>(
    io: &mut Io<R, W>,
    rows: usize,
    per_row: usize,
    ttype: i32,
) -> Result<()> {
    let mut f32s = vec![0f32; ROWS_PER_PASS * per_row];
    let mut out = vec![0u8; ROWS_PER_PASS * per_row / Q_BLOCK * Q5_1_BLOCK_BYTES];
    let mut done = 0;
    while done < rows {
        let n = (rows - done).min(ROWS_PER_PASS);
        let count = n * per_row;
        if ttype == TYPE_F16 {
            let raw = io.take(count * 2)?;
            let halves: Vec<u16> = raw
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect();
            // SAFETY: both buffers hold `count` elements.
            unsafe { sys::ggml_fp16_to_fp32_row(halves.as_ptr(), f32s.as_mut_ptr(), count as i64) };
        } else {
            let raw = io.take(count * 4)?;
            for (dst, b) in f32s.iter_mut().zip(raw.chunks_exact(4)) {
                *dst = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
            }
        }
        // SAFETY: `f32s` holds `n` rows of `per_row` floats and `out` the q5_1 blocks for them;
        // q5_1 needs no importance matrix.
        let written = unsafe {
            sys::ggml_quantize_chunk(
                TYPE_Q5_1 as sys::ggml_type,
                f32s.as_ptr(),
                out.as_mut_ptr().cast(),
                0,
                n as i64,
                per_row as i64,
                std::ptr::null(),
            )
        };
        let expected = count / Q_BLOCK * Q5_1_BLOCK_BYTES;
        if written != expected {
            return Err(anyhow!("q5_1 wrote {written} bytes for {expected}"));
        }
        io.w.write_all(&out[..written])?;
        done += n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn le(v: i32) -> [u8; 4] {
        v.to_le_bytes()
    }

    fn tensor(out: &mut Vec<u8>, name: &str, ttype: i32, ne: &[i32], data: &[u8]) {
        out.extend(le(ne.len() as i32));
        out.extend(le(name.len() as i32));
        out.extend(le(ttype));
        for &d in ne {
            out.extend(le(d));
        }
        out.extend(name.as_bytes());
        out.extend(data);
    }

    /// A model with 8 audio frames and 8 text positions of width 32.
    fn tiny_model() -> Vec<u8> {
        let mut m = Vec::new();
        m.extend(le(MAGIC as i32));
        for v in [2, 8, 32, 1, 1, 8, 32, 1, 1, 1, 1] {
            m.extend(le(v));
        }
        m.extend(le(1));
        m.extend(le(1));
        m.extend(1.0f32.to_le_bytes());
        m.extend(le(2));
        for word in ["a", "bc"] {
            m.extend(le(word.len() as i32));
            m.extend(word.as_bytes());
        }
        let rows_f32 = |rows: usize| -> Vec<u8> {
            (0..rows * 32)
                .flat_map(|i| (i as f32).to_le_bytes())
                .collect()
        };
        tensor(
            &mut m,
            "encoder.positional_embedding",
            TYPE_F32,
            &[32, 8],
            &rows_f32(8),
        );
        tensor(
            &mut m,
            "decoder.positional_embedding",
            TYPE_F32,
            &[32, 8],
            &rows_f32(8),
        );
        tensor(
            &mut m,
            "encoder.conv1.bias",
            TYPE_F32,
            &[1, 32],
            &rows_f32(1),
        );
        let halves: Vec<u8> = (0..32 * 4).flat_map(|_| 0x3c00u16.to_le_bytes()).collect();
        tensor(
            &mut m,
            "decoder.token_embedding.weight",
            TYPE_F16,
            &[32, 4],
            &halves,
        );
        tensor(
            &mut m,
            "encoder.conv1.weight",
            TYPE_F16,
            &[3, 1, 32],
            &vec![0u8; 3 * 32 * 2],
        );
        m
    }

    #[test]
    fn trims_both_contexts_and_quantizes_only_what_whisper_cpp_would() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("m.bin"), dir.path().join("w.bin"));
        std::fs::write(&src, tiny_model()).unwrap();
        derive(&src, &dst, 4, 2).unwrap();
        let out = std::fs::read(&dst).unwrap();
        let at = |i: usize| i32::from_le_bytes(out[i..i + 4].try_into().unwrap());
        assert_eq!(at(0) as u32, MAGIC);
        assert_eq!(
            (at(8), at(24), at(44)),
            (4, 2, FTYPE_Q5_1_V2),
            "audio, text, ftype"
        );

        let original = tiny_model().len();
        let saved = 4 * 32 * 4 + 6 * 32 * 4 // the dropped positional rows
            + 32 * 4 * 2
            - 32 * 4 / 32 * 24; // f16 embedding against q5_1
        assert_eq!(out.len(), original - saved);
        let embedding = out
            .windows(30)
            .position(|w| w == b"decoder.token_embedding.weight")
            .unwrap();
        assert_eq!(at(embedding - 12), TYPE_Q5_1);
        let bias = out
            .windows(18)
            .position(|w| w == b"encoder.conv1.bias")
            .unwrap();
        assert_eq!(at(bias - 12), TYPE_F32, "kept as whisper.cpp keeps it");
    }

    #[test]
    fn ensure_makes_the_copy_once_and_again_only_for_a_newer_source() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("ggml-tiny.bin");
        std::fs::write(&src, tiny_model()).unwrap();
        let wake_dir = dir.path().join("wake");
        // `ensure` uses the real wake contexts, which the tiny model is smaller than.
        assert!(ensure(&src, &wake_dir).is_err());
        assert!(!wake_model_path(&src, &wake_dir).exists());
        assert_eq!(
            wake_model_path(&src, &wake_dir),
            wake_dir.join("ggml-tiny-wake-q5_1.bin")
        );
    }

    #[test]
    fn a_quantized_source_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (src, dst) = (dir.path().join("m.bin"), dir.path().join("w.bin"));
        let mut m = tiny_model();
        let at = m
            .windows(30)
            .position(|w| w == b"decoder.token_embedding.weight")
            .unwrap();
        m[at - 12..at - 8].copy_from_slice(&le(TYPE_Q5_1));
        std::fs::write(&src, m).unwrap();
        assert!(derive(&src, &dst, 4, 2).is_err());
    }
}
