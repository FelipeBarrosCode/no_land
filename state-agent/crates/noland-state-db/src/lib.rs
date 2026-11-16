//! Ephemeral SQLite runtime database. Not the durable source of truth.

mod checkpoint;
mod metadata_cache;
mod persistence;
mod projections;
mod restore;
mod schema;
mod store;

pub use checkpoint::LearnedCheckpointGeneration;
pub use metadata_cache::CachedMetadata;
pub use store::{RestoredPathAssociationInput, StateDb};

pub const SCHEMA_VERSION: i64 = 7;

#[cfg(test)]
mod verification_index_tests;
