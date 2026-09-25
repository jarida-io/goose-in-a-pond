//! Private Pond Compute: a trust-scoped P2P inference mesh. Real transport, payment and ledger
//! adapters live in `pond-adapters-mesh-*` and `pond-infra`.

pub mod domain;
pub mod ports;
pub mod services;

#[cfg(any(test, feature = "test-mocks"))]
pub mod mocks;
