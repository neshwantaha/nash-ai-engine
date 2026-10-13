//! SQLite-backed persistence for the Nash AI Engine.
//!
//! Two concerns live here:
//! * [`db::Store`] — the sessions/messages/settings database, with versioned
//!   migrations. Configuration is stored as JSON (never secrets).
//! * [`secrets`] — a secret store backed by the OS keyring (with an in-memory
//!   implementation for tests), so credentials never touch the database.

pub mod db;
pub mod secrets;
pub mod sessions;
pub mod settings;
pub mod usage;

pub use db::{Store, SCHEMA_VERSION};
pub use secrets::{MemorySecretStore, SecretStore};
#[cfg(feature = "os-keyring")]
pub use secrets::KeyringSecretStore;
pub use sessions::{Session, StoredMessage};
pub use usage::{UsageRecord, UsageTotals};
