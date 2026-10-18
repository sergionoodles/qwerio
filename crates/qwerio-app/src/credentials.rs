//! Credential storage boundary. The desktop provides a platform keychain
//! implementation; secrets never enter manifests, local state, or logs.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};

use qwerio_core::connection::Secret;
use qwerio_core::ids::ConnectionId;
use qwerio_workspace::manifest::WorkspaceId;
use serde::{Deserialize, Serialize};

use crate::error::AppResult;

/// The key under which a connection's secret is stored.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CredentialRef(String);

impl CredentialRef {
    pub fn for_connection(workspace: WorkspaceId, connection: ConnectionId) -> Self {
        Self(format!("workspace/{workspace}/connection/{connection}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Blocking calls; run them off the UI thread.
pub trait CredentialStore: Send + Sync {
    fn load(&self, reference: &CredentialRef) -> AppResult<Option<Secret>>;

    fn store(&self, reference: &CredentialRef, secret: &Secret) -> AppResult<()>;

    fn delete(&self, reference: &CredentialRef) -> AppResult<()>;
}

/// Keeps secrets for the lifetime of the process only. Used in tests and
/// when no platform keychain is available.
#[derive(Default)]
pub struct MemoryCredentialStore {
    secrets: Mutex<HashMap<CredentialRef, Secret>>,
}

impl CredentialStore for MemoryCredentialStore {
    fn load(&self, reference: &CredentialRef) -> AppResult<Option<Secret>> {
        let secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
        Ok(secrets.get(reference).cloned())
    }

    fn store(&self, reference: &CredentialRef, secret: &Secret) -> AppResult<()> {
        let mut secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
        secrets.insert(reference.clone(), secret.clone());
        Ok(())
    }

    fn delete(&self, reference: &CredentialRef) -> AppResult<()> {
        let mut secrets = self.secrets.lock().unwrap_or_else(PoisonError::into_inner);
        secrets.remove(reference);
        Ok(())
    }
}
