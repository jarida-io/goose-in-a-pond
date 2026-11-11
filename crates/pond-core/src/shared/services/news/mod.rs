//! A member's news, written on the inference lane from their interests: headline stories a few
//! times a day and a digest once a day. The contract is section 2 of
//! `docs/architecture/my-goose-hub.md`.

pub mod coverage;
pub mod dedupe;
pub mod prompts;
pub mod schedule;
pub mod speaker;
pub mod speech;
pub mod topic_map;
pub mod wav;
pub mod writer;
