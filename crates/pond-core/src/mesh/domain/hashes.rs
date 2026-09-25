//! Hashes pinning mesh peers to a known-good harness/model; blake3 lives in `pond-mesh-protocol`.

use serde::{Deserialize, Serialize};

macro_rules! hash_newtype {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        pub struct $name([u8; 32]);

        impl $name {
            pub fn as_bytes(&self) -> &[u8; 32] {
                &self.0
            }
        }

        impl From<[u8; 32]> for $name {
            fn from(bytes: [u8; 32]) -> Self {
                Self(bytes)
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                for byte in &self.0 {
                    write!(f, "{byte:02x}")?;
                }
                Ok(())
            }
        }
    };
}

hash_newtype!(HarnessHash);
hash_newtype!(ModelHash);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_and_model_hash_are_distinct_types() {
        let harness = HarnessHash::from([1u8; 32]);
        let model = ModelHash::from([1u8; 32]);
        // Distinct types: `assert_eq!(harness, model)` does not compile.
        assert_eq!(harness.as_bytes(), model.as_bytes());
    }

    #[test]
    fn display_is_lowercase_hex() {
        let hash = ModelHash::from([0xffu8; 32]);
        assert_eq!(hash.to_string(), "ff".repeat(32));
    }
}
