//! Self-contained workspace files. Only ciphertext is ever written for an encrypted file.
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read as _;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

use qwerio_core::connection::{ConnectionOptions, Secret};
use qwerio_core::ids::ConnectionId;
use ring::{
    aead, digest, pbkdf2,
    rand::{SecureRandom as _, SystemRandom},
};
use serde::{Deserialize, Serialize};

use crate::document::{Fingerprint, SaveExpectation, save_bytes};
use crate::manifest::{DocumentId, Manifest};
use crate::{WorkspaceError, WorkspaceResult};

const MAGIC: &[u8; 8] = b"QWERIO\0\x01";
const PLAIN: u8 = 0;
const ENCRYPTED: u8 = 1;
const SALT_LENGTH: usize = 16;
const NONCE_LENGTH: usize = 12;
const KEY_LENGTH: usize = 32;
const ITERATIONS: u32 = 600_000;
const PREFIX_LENGTH: usize = MAGIC.len() + 1;
const SALT_END: usize = PREFIX_LENGTH + SALT_LENGTH;
const ENCRYPTED_HEADER_LENGTH: usize = SALT_END + NONCE_LENGTH;
const PLAIN_HEADER_LENGTH: usize = PREFIX_LENGTH + digest::SHA256_OUTPUT_LEN;
pub const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StoredConnection {
    pub id: ConnectionId,
    pub options: ConnectionOptions,
    #[serde(default, with = "optional_secret")]
    pub password: Option<Secret>,
}

mod optional_secret {
    use super::*;
    pub fn serialize<S: serde::Serializer>(
        value: &Option<Secret>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.as_ref().map(Secret::expose).serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Secret>, D::Error> {
        Ok(Option::<String>::deserialize(deserializer)?.map(Secret::new))
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct Contents {
    pub manifest: Manifest,
    #[serde(default)]
    pub documents: BTreeMap<String, String>,
    #[serde(default)]
    pub drafts: BTreeMap<String, String>,
    #[serde(default)]
    pub connections: Vec<StoredConnection>,
    #[serde(default)]
    pub ui_state: Option<String>,
}

impl Contents {
    pub fn new(manifest: Manifest) -> Self {
        Self {
            manifest,
            documents: BTreeMap::new(),
            drafts: BTreeMap::new(),
            connections: Vec::new(),
            ui_state: None,
        }
    }

    fn validate(&self) -> WorkspaceResult<()> {
        Manifest::parse(&self.manifest.to_toml()?).map_err(|_| WorkspaceError::InvalidFile)?;
        for document in &self.manifest.documents {
            if !crate::paths::is_plain_relative(Path::new(&document.path)) {
                return Err(WorkspaceError::InvalidFile);
            }
            if !self.documents.contains_key(&document.id.to_string()) {
                return Err(WorkspaceError::InvalidFile);
            }
        }
        if self.connections.iter().any(|stored| {
            self.manifest
                .connection(stored.id)
                .is_none_or(|alias| alias.engine != stored.options.engine())
        }) {
            return Err(WorkspaceError::InvalidFile);
        }
        Ok(())
    }
}

struct Protection {
    salt: [u8; SALT_LENGTH],
    key: aead::LessSafeKey,
}

impl Protection {
    fn derive(password: &Secret, salt: [u8; SALT_LENGTH]) -> WorkspaceResult<Self> {
        let iterations = NonZeroU32::new(ITERATIONS).ok_or(WorkspaceError::Encryption)?;
        let mut bytes = [0; KEY_LENGTH];
        pbkdf2::derive(
            pbkdf2::PBKDF2_HMAC_SHA256,
            iterations,
            &salt,
            password.expose().as_bytes(),
            &mut bytes,
        );
        let key = aead::UnboundKey::new(&aead::AES_256_GCM, &bytes)
            .map_err(|_| WorkspaceError::Encryption)?;
        bytes.fill(0);
        Ok(Self {
            salt,
            key: aead::LessSafeKey::new(key),
        })
    }

    fn new(password: &Secret) -> WorkspaceResult<Self> {
        if password.expose().is_empty() {
            return Err(WorkspaceError::PasswordRequired);
        }
        let mut salt = [0; SALT_LENGTH];
        SystemRandom::new()
            .fill(&mut salt)
            .map_err(|_| WorkspaceError::Encryption)?;
        Self::derive(password, salt)
    }
}

pub(crate) struct WorkspaceFile {
    pub path: PathBuf,
    pub contents: Contents,
    fingerprint: Fingerprint,
    protection: Option<Protection>,
}

impl std::fmt::Debug for WorkspaceFile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("WorkspaceFile")
            .field("path", &self.path)
            .field("encrypted", &self.is_encrypted())
            .finish_non_exhaustive()
    }
}

impl WorkspaceFile {
    pub fn create(
        path: &Path,
        contents: Contents,
        password: Option<&Secret>,
    ) -> WorkspaceResult<Self> {
        if path
            .extension()
            .is_none_or(|extension| extension != "qwerio")
        {
            return Err(WorkspaceError::InvalidFile);
        }
        contents.validate()?;
        let protection = password.map(Protection::new).transpose()?;
        let bytes = encode(&contents, protection.as_ref())?;
        let fingerprint = save_bytes(path, &bytes, SaveExpectation::New)?;
        Ok(Self {
            path: path.to_path_buf(),
            contents,
            fingerprint,
            protection,
        })
    }

    pub fn open(path: &Path, password: Option<&Secret>) -> WorkspaceResult<Self> {
        let bytes = read_bounded(path)?;
        let (contents, protection) = decode(&bytes, password)?;
        contents.validate()?;
        Ok(Self {
            path: path.to_path_buf(),
            contents,
            fingerprint: Fingerprint::of(&bytes),
            protection,
        })
    }

    pub fn is_encrypted(&self) -> bool {
        self.protection.is_some()
    }

    pub fn save(&mut self, contents: Contents) -> WorkspaceResult<()> {
        contents.validate()?;
        let bytes = encode(&contents, self.protection.as_ref())?;
        let fingerprint = save_bytes(
            &self.path,
            &bytes,
            SaveExpectation::Unchanged(self.fingerprint),
        )?;
        self.contents = contents;
        self.fingerprint = fingerprint;
        Ok(())
    }

    pub fn reload(&mut self) -> WorkspaceResult<()> {
        let bytes = read_bounded(&self.path)?;
        // Reuse the unlocked key, and refuse a changed encryption header.
        let text = decrypt(&bytes, self.protection.as_ref())?;
        let contents = parse_contents(&text)?;
        contents.validate()?;
        self.contents = contents;
        self.fingerprint = Fingerprint::of(&bytes);
        Ok(())
    }
}

pub fn requires_password(path: &Path) -> WorkspaceResult<bool> {
    let bytes = read_bounded(path)?;
    header(&bytes).map(|mode| mode == ENCRYPTED)
}

fn read_bounded(path: &Path) -> WorkspaceResult<Vec<u8>> {
    let file = File::open(path).map_err(|error| WorkspaceError::io(path, error))?;
    let mut bytes = Vec::new();
    file.take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| WorkspaceError::io(path, error))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(WorkspaceError::InvalidFile);
    }
    Ok(bytes)
}

fn encode(contents: &Contents, protection: Option<&Protection>) -> WorkspaceResult<Vec<u8>> {
    let mut payload = toml::to_string(contents)
        .map_err(|_| WorkspaceError::InvalidFile)?
        .into_bytes();
    let mut header = MAGIC.to_vec();
    match protection {
        Some(protection) => {
            let mut nonce = [0; NONCE_LENGTH];
            SystemRandom::new()
                .fill(&mut nonce)
                .map_err(|_| WorkspaceError::Encryption)?;
            header.push(ENCRYPTED);
            header.extend_from_slice(&protection.salt);
            header.extend_from_slice(&nonce);
            protection
                .key
                .seal_in_place_append_tag(
                    aead::Nonce::assume_unique_for_key(nonce),
                    aead::Aad::from(&header),
                    &mut payload,
                )
                .map_err(|_| WorkspaceError::Encryption)?;
        }
        None => {
            header.push(PLAIN);
            header.extend_from_slice(digest::digest(&digest::SHA256, &payload).as_ref());
        }
    }
    header.extend_from_slice(&payload);
    if header.len() as u64 > MAX_FILE_BYTES {
        return Err(WorkspaceError::InvalidFile);
    }
    Ok(header)
}

fn header(bytes: &[u8]) -> WorkspaceResult<u8> {
    if bytes.len() < PREFIX_LENGTH || !bytes.starts_with(MAGIC) {
        return Err(WorkspaceError::InvalidFile);
    }
    match bytes[MAGIC.len()] {
        mode @ (PLAIN | ENCRYPTED) => Ok(mode),
        _ => Err(WorkspaceError::InvalidFile),
    }
}

fn decode(
    bytes: &[u8],
    password: Option<&Secret>,
) -> WorkspaceResult<(Contents, Option<Protection>)> {
    let protection = if header(bytes)? == ENCRYPTED {
        let password = password.ok_or(WorkspaceError::PasswordRequired)?;
        let salt = bytes
            .get(PREFIX_LENGTH..SALT_END)
            .ok_or(WorkspaceError::InvalidFile)?
            .try_into()
            .map_err(|_| WorkspaceError::InvalidFile)?;
        Some(Protection::derive(password, salt)?)
    } else {
        None
    };
    Ok((
        parse_contents(&decrypt(bytes, protection.as_ref())?)?,
        protection,
    ))
}

fn decrypt(bytes: &[u8], protection: Option<&Protection>) -> WorkspaceResult<Vec<u8>> {
    if header(bytes)? == PLAIN {
        if protection.is_some() {
            return Err(WorkspaceError::Authentication);
        }
        let checksum = bytes
            .get(PREFIX_LENGTH..PLAIN_HEADER_LENGTH)
            .ok_or(WorkspaceError::InvalidFile)?;
        let payload = bytes
            .get(PLAIN_HEADER_LENGTH..)
            .ok_or(WorkspaceError::InvalidFile)?;
        if digest::digest(&digest::SHA256, payload).as_ref() != checksum {
            return Err(WorkspaceError::InvalidFile);
        }
        return Ok(payload.to_vec());
    }
    let protection = protection.ok_or(WorkspaceError::PasswordRequired)?;
    if bytes.get(PREFIX_LENGTH..SALT_END) != Some(protection.salt.as_slice()) {
        return Err(WorkspaceError::Authentication);
    }
    let nonce = bytes
        .get(SALT_END..ENCRYPTED_HEADER_LENGTH)
        .ok_or(WorkspaceError::InvalidFile)?
        .try_into()
        .map_err(|_| WorkspaceError::InvalidFile)?;
    let mut payload = bytes
        .get(ENCRYPTED_HEADER_LENGTH..)
        .ok_or(WorkspaceError::InvalidFile)?
        .to_vec();
    let text = protection
        .key
        .open_in_place(
            aead::Nonce::assume_unique_for_key(nonce),
            aead::Aad::from(&bytes[..ENCRYPTED_HEADER_LENGTH]),
            &mut payload,
        )
        .map_err(|_| WorkspaceError::Authentication)?;
    Ok(text.to_vec())
}

fn parse_contents(bytes: &[u8]) -> WorkspaceResult<Contents> {
    let text = std::str::from_utf8(bytes).map_err(|_| WorkspaceError::InvalidFile)?;
    toml::from_str(text).map_err(|_| WorkspaceError::InvalidFile)
}

pub(crate) fn document_key(id: DocumentId) -> String {
    id.to_string()
}
