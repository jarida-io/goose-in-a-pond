//! Discovery, setup and claiming of a pond (`docs/architecture/pond-networking.md`). The policy
//! lives here; radios and files live in adapters.

pub mod domain;
pub mod ports;
pub mod services;

#[cfg(any(test, feature = "test-mocks"))]
pub mod mocks;
