//! Credential storage in the platform keychain: Keychain on macOS, the
//! Credential Manager on Windows, and the Secret Service on Linux.

use qwerio_app::AppError;
use qwerio_app::AppResult;
use qwerio_app::credentials::{CredentialRef, CredentialStore};
use qwerio_core::connection::Secret;

use super::paths::APPLICATION_ID;

pub struct KeychainCredentialStore;

fn entry(reference: &CredentialRef) -> AppResult<keyring::Entry> {
    keyring::Entry::new(APPLICATION_ID, reference.as_str()).map_err(credential_error)
}

fn credential_error(error: keyring::Error) -> AppError {
    AppError::Credentials(error.to_string())
}

impl CredentialStore for KeychainCredentialStore {
    fn load(&self, reference: &CredentialRef) -> AppResult<Option<Secret>> {
        match entry(reference)?.get_password() {
            Ok(password) => Ok(Some(Secret::new(password))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(error) => Err(credential_error(error)),
        }
    }

    fn store(&self, reference: &CredentialRef, secret: &Secret) -> AppResult<()> {
        entry(reference)?
            .set_password(secret.expose())
            .map_err(credential_error)
    }

    fn delete(&self, reference: &CredentialRef) -> AppResult<()> {
        match entry(reference)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(error) => Err(credential_error(error)),
        }
    }
}
