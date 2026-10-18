//! Opening and reusing sessions.
//!
//! Browsing uses short-lived, read-only sessions shared per connection and
//! database, so a long console query never blocks navigation. Each console
//! owns its own retained session. Both kinds are bounded.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Instant;

use qwerio_core::connection::{ConnectionDescriptor, Credentials};
use qwerio_core::driver::{DatabaseSession, OpenSessionRequest};
use qwerio_core::error::{DatabaseError, DbResult, ErrorKind};
use qwerio_core::ids::ConnectionId;
use qwerio_core::session::{AccessMode, SessionPurpose};
use tokio::sync::OnceCell;

use crate::registry::DriverRegistry;

pub const MAX_BROWSE_SESSIONS: usize = 8;
pub const MAX_CONSOLE_SESSIONS: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct BrowseKey {
    connection: ConnectionId,
    database: Option<String>,
}

/// A browsing session that is opened at most once, by its first user.
type SharedSession = Arc<OnceCell<Arc<dyn DatabaseSession>>>;

struct BrowseSlot {
    session: SharedSession,
    last_used: Instant,
}

pub struct SessionRegistry {
    drivers: Arc<DriverRegistry>,
    browse: Mutex<HashMap<BrowseKey, BrowseSlot>>,
    consoles: Mutex<usize>,
}

/// A console session that releases its slot when dropped. Close it
/// explicitly (after rolling back) to end the connection promptly.
pub struct ConsoleLease {
    session: Arc<dyn DatabaseSession>,
    registry: Arc<SessionRegistry>,
}

impl ConsoleLease {
    pub fn session(&self) -> &Arc<dyn DatabaseSession> {
        &self.session
    }
}

impl Drop for ConsoleLease {
    fn drop(&mut self) {
        let mut consoles = self
            .registry
            .consoles
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *consoles = consoles.saturating_sub(1);
    }
}

impl SessionRegistry {
    pub fn new(drivers: Arc<DriverRegistry>) -> Self {
        Self {
            drivers,
            browse: Mutex::new(HashMap::new()),
            consoles: Mutex::new(0),
        }
    }

    pub fn drivers(&self) -> &Arc<DriverRegistry> {
        &self.drivers
    }

    async fn open(
        &self,
        connection: ConnectionDescriptor,
        credentials: Credentials,
        database: Option<String>,
        purpose: SessionPurpose,
        access: AccessMode,
    ) -> DbResult<Arc<dyn DatabaseSession>> {
        let driver = self
            .drivers
            .driver(connection.engine())
            .map_err(|error| DatabaseError::new(ErrorKind::Unsupported, error.to_string()))?;

        driver
            .open(OpenSessionRequest {
                connection,
                credentials,
                purpose,
                access,
                database,
            })
            .await
    }

    /// The shared read-only browsing session for a connection and database,
    /// opened on first use. Concurrent callers share one opening attempt.
    pub async fn browse(
        &self,
        connection: &ConnectionDescriptor,
        credentials: &Credentials,
        database: Option<String>,
    ) -> DbResult<Arc<dyn DatabaseSession>> {
        let key = BrowseKey {
            connection: connection.id,
            database: database.clone(),
        };
        let (cell, evicted) = self.browse_slot(key);

        if let Some(evicted) = evicted {
            close_quietly(evicted).await;
        }

        let session = cell
            .get_or_try_init(|| {
                self.open(
                    connection.clone(),
                    credentials.clone(),
                    database,
                    SessionPurpose::Browse,
                    AccessMode::ReadOnly,
                )
            })
            .await?;

        Ok(Arc::clone(session))
    }

    /// Returns the slot for `key`, evicting the least recently used session
    /// when the bound is reached.
    fn browse_slot(&self, key: BrowseKey) -> (SharedSession, Option<Arc<dyn DatabaseSession>>) {
        let mut browse = self.browse.lock().unwrap_or_else(PoisonError::into_inner);

        if let Some(slot) = browse.get_mut(&key) {
            slot.last_used = Instant::now();
            return (Arc::clone(&slot.session), None);
        }

        let mut evicted = None;

        if browse.len() >= MAX_BROWSE_SESSIONS {
            let oldest = browse
                .iter()
                .min_by_key(|(_, slot)| slot.last_used)
                .map(|(key, _)| key.clone());

            if let Some(oldest) = oldest {
                evicted = browse
                    .remove(&oldest)
                    .and_then(|slot| slot.session.get().cloned());
            }
        }

        let cell = Arc::new(OnceCell::new());
        browse.insert(
            key,
            BrowseSlot {
                session: Arc::clone(&cell),
                last_used: Instant::now(),
            },
        );

        (cell, evicted)
    }

    /// Forgets a browsing session after a connection failure so the next
    /// request reconnects. Browsing sessions hold no transactions, so this
    /// never resumes interrupted work.
    pub async fn discard_browse(&self, connection: ConnectionId, database: Option<String>) {
        let removed = self
            .browse
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&BrowseKey {
                connection,
                database,
            })
            .and_then(|slot| slot.session.get().cloned());

        if let Some(session) = removed {
            close_quietly(session).await;
        }
    }

    /// Closes every browsing session of a connection, for example after its
    /// binding changed.
    pub async fn close_connection(&self, connection: ConnectionId) {
        let removed: Vec<Arc<dyn DatabaseSession>> = {
            let mut browse = self.browse.lock().unwrap_or_else(PoisonError::into_inner);
            let keys: Vec<BrowseKey> = browse
                .keys()
                .filter(|key| key.connection == connection)
                .cloned()
                .collect();

            keys.iter()
                .filter_map(|key| browse.remove(key))
                .filter_map(|slot| slot.session.get().cloned())
                .collect()
        };

        for session in removed {
            close_quietly(session).await;
        }
    }

    /// Opens a dedicated console session with the access the user chose.
    pub async fn open_console(
        self: &Arc<Self>,
        connection: ConnectionDescriptor,
        credentials: Credentials,
        database: Option<String>,
        access: AccessMode,
    ) -> DbResult<ConsoleLease> {
        {
            let mut consoles = self.consoles.lock().unwrap_or_else(PoisonError::into_inner);

            if *consoles >= MAX_CONSOLE_SESSIONS {
                return Err(DatabaseError::new(
                    ErrorKind::SessionBusy,
                    format!("Close a console first; at most {MAX_CONSOLE_SESSIONS} can be open"),
                ));
            }

            *consoles += 1;
        }

        // The lease releases the slot again if opening fails.
        let opened = self
            .open(
                connection,
                credentials,
                database,
                SessionPurpose::Console,
                access,
            )
            .await;

        match opened {
            Ok(session) => Ok(ConsoleLease {
                session,
                registry: Arc::clone(self),
            }),
            Err(error) => {
                let mut consoles = self.consoles.lock().unwrap_or_else(PoisonError::into_inner);
                *consoles = consoles.saturating_sub(1);
                Err(error)
            }
        }
    }

    /// Opens and closes a session to check options and credentials.
    pub async fn test_connection(
        &self,
        connection: ConnectionDescriptor,
        credentials: Credentials,
    ) -> DbResult<Option<String>> {
        let session = self
            .open(
                connection,
                credentials,
                None,
                SessionPurpose::Browse,
                AccessMode::ReadOnly,
            )
            .await?;
        let version = session.info().server_version.clone();
        session.close().await?;
        Ok(version)
    }

    /// Closes all browsing sessions, for shutdown.
    pub async fn close_all(&self) {
        let sessions: Vec<Arc<dyn DatabaseSession>> = self
            .browse
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain()
            .filter_map(|(_, slot)| slot.session.get().cloned())
            .collect();

        for session in sessions {
            close_quietly(session).await;
        }
    }
}

/// Closing a session that is being discarded can only fail because its
/// connection is already gone; there is nothing further to release.
async fn close_quietly(session: Arc<dyn DatabaseSession>) {
    let _ = session.close().await;
}
