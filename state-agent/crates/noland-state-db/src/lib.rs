//! Ephemeral SQLite runtime database. Not the durable source of truth.

mod checkpoint;
mod persistence;
mod projections;
mod restore;
mod schema;
mod store;

pub use checkpoint::LearnedCheckpointGeneration;
pub use store::{RestoredPathAssociationInput, StateDb};

pub const SCHEMA_VERSION: i64 = 5;
