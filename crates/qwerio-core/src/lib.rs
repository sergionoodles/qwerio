//! Engine-neutral database contracts shared by the Qwerio adapters and services.
//!
//! This crate has no GPUI, filesystem, driver, or credential-store dependency.
//! Adapters implement [`driver::DatabaseDriver`] and [`driver::DatabaseSession`];
//! shared code owns the meaning of filters, ordering, and paging while adapters
//! own SQL dialect details through [`sql::SqlDialect`].

pub mod capabilities;
pub mod catalog;
pub mod cell_input;
pub mod connection;
pub mod connection_string;
#[cfg(feature = "contract-tests")]
pub mod contract;
pub mod ddl;
pub mod display;
pub mod driver;
pub mod edit;
pub mod error;
pub mod export;
pub mod filter;
pub mod ids;
pub mod paging;
pub mod permissions;
pub mod relationship;
pub mod resource;
pub mod result;
pub mod session;
pub mod sql;
pub mod sql_text;
pub mod value;

pub use error::{DatabaseError, DbResult, ErrorKind};
pub use value::Value;
