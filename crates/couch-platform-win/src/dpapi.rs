//! DPAPI: Windows' per-user encryption for secrets kept on disk (the identity
//! key, remembered rooms). Only the same Windows user on the same machine can
//! decrypt, and no key is ever stored by the app itself.

use std::{ffi::c_void, io, ptr, slice};

use windows::Win32::{
    Foundation::{HLOCAL, LocalFree},
    Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
    },
};
use windows::core::PCWSTR;

fn blob(data: &[u8]) -> io::Result<CRYPT_INTEGER_BLOB> {
    Ok(CRYPT_INTEGER_BLOB {
        cbData: u32::try_from(data.len()).map_err(|_| io::Error::other("data too large"))?,
        // DPAPI only reads input blobs; the pointer is never written through.
        pbData: data.as_ptr().cast_mut(),
    })
}

/// Copies a blob DPAPI allocated, then frees it with `LocalFree` as the API
/// requires.
///
/// # Safety
/// `out` must have been filled in by a successful DPAPI call.
unsafe fn take(out: CRYPT_INTEGER_BLOB) -> Vec<u8> {
    if out.pbData.is_null() {
        return Vec::new();
    }
    // SAFETY: DPAPI returned `cbData` readable bytes at `pbData`.
    let data = unsafe { slice::from_raw_parts(out.pbData, out.cbData as usize) }.to_vec();
    // SAFETY: the buffer came from DPAPI's LocalAlloc and is freed exactly once,
    // after the copy above.
    unsafe { LocalFree(Some(HLOCAL(out.pbData.cast::<c_void>()))) };
    data
}

/// Encrypts `data` for the current Windows user. `entropy` is mixed in and
/// must be given again to decrypt, which keeps other apps of the same user
/// from decrypting by accident.
pub fn protect(data: &[u8], entropy: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(data)?;
    let entropy = blob(entropy)?;
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: the input blobs point at live slices for the duration of the
    // call; `out` receives a LocalAlloc'd buffer that `take` frees.
    unsafe {
        CryptProtectData(
            &input,
            PCWSTR::null(),
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| io::Error::other(format!("CryptProtectData: {e}")))?;
        Ok(take(out))
    }
}

/// Decrypts what [`protect`] produced for this user with the same `entropy`.
pub fn unprotect(blob_bytes: &[u8], entropy: &[u8]) -> io::Result<Vec<u8>> {
    let input = blob(blob_bytes)?;
    let entropy = blob(entropy)?;
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: ptr::null_mut(),
    };
    // SAFETY: as in `protect`. No description is requested, so nothing else
    // is allocated.
    unsafe {
        CryptUnprotectData(
            &input,
            None,
            Some(&entropy),
            None,
            None,
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
        .map_err(|e| io::Error::other(format!("CryptUnprotectData: {e}")))?;
        Ok(take(out))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_for_this_user() {
        let secret = b"room secret \x00\xff";
        let sealed = protect(secret, b"couch-test").unwrap();
        assert_ne!(&sealed[..], &secret[..]);
        assert_eq!(unprotect(&sealed, b"couch-test").unwrap(), secret);
    }

    #[test]
    fn wrong_entropy_or_tampering_fails() {
        let sealed = protect(b"key", b"couch-test").unwrap();
        assert!(unprotect(&sealed, b"other").is_err());
        let mut tampered = sealed.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert!(unprotect(&tampered, b"couch-test").is_err());
    }

    #[test]
    fn empty_data_roundtrips() {
        let sealed = protect(b"", b"e").unwrap();
        assert_eq!(unprotect(&sealed, b"e").unwrap(), b"");
    }
}
