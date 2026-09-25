//! Spoken-English times for voice prompts: small models mis-say minute "23" as "oh three".

/// Speak a 24-hour time, e.g. `(5, 23)` -> "five twenty-three in the morning"; input is clamped.
pub fn spoken_time(hour24: u32, minute: u32) -> String {
    let hour24 = hour24.min(23);
    let minute = minute.min(59);

    if hour24 == 0 && minute == 0 {
        return "midnight".to_string();
    }
    if hour24 == 12 && minute == 0 {
        return "noon".to_string();
    }

    let h12 = match hour24 {
        0 => 12,
        h if h > 12 => h - 12,
        h => h,
    };
    let period = if hour24 < 12 {
        "in the morning"
    } else if hour24 < 17 {
        "in the afternoon"
    } else {
        "in the evening"
    };

    let hour_word = number_word(h12);
    let minute_phrase = match minute {
        0 => "o'clock".to_string(),
        1..=9 => format!("oh {}", number_word(minute)),
        _ => number_word(minute).to_string(),
    };

    format!("{hour_word} {minute_phrase} {period}")
}

/// English word for `0..=59`, e.g. `23 -> "twenty-three"`.
fn number_word(n: u32) -> std::borrow::Cow<'static, str> {
    const ONES: [&str; 20] = [
        "zero",
        "one",
        "two",
        "three",
        "four",
        "five",
        "six",
        "seven",
        "eight",
        "nine",
        "ten",
        "eleven",
        "twelve",
        "thirteen",
        "fourteen",
        "fifteen",
        "sixteen",
        "seventeen",
        "eighteen",
        "nineteen",
    ];
    if (n as usize) < ONES.len() {
        return std::borrow::Cow::Borrowed(ONES[n as usize]);
    }
    let (tens, ones) = (n / 10, n % 10);
    let tens_word = match tens {
        2 => "twenty",
        3 => "thirty",
        4 => "forty",
        5 => "fifty",
        _ => "fifty", // n is clamped to 0..=59 by callers, so tens is always 2..=5
    };
    if ones == 0 {
        std::borrow::Cow::Borrowed(tens_word)
    } else {
        std::borrow::Cow::Owned(format!("{tens_word}-{}", ONES[ones as usize]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midnight_and_noon_are_special_cased() {
        assert_eq!(spoken_time(0, 0), "midnight");
        assert_eq!(spoken_time(12, 0), "noon");
    }

    #[test]
    fn the_bug_report_case_is_correct() {
        // 5:23 must say "twenty-three", never "oh three".
        assert_eq!(spoken_time(5, 23), "five twenty-three in the morning");
    }

    #[test]
    fn single_digit_minutes_get_the_oh_prefix() {
        assert_eq!(spoken_time(5, 3), "five oh three in the morning");
    }

    #[test]
    fn zero_minutes_use_oclock() {
        assert_eq!(spoken_time(5, 0), "five o'clock in the morning");
    }

    #[test]
    fn teen_minutes_are_said_plainly() {
        assert_eq!(spoken_time(0, 15), "twelve fifteen in the morning");
    }

    #[test]
    fn afternoon_and_evening_periods() {
        assert_eq!(spoken_time(13, 45), "one forty-five in the afternoon");
        assert_eq!(spoken_time(17, 23), "five twenty-three in the evening");
    }

    #[test]
    fn round_tens_have_no_trailing_hyphen() {
        assert_eq!(spoken_time(9, 40), "nine forty in the morning");
    }

    #[test]
    fn out_of_range_inputs_are_clamped_not_panicking() {
        assert_eq!(spoken_time(99, 99), spoken_time(23, 59));
    }
}
