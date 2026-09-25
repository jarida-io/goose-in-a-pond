//! Content hashing for mesh trust-pinning; the one place that runs blake3.

use pond_core::mesh::domain::hashes::{HarnessHash, ModelHash};

pub fn hash_harness(bytes: &[u8]) -> HarnessHash {
    HarnessHash::from(*blake3::hash(bytes).as_bytes())
}

pub fn hash_model(bytes: &[u8]) -> ModelHash {
    ModelHash::from(*blake3::hash(bytes).as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_bytes_hash_identically() {
        assert_eq!(hash_harness(b"harness-v1"), hash_harness(b"harness-v1"));
    }

    #[test]
    fn different_bytes_hash_differently() {
        assert_ne!(hash_harness(b"harness-v1"), hash_harness(b"harness-v2"));
    }

    #[test]
    fn harness_and_model_hash_of_same_bytes_carry_same_digest() {
        // Distinct types, same digest: the type system, not the hash, keeps them apart.
        let bytes = b"some-weights";
        assert_eq!(hash_harness(bytes).as_bytes(), hash_model(bytes).as_bytes());
    }
}
