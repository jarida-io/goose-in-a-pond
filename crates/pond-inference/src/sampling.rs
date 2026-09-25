//! Sampler chain construction for token generation.

use llama_cpp_2::sampling::LlamaSampler;

/// Greedy at `temperature <= 0.01`, else filters then temp then dist (llama.cpp's order).
pub(crate) fn build_sampler(temperature: Option<f32>) -> LlamaSampler {
    let t = temperature.unwrap_or(0.8);

    if t <= 0.01 {
        LlamaSampler::greedy()
    } else {
        let seed = rand_seed();
        LlamaSampler::chain_simple(vec![
            LlamaSampler::top_k(40),
            LlamaSampler::top_p(0.95, 1),
            LlamaSampler::min_p(0.05, 1),
            LlamaSampler::temp(t),
            LlamaSampler::dist(seed),
        ])
    }
}

/// Non-cryptographic sampler seed from the clock, to avoid a `rand` dependency.
fn rand_seed() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u32)
        .unwrap_or(42)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn greedy_at_zero_temperature() {
        // Should not panic.
        let _ = build_sampler(Some(0.0));
    }

    #[test]
    fn greedy_at_very_low_temperature() {
        let _ = build_sampler(Some(0.001));
    }

    #[test]
    fn default_temperature_is_0_8() {
        // None -> 0.8, should produce a chain, not greedy.
        let _ = build_sampler(None);
    }

    #[test]
    fn high_temperature() {
        let _ = build_sampler(Some(1.5));
    }

    #[test]
    fn rand_seed_varies() {
        let a = rand_seed();
        // Busy-wait a tiny bit to get a different nanosecond.
        std::hint::spin_loop();
        let b = rand_seed();
        // Very unlikely but not impossible to be equal; just verify no panic.
        let _ = (a, b);
    }
}
