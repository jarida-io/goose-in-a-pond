/// Overwrites `bytes` with zeros. pond-core has no zeroize, and a plain store to memory about to be
/// freed is a dead store the optimiser may delete; `black_box` keeps it.
pub fn wipe(bytes: &mut [u8]) {
    bytes.fill(0);
    std::hint::black_box(bytes);
}

/// [`wipe`] for an owned string, which safe code can only reach through its bytes.
pub fn wipe_string(text: &mut String) {
    let mut bytes = std::mem::take(text).into_bytes();
    wipe(&mut bytes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_zeroes_every_byte() {
        let mut bytes = [0xa5u8; 32];
        wipe(&mut bytes);
        assert_eq!(bytes, [0u8; 32]);
    }

    #[test]
    fn a_wiped_string_is_left_empty() {
        let mut text = String::from("correct horse");
        wipe_string(&mut text);
        assert!(text.is_empty());
    }
}
