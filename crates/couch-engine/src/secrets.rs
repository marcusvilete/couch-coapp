//! Encrypting what the engine keeps on disk: the identity key and remembered
//! rooms (the password key of each room we own).

use std::io;

/// Seals data so only this user can read it back.
pub trait SecretStore: Send + Sync {
    fn protect(&self, data: &[u8]) -> io::Result<Vec<u8>>;
    fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>>;
}

/// DPAPI, tied to the current Windows user.
#[cfg(windows)]
#[derive(Debug, Default, Clone, Copy)]
pub struct Dpapi;

#[cfg(windows)]
const DPAPI_ENTROPY: &[u8] = b"couch-coapp/secrets/v1";

#[cfg(windows)]
impl SecretStore for Dpapi {
    fn protect(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        couch_platform_win::dpapi::protect(data, DPAPI_ENTROPY)
    }

    fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>> {
        couch_platform_win::dpapi::unprotect(sealed, DPAPI_ENTROPY)
    }
}

/// Stores data as it is. For tests, and on Linux, where the profile's files
/// are their owner's alone instead (0600 in 0700 folders, `store::
/// write_atomic`; ADR 0011).
#[derive(Debug, Default, Clone, Copy)]
pub struct Unprotected;

impl SecretStore for Unprotected {
    fn protect(&self, data: &[u8]) -> io::Result<Vec<u8>> {
        Ok(data.to_vec())
    }

    fn unprotect(&self, sealed: &[u8]) -> io::Result<Vec<u8>> {
        Ok(sealed.to_vec())
    }
}

/// The platform's store: DPAPI on Windows; elsewhere the files' own
/// permissions protect them (ADR 0011).
pub fn platform_store() -> std::sync::Arc<dyn SecretStore> {
    #[cfg(windows)]
    {
        std::sync::Arc::new(Dpapi)
    }
    #[cfg(not(windows))]
    {
        tracing::debug!(
            "the identity key and remembered rooms are kept in files only this user can read (0600)"
        );
        std::sync::Arc::new(Unprotected)
    }
}
