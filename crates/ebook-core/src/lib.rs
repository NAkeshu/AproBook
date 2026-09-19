//! Domain and persistence layer for AproBook.

mod database;
mod error;
mod importer;
mod library;
mod model;
pub mod reader;

pub use database::{BookQuery, BookSort, SortDirection};
pub use error::{CoreError, Result};
pub use importer::{ImportItemResult, ImportOutcome, ImportReport};
pub use library::{Library, RecoveryReport, RemovalPlan};
pub use model::*;

/// Current on-disk database and marker schema.
pub const SCHEMA_VERSION: u32 = 4;
