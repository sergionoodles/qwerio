//! Opaque identifiers used across services.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! uuid_id {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                Self(uuid)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}

uuid_id!(
    /// A logical connection alias. Shared workspaces refer to connections by this
    /// ID; machine-local settings bind it to an endpoint or file.
    ConnectionId
);
uuid_id!(
    /// One open physical session (connection or SQLite worker).
    SessionId
);
uuid_id!(
    /// One execution request. Events for superseded requests are ignored.
    RequestId
);
uuid_id!(
    /// One result set inside a request.
    ResultId
);
