//! WAV in and out, for replaying recordings through the ear and writing what it heard.

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// 16-bit PCM or 32-bit float WAV, mixed down to mono, as `(rate, [-1, 1] samples)`.
pub fn decode_wav(b: &[u8]) -> Result<(u32, Vec<f32>), String> {
    let (rate, ch, frames) = decode_wav_channels(b)?;
    let mono = frames
        .chunks_exact(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect();
    Ok((rate, mono))
}

/// `decode_wav` without the mix-down: `(rate, channels, interleaved samples)`.
pub fn decode_wav_channels(b: &[u8]) -> Result<(u32, usize, Vec<f32>), String> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a RIFF/WAVE file".into());
    }
    let (mut fmt, mut i) = (None, 12);
    while i + 8 <= b.len() {
        let id = &b[i..i + 4];
        let len = u32_at(b, i + 4) as usize;
        let body = b
            .get(i + 8..(i + 8 + len).min(b.len()))
            .ok_or("truncated chunk")?;
        if id == b"fmt " && body.len() >= 16 {
            fmt = Some((
                u16_at(body, 0),
                u16_at(body, 2) as usize,
                u32_at(body, 4),
                u16_at(body, 14),
            ));
        } else if id == b"data" {
            let (tag, ch, rate, bits) = fmt.ok_or("data before fmt")?;
            let ch = ch.max(1);
            let frames: Vec<f32> = match (tag, bits) {
                (1, 16) => body
                    .chunks_exact(2)
                    .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                    .collect(),
                (3, 32) | (0xFFFE, 32) => body
                    .chunks_exact(4)
                    .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
                    .collect(),
                _ => return Err(format!("unsupported WAV format tag {tag}, {bits} bits")),
            };
            return Ok((rate, ch, frames));
        }
        i += 8 + len + (len & 1);
    }
    Err("no data chunk".into())
}

/// Mono 16-bit PCM WAV: the pond's own capture format.
pub fn encode_wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    for v in [1u16, 1] {
        w.extend_from_slice(&v.to_le_bytes());
    }
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * 2).to_le_bytes());
    for v in [2u16, 16] {
        w.extend_from_slice(&v.to_le_bytes());
    }
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        w.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    w
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_written_wav_reads_back() {
        let x: Vec<f32> = (0..1000).map(|i| ((i as f32) * 0.01).sin() * 0.5).collect();
        let (rate, y) = decode_wav(&encode_wav16(&x, 16_000)).unwrap();
        assert_eq!((rate, y.len()), (16_000, x.len()));
        assert!(x
            .iter()
            .zip(&y)
            .all(|(a, b)| (a - b).abs() < 1.0 / 16_000.0));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(decode_wav(b"RIFF\0\0\0\0WAVEdata\xff\xff\xff\xff").is_err());
        assert!(decode_wav(&[0u8; 3]).is_err());
    }
}
