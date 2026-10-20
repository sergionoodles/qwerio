//! Application services for Qwerio: use cases, the driver and session
//! registries, navigation history and tabs, table-browsing and console state,
//! and machine-local state. Nothing here depends on GPUI, so it is tested
//! without a graphical environment.

pub mod console;
pub mod credentials;
pub mod error;
pub mod local_state;
pub mod navigation;
pub mod registry;
pub mod requests;
pub mod schema;
pub mod services;
pub mod sessions;
pub mod table;
pub mod table_edit_session;
pub mod table_edits;
pub mod tabs;
pub mod templates;
pub mod workspace_store;

pub use error::{AppError, AppResult};
pub use services::AppServices;
