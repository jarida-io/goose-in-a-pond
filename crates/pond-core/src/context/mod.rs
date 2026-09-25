//! Personal context streaming: per-member, classified items governed by a retention window.

pub mod bus_ingest;
pub mod chunking;
pub mod domain;
pub mod index_maintenance;
pub mod ingest;
pub mod ports;
pub mod producer;
pub mod retention;
pub mod retrieval;
pub mod retrieval_service;
pub mod scope;
pub mod summary_indexing;
pub mod vector_index;

#[cfg(any(test, feature = "test-mocks"))]
pub mod mocks;
