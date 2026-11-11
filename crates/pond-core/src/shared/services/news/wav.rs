//! Joining the voice's WAVs into one recording, so a digest is synthesised a paragraph at a time
//! and played as one file.

/// A WAV's format chunk and its samples.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Wav<'a> {
    fmt: &'a [u8],
    data: &'a [u8],
}

fn le16(bytes: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?))
}

fn le32(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn parse(wav: &[u8]) -> Option<Wav<'_>> {
    if wav.get(0..4)? != b"RIFF" || wav.get(8..12)? != b"WAVE" {
        return None;
    }
    let mut fmt = None;
    let mut at = 12;
    while let (Some(id), Some(size)) = (wav.get(at..at + 4), le32(wav, at + 4)) {
        let body = at + 8;
        let size = size as usize;
        match id {
            b"fmt " => fmt = wav.get(body..body + size),
            b"data" => {
                // A streamed WAV can carry 0 or u32::MAX here; what is there is the length.
                let there = wav.len().saturating_sub(body);
                let len = if size == 0 || size > there {
                    there
                } else {
                    size
                };
                return Some(Wav {
                    fmt: fmt?,
                    data: wav.get(body..body + len)?,
                });
            }
            _ => {}
        }
        at = body.checked_add(size)?.checked_add(size & 1)?;
    }
    None
}

/// One WAV holding every part in order, `gap_ms` of silence between them. Parts in another
/// format than the first, or unreadable, are left out; `None` when no part is readable.
pub fn join(parts: &[Vec<u8>], gap_ms: u32) -> Option<Vec<u8>> {
    let parsed: Vec<Wav<'_>> = parts.iter().filter_map(|p| parse(p)).collect();
    let first = parsed.first()?;
    let fmt = first.fmt;
    let byte_rate = le32(fmt, 8)?;
    let block_align = u32::from(le16(fmt, 12)?.max(1));
    let gap_bytes = (byte_rate / 1000 * gap_ms) / block_align * block_align;
    let same: Vec<&Wav<'_>> = parsed.iter().filter(|w| w.fmt == fmt).collect();
    let mut data: Vec<u8> = Vec::new();
    for (i, wav) in same.iter().enumerate() {
        if i > 0 {
            data.extend(std::iter::repeat_n(0u8, gap_bytes as usize));
        }
        data.extend_from_slice(wav.data);
    }
    let mut out = Vec::with_capacity(data.len() + fmt.len() + 28);
    out.extend_from_slice(b"RIFF");
    let riff_size = u32::try_from(4 + 8 + fmt.len() + (fmt.len() & 1) + 8 + data.len()).ok()?;
    out.extend_from_slice(&riff_size.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&u32::try_from(fmt.len()).ok()?.to_le_bytes());
    out.extend_from_slice(fmt);
    if fmt.len() & 1 == 1 {
        out.push(0);
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&u32::try_from(data.len()).ok()?.to_le_bytes());
    out.extend_from_slice(&data);
    Some(out)
}

#[cfg(any(test, feature = "test-mocks"))]
pub fn pcm16(rate: u32, samples: &[i16]) -> Vec<u8> {
    let mut fmt = Vec::new();
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&1u16.to_le_bytes());
    fmt.extend_from_slice(&rate.to_le_bytes());
    fmt.extend_from_slice(&(rate * 2).to_le_bytes());
    fmt.extend_from_slice(&2u16.to_le_bytes());
    fmt.extend_from_slice(&16u16.to_le_bytes());
    let data: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let mut out = b"RIFF".to_vec();
    out.extend_from_slice(&((4 + 8 + fmt.len() + 8 + data.len()) as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(&fmt);
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(&data);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parts_join_in_order_with_a_silence_between() {
        let joined = join(&[pcm16(1000, &[1, 2]), pcm16(1000, &[3])], 450).unwrap();
        let wav = parse(&joined).unwrap();
        let gap = 2 * 450;
        assert_eq!(wav.data.len(), 4 + gap + 2);
        assert_eq!(&wav.data[..4], &[1, 0, 2, 0]);
        assert!(wav.data[4..4 + gap].iter().all(|b| *b == 0));
        assert_eq!(&wav.data[4 + gap..], &[3, 0]);
        assert_eq!(le32(&joined, 4).unwrap() as usize, joined.len() - 8);
    }

    #[test]
    fn a_part_in_another_format_or_not_a_wav_is_left_out() {
        let joined = join(
            &[
                b"not audio".to_vec(),
                pcm16(1000, &[1]),
                pcm16(2000, &[9, 9, 9]),
            ],
            0,
        )
        .unwrap();
        assert_eq!(parse(&joined).unwrap().data, &[1, 0]);
        assert_eq!(join(&[b"nothing".to_vec()], 0), None);
    }
}
