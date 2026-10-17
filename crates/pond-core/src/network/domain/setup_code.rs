use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// Digits drawn at random. About 33 bits, which the guess throttle makes outlast the hardware.
pub const SETUP_CODE_RANDOM_DIGITS: usize = 10;
/// Every digit of a setup code, its check digit included.
pub const SETUP_CODE_LEN: usize = SETUP_CODE_RANDOM_DIGITS + 1;

/// Why text is not a setup code. No variant carries the digits, so an error is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum SetupCodeError {
    #[error("a setup code has {} digits, this has {found}", SETUP_CODE_LEN)]
    WrongLength { found: usize },
    #[error("a setup code is digits only")]
    NotDigits,
    /// Lets the phone say "typo" before it spends a guess on the pond.
    #[error("the setup code's check digit does not match: a digit is mistyped")]
    CheckDigitMismatch,
    #[error("a setup code's random digits are each 0 to 9")]
    DigitOutOfRange,
}

/// The secret on a pond's printed card. `Debug` redacts and there is no `Display`, so a log line
/// or an error can never carry it by accident (PAI-2).
#[derive(Clone, PartialEq, Eq)]
pub struct SetupCode(String);

impl SetupCode {
    /// Takes the random digits rather than an RNG: pond-core has none, so the store draws them.
    pub fn from_digits(random: [u8; SETUP_CODE_RANDOM_DIGITS]) -> Result<Self, SetupCodeError> {
        if random.iter().any(|&digit| digit > 9) {
            return Err(SetupCodeError::DigitOutOfRange);
        }
        let check = luhn_check_digit(&random);
        Ok(Self(
            random
                .iter()
                .chain(std::iter::once(&check))
                .map(|&digit| char::from(b'0' + digit))
                .collect(),
        ))
    }

    /// Accepts the eleven digits with or without the card's dashes and spaces.
    pub fn parse(input: &str) -> Result<Self, SetupCodeError> {
        let mut digits = [0u8; SETUP_CODE_LEN];
        let mut found = 0usize;
        for c in input.chars() {
            if c == '-' || c == ' ' {
                continue;
            }
            if !c.is_ascii_digit() {
                return Err(SetupCodeError::NotDigits);
            }
            if let Some(slot) = digits.get_mut(found) {
                *slot = c as u8 - b'0';
            }
            found += 1;
        }
        if found != SETUP_CODE_LEN {
            return Err(SetupCodeError::WrongLength { found });
        }
        let (random, check) = digits.split_at(SETUP_CODE_RANDOM_DIGITS);
        if check != [luhn_check_digit(random)] {
            return Err(SetupCodeError::CheckDigitMismatch);
        }
        Ok(Self(
            digits
                .iter()
                .map(|&digit| char::from(b'0' + digit))
                .collect(),
        ))
    }

    /// The eleven digits, as the QR and the exchange carry them.
    pub fn digits(&self) -> &str {
        &self.0
    }

    /// The card's `4-3-4` grouping, `3497-011-2339`.
    pub fn display(&self) -> String {
        let d = &self.0;
        format!("{}-{}-{}", &d[..4], &d[4..7], &d[7..])
    }
}

impl fmt::Debug for SetupCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SetupCode(<redacted>)")
    }
}

impl Serialize for SetupCode {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for SetupCode {
    /// Through [`SetupCode::parse`], so a stored code with a bad check digit is refused, not used.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

/// Luhn mod 10: the digit that, appended to `payload`, makes the weighted sum a multiple of ten.
fn luhn_check_digit(payload: &[u8]) -> u8 {
    let sum: u32 = payload
        .iter()
        .rev()
        .enumerate()
        .map(|(position, &digit)| {
            let digit = u32::from(digit);
            if position % 2 == 0 {
                let doubled = digit * 2;
                if doubled > 9 {
                    doubled - 9
                } else {
                    doubled
                }
            } else {
                digit
            }
        })
        .sum();
    ((10 - sum % 10) % 10) as u8
}

#[cfg(test)]
mod tests {
    use super::*;

    const LUHN_VECTOR: [u8; 10] = [7, 9, 9, 2, 7, 3, 9, 8, 7, 1];

    fn code(text: &str) -> SetupCode {
        SetupCode::parse(text).unwrap()
    }

    #[test]
    fn the_classic_luhn_vector_gets_its_check_digit() {
        let made = SetupCode::from_digits(LUHN_VECTOR).unwrap();
        assert_eq!(made.digits(), "79927398713");
        assert_eq!(
            SetupCode::from_digits([0; 10]).unwrap().digits(),
            "00000000000"
        );
        assert_eq!(
            SetupCode::from_digits([1, 2, 3, 4, 5, 6, 7, 8, 9, 0])
                .unwrap()
                .digits(),
            "12345678903"
        );
    }

    #[test]
    fn a_random_digit_above_nine_is_refused() {
        let mut digits = LUHN_VECTOR;
        digits[4] = 10;
        assert_eq!(
            SetupCode::from_digits(digits),
            Err(SetupCodeError::DigitOutOfRange)
        );
    }

    #[test]
    fn every_single_digit_substitution_is_a_check_digit_mismatch() {
        let valid = SetupCode::from_digits(LUHN_VECTOR).unwrap();
        let digits: Vec<char> = valid.digits().chars().collect();
        for position in 0..SETUP_CODE_LEN {
            for replacement in '0'..='9' {
                if replacement == digits[position] {
                    continue;
                }
                let mut typo = digits.clone();
                typo[position] = replacement;
                let typo: String = typo.into_iter().collect();
                assert_eq!(
                    SetupCode::parse(&typo),
                    Err(SetupCodeError::CheckDigitMismatch),
                    "a substitution at position {position} went unnoticed"
                );
            }
        }
    }

    #[test]
    fn every_adjacent_transposition_is_caught_except_luhns_09_blind_spot() {
        let samples = [
            LUHN_VECTOR,
            [0, 9, 1, 2, 3, 4, 5, 6, 7, 8],
            [3, 4, 9, 7, 0, 1, 1, 2, 3, 3],
            [9, 0, 5, 5, 0, 9, 2, 8, 6, 4],
        ];
        let mut blind_spots = 0;
        for random in samples {
            let valid = SetupCode::from_digits(random).unwrap();
            let digits: Vec<u8> = valid.digits().bytes().collect();
            for i in 0..SETUP_CODE_LEN - 1 {
                if digits[i] == digits[i + 1] {
                    continue;
                }
                let mut swapped = digits.clone();
                swapped.swap(i, i + 1);
                let swapped = String::from_utf8(swapped).unwrap();
                let pair = [digits[i], digits[i + 1]];
                if pair == *b"09" || pair == *b"90" {
                    blind_spots += 1;
                    assert!(
                        SetupCode::parse(&swapped).is_ok(),
                        "Luhn is documented to miss 09/90; if this changed, so did the algorithm"
                    );
                } else {
                    assert_eq!(
                        SetupCode::parse(&swapped),
                        Err(SetupCodeError::CheckDigitMismatch),
                        "swapping positions {i} and {} went unnoticed",
                        i + 1
                    );
                }
            }
        }
        assert!(blind_spots > 0, "the samples never exercised the 09 pair");
    }

    #[test]
    fn parse_accepts_the_card_grouping_and_spaces() {
        let plain = code("34970112339");
        assert_eq!(code("3497-011-2339"), plain);
        assert_eq!(code("3497 011 2339"), plain);
        assert_eq!(code(" 3497 - 011 - 2339 "), plain);
    }

    #[test]
    fn parse_refuses_the_wrong_number_of_digits() {
        assert_eq!(
            SetupCode::parse("3497011233"),
            Err(SetupCodeError::WrongLength { found: 10 })
        );
        assert_eq!(
            SetupCode::parse("349701123390"),
            Err(SetupCodeError::WrongLength { found: 12 })
        );
        assert_eq!(
            SetupCode::parse(""),
            Err(SetupCodeError::WrongLength { found: 0 })
        );
    }

    #[test]
    fn parse_refuses_letters_and_non_ascii_digits() {
        assert_eq!(
            SetupCode::parse("3497-0l1-2339"),
            Err(SetupCodeError::NotDigits)
        );
        assert_eq!(
            SetupCode::parse("3497_011_2339"),
            Err(SetupCodeError::NotDigits)
        );
        assert_eq!(
            SetupCode::parse("\u{FF13}497-011-2339"),
            Err(SetupCodeError::NotDigits)
        );
    }

    #[test]
    fn display_groups_four_three_four_and_parses_back() {
        let made = SetupCode::from_digits([3, 4, 9, 7, 0, 1, 1, 2, 3, 3]).unwrap();
        assert_eq!(made.display(), "3497-011-2339");
        assert_eq!(SetupCode::parse(&made.display()).unwrap(), made);
        assert_eq!(SetupCode::parse(made.digits()).unwrap(), made);
    }

    #[test]
    fn debug_never_shows_the_digits() {
        let made = SetupCode::from_digits(LUHN_VECTOR).unwrap();
        let debug = format!("{made:?} {made:#?}");
        assert!(!debug.contains(made.digits()), "{debug}");
        assert!(!debug.contains(&made.display()), "{debug}");
        assert!(
            !debug.chars().any(|c| c.is_ascii_digit()),
            "a fragment of the code leaked: {debug}"
        );
    }

    #[test]
    fn serde_stores_the_eleven_digits_and_refuses_a_bad_check_digit() {
        let made = SetupCode::from_digits(LUHN_VECTOR).unwrap();
        let json = serde_json::to_string(&made).unwrap();
        assert_eq!(json, "\"79927398713\"");
        assert_eq!(serde_json::from_str::<SetupCode>(&json).unwrap(), made);

        let err = serde_json::from_str::<SetupCode>("\"79927398714\"").unwrap_err();
        assert!(err.to_string().contains("check digit"), "{err}");
        assert!(
            !err.to_string().contains("7992739871"),
            "the refused code is in the error: {err}"
        );
    }
}
