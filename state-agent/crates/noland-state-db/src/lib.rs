//! Ephemeral SQLite runtime database. Not the durable source of truth.

mod schema;
mod store;

pub use store::StateDb;

pub const SCHEMA_VERSION: i64 = 1;
