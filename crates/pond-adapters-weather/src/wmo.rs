//! WMO weather code → description, per https://open-meteo.com/en/docs ("WMO Weather Codes").

pub fn describe(code: u32) -> &'static str {
    match code {
        0 => "Clear sky",
        1 => "Mainly clear",
        2 => "Partly cloudy",
        3 => "Overcast",
        45 | 48 => "Fog",
        51 => "Light drizzle",
        53 => "Moderate drizzle",
        55 => "Dense drizzle",
        56 | 57 => "Freezing drizzle",
        61 => "Slight rain",
        63 => "Moderate rain",
        65 => "Heavy rain",
        66 | 67 => "Freezing rain",
        71 => "Slight snow",
        73 => "Moderate snow",
        75 => "Heavy snow",
        77 => "Snow grains",
        80 => "Slight showers",
        81 => "Moderate showers",
        82 => "Violent showers",
        85 | 86 => "Snow showers",
        95 => "Thunderstorm",
        96 | 99 => "Thunderstorm with hail",
        _ => "Unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clear() {
        assert_eq!(describe(0), "Clear sky");
    }
    #[test]
    fn rain() {
        assert_eq!(describe(63), "Moderate rain");
    }
    #[test]
    fn storm() {
        assert_eq!(describe(95), "Thunderstorm");
    }
    #[test]
    fn unknown() {
        assert_eq!(describe(200), "Unknown");
    }
}
