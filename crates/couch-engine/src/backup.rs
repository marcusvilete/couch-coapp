//! A profile's backup (M13): one file, sealed with a passphrase, that takes
//! a profile's identity, rooms and settings to another PC, or back after
//! Windows is reinstalled. DPAPI ties what the profile keeps on disk to this
//! Windows user, so a copy of the folder can't do it.
//!
//! - **What goes in:** the identity key, the remembered rooms and the
//!   settings files. Never an owned room's password key or invites: an old
//!   backup must not bring back an invite revoked since, or a password
//!   changed since. A restored owner sets a new password before inviting.
//!   Rooms on their way out (left, still telling the others) stay out too.
//! - **Sealed:** a key from the passphrase (trimmed and NFKC-normalized, as
//!   room passwords are) by Argon2id, then XChaCha20-Poly1305 over the body,
//!   with the header (format, Argon2's parameters, salt, nonce) as its
//!   associated data, so none of it can be changed unnoticed.
//! - **Restored** only into a profile with no rooms (`Store::restore`), each
//!   room catching up with its members before it signs anything
//!   (`couch_room::room::Room::catch_up_first`).
//! - **Where the file goes** is the app's own dialog's answer
//!   (`crate::BackupFiles`), never a path from the webview.
//!
//! The key, the passphrase and the opened body are zeroized when done, as
//! far as Rust lets us: copies the allocator or serde made on the way may
//! stay in memory until reused.

use std::path::Path;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroize;

use crate::store::RememberedRoom;

/// Shorter passphrases are refused: the file can be guessed at offline,
/// as fast as an attacker's hardware runs Argon2.
pub const MIN_PASSPHRASE_CHARS: usize = 12;

const MAGIC: &[u8; 4] = b"CCBK";
/// The format written: 2 since M13 (rooms with their chain of handovers);
/// format 1 still opens.
const FORMAT: u8 = 2;
const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 24;
const HEADER_LEN: usize = MAGIC.len() + 1 + 3 * 4 + SALT_LEN + NONCE_LEN;

/// Argon2id's cost for new backups: 64 MiB, 3 passes, one lane (about half
/// a second on the development PC).
const MEMORY_KIB: u32 = 64 * 1024;
const PASSES: u32 = 3;
const LANES: u32 = 1;
/// The most a file may ask for when it is opened: a crafted one must not
/// take the PC's memory or minutes.
const MAX_MEMORY_KIB: u32 = 512 * 1024;
const MAX_PASSES: u32 = 16;
const MAX_LANES: u32 = 8;
/// A profile is a few hundred kilobytes; anything far larger isn't one.
pub const MAX_FILE_LEN: usize = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BackupError {
    #[error("the passphrase must be at least {MIN_PASSPHRASE_CHARS} characters")]
    ShortPassphrase,
    #[error("that file isn't a Couch Co-App backup")]
    NotABackup,
    #[error("that backup was made by a newer Couch Co-App: update this one first")]
    NewerFormat,
    #[error("wrong passphrase, or the file is damaged")]
    WrongPassphrase,
    #[error("the backup couldn't be made: {0}")]
    Failed(String),
    /// Reading or writing the file; the text names it.
    #[error("{0}")]
    File(String),
}

/// Writes a sealed backup to `path` through a sibling file renamed over it,
/// so a failure never leaves half a backup where a good one was.
pub fn write_file(path: &Path, bytes: &[u8]) -> Result<(), BackupError> {
    let failed = |e: std::io::Error| BackupError::File(format!("{}: {e}", path.display()));
    let name = path
        .file_name()
        .ok_or_else(|| BackupError::File(format!("{}: not a file", path.display())))?;
    let mut partial = name.to_os_string();
    partial.push(".partial");
    let partial = path.with_file_name(partial);
    if let Err(e) = std::fs::write(&partial, bytes).and_then(|()| std::fs::rename(&partial, path)) {
        let _ = std::fs::remove_file(&partial);
        return Err(failed(e));
    }
    Ok(())
}

/// A backup file's bytes; one far larger than a profile isn't read at all.
pub fn read_file(path: &Path) -> Result<Vec<u8>, BackupError> {
    let failed = |e: std::io::Error| BackupError::File(format!("{}: {e}", path.display()));
    let len = std::fs::metadata(path).map_err(failed)?.len();
    if len > MAX_FILE_LEN as u64 {
        return Err(BackupError::NotABackup);
    }
    std::fs::read(path).map_err(failed)
}

/// What a backup holds.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contents {
    /// The identity's secret key.
    pub identity: [u8; 32],
    pub rooms: Vec<RememberedRoom>,
    /// The settings files, by name (`settings.json`, `shortcuts.json`, …).
    pub files: Vec<(String, Vec<u8>)>,
}

impl std::fmt::Debug for Contents {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Contents")
            .field("identity", &"..")
            .field("rooms", &self.rooms.len())
            .field(
                "files",
                &self
                    .files
                    .iter()
                    .map(|(n, _)| n.as_str())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}

impl Drop for Contents {
    fn drop(&mut self) {
        self.identity.zeroize();
    }
}

/// The passphrase as both ends see it: trimmed and NFKC-normalized, so the
/// same words typed on another keyboard layout open the file.
fn normalized(passphrase: &str) -> String {
    passphrase.trim().nfkc().collect()
}

/// Whether `passphrase` may seal a backup.
pub fn check_passphrase(passphrase: &str) -> Result<(), BackupError> {
    if normalized(passphrase).chars().count() < MIN_PASSPHRASE_CHARS {
        return Err(BackupError::ShortPassphrase);
    }
    Ok(())
}

fn key(
    passphrase: &str,
    salt: &[u8],
    memory: u32,
    passes: u32,
    lanes: u32,
) -> Result<[u8; 32], BackupError> {
    let params = Params::new(memory, passes, lanes, Some(32))
        .map_err(|e| BackupError::Failed(e.to_string()))?;
    let mut key = [0u8; 32];
    let mut passphrase = normalized(passphrase);
    let result = Argon2::new(Algorithm::Argon2id, Version::V0x13, params).hash_password_into(
        passphrase.as_bytes(),
        salt,
        &mut key,
    );
    passphrase.zeroize();
    result.map_err(|e| BackupError::Failed(e.to_string()))?;
    Ok(key)
}

/// `contents` sealed with `passphrase`, as the file's bytes.
pub fn seal(contents: &Contents, passphrase: &str) -> Result<Vec<u8>, BackupError> {
    let mut body = postcard::to_stdvec(contents).map_err(|e| BackupError::Failed(e.to_string()))?;
    let sealed = seal_body(FORMAT, &body, passphrase);
    body.zeroize();
    sealed
}

/// `body`, in `format`, sealed with `passphrase`.
fn seal_body(format: u8, body: &[u8], passphrase: &str) -> Result<Vec<u8>, BackupError> {
    check_passphrase(passphrase)?;
    let mut salt = [0u8; SALT_LEN];
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::fill(&mut salt).map_err(|e| BackupError::Failed(e.to_string()))?;
    getrandom::fill(&mut nonce).map_err(|e| BackupError::Failed(e.to_string()))?;
    let mut header = Vec::with_capacity(HEADER_LEN);
    header.extend_from_slice(MAGIC);
    header.push(format);
    for n in [MEMORY_KIB, PASSES, LANES] {
        header.extend_from_slice(&n.to_le_bytes());
    }
    header.extend_from_slice(&salt);
    header.extend_from_slice(&nonce);

    let mut key = key(passphrase, &salt, MEMORY_KIB, PASSES, LANES)?;
    let sealed = XChaCha20Poly1305::new(&key.into()).encrypt(
        &XNonce::from(nonce),
        Payload {
            msg: body,
            aad: &header,
        },
    );
    key.zeroize();
    let sealed = sealed.map_err(|e| BackupError::Failed(e.to_string()))?;
    let mut out = header;
    out.extend_from_slice(&sealed);
    Ok(out)
}

/// The contents of a backup file's `bytes`, opened with `passphrase`.
pub fn open(bytes: &[u8], passphrase: &str) -> Result<Contents, BackupError> {
    if bytes.len() < HEADER_LEN || bytes.len() > MAX_FILE_LEN || &bytes[..MAGIC.len()] != MAGIC {
        return Err(BackupError::NotABackup);
    }
    let format = bytes[MAGIC.len()];
    match format {
        0 => return Err(BackupError::NotABackup),
        f if f > FORMAT => return Err(BackupError::NewerFormat),
        _ => {}
    }
    let (header, sealed) = bytes.split_at(HEADER_LEN);
    let word = |i: usize| {
        let at = MAGIC.len() + 1 + 4 * i;
        u32::from_le_bytes(header[at..at + 4].try_into().expect("four bytes"))
    };
    let (memory, passes, lanes) = (word(0), word(1), word(2));
    // Parameters Argon2 itself refuses (no passes, too little memory for
    // the lanes) are no backup of ours either.
    if memory > MAX_MEMORY_KIB
        || passes > MAX_PASSES
        || lanes > MAX_LANES
        || Params::new(memory, passes, lanes, Some(32)).is_err()
    {
        return Err(BackupError::NotABackup);
    }
    let salt = &header[MAGIC.len() + 13..MAGIC.len() + 13 + SALT_LEN];
    let nonce: [u8; NONCE_LEN] = header[HEADER_LEN - NONCE_LEN..]
        .try_into()
        .expect("the nonce's length");
    let mut key = key(passphrase, salt, memory, passes, lanes)?;
    let opened = XChaCha20Poly1305::new(&key.into()).decrypt(
        &XNonce::from(nonce),
        Payload {
            msg: sealed,
            aad: header,
        },
    );
    key.zeroize();
    let mut body = opened.map_err(|_| BackupError::WrongPassphrase)?;
    let contents = match format {
        // From before M13: rooms as they were saved then.
        1 => postcard::from_bytes::<ContentsV1>(&body).map(Contents::from),
        _ => postcard::from_bytes(&body),
    }
    .map_err(|_| BackupError::WrongPassphrase);
    body.zeroize();
    contents
}

/// What format 1 held (before M13): the same, with rooms saved without
/// their chain of handovers.
#[derive(Serialize, Deserialize)]
struct ContentsV1 {
    identity: [u8; 32],
    rooms: Vec<crate::store::legacy::RememberedRoom>,
    files: Vec<(String, Vec<u8>)>,
}

impl From<ContentsV1> for Contents {
    fn from(mut old: ContentsV1) -> Self {
        let contents = Self {
            identity: old.identity,
            rooms: std::mem::take(&mut old.rooms)
                .into_iter()
                .map(Into::into)
                .collect(),
            files: std::mem::take(&mut old.files),
        };
        old.identity.zeroize();
        contents
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contents() -> Contents {
        Contents {
            identity: [7; 32],
            rooms: Vec::new(),
            files: vec![(
                "settings.json".into(),
                b"{\"display_name\":\"Ana\"}".to_vec(),
            )],
        }
    }

    const PASSPHRASE: &str = "pizza night on the couch";

    #[test]
    fn a_backup_opens_with_its_passphrase_alone() {
        let sealed = seal(&contents(), PASSPHRASE).unwrap();
        assert_eq!(open(&sealed, PASSPHRASE).unwrap(), contents());
        // Trimmed and normalized, as room passwords are.
        assert_eq!(
            open(&sealed, &format!("  {PASSPHRASE} ")).unwrap(),
            contents()
        );
        assert_eq!(
            open(&sealed, "pizza night on the sofa"),
            Err(BackupError::WrongPassphrase)
        );
        // Two backups of the same profile share no salt or nonce.
        assert_ne!(seal(&contents(), PASSPHRASE).unwrap(), sealed);
    }

    #[test]
    fn nothing_in_the_file_can_change_unnoticed() {
        let sealed = seal(&contents(), PASSPHRASE).unwrap();
        // Every byte after the magic and format: the parameters, salt and
        // nonce are bound to the body, and the body is authenticated.
        for at in [
            MAGIC.len() + 2,
            HEADER_LEN - 1,
            HEADER_LEN,
            sealed.len() - 1,
        ] {
            let mut bent = sealed.clone();
            bent[at] ^= 1;
            assert!(
                open(&bent, PASSPHRASE).is_err(),
                "byte {at} changed unnoticed"
            );
        }
    }

    #[test]
    fn short_passphrases_and_other_files_are_refused() {
        assert_eq!(
            seal(&contents(), "short"),
            Err(BackupError::ShortPassphrase)
        );
        assert_eq!(
            seal(&contents(), "   eleven chr   "),
            Err(BackupError::ShortPassphrase),
            "counted after trimming"
        );
        assert_eq!(
            open(b"not a backup at all, really", PASSPHRASE),
            Err(BackupError::NotABackup)
        );
        let mut newer = seal(&contents(), PASSPHRASE).unwrap();
        newer[MAGIC.len()] = FORMAT + 1;
        assert_eq!(open(&newer, PASSPHRASE), Err(BackupError::NewerFormat));
        // A file asking Argon2 for 4 GiB is not opened.
        let mut greedy = seal(&contents(), PASSPHRASE).unwrap();
        greedy[MAGIC.len() + 1..MAGIC.len() + 5].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(open(&greedy, PASSPHRASE), Err(BackupError::NotABackup));
        // Nor one Argon2 refuses, nor a format that never was.
        let mut no_passes = seal(&contents(), PASSPHRASE).unwrap();
        no_passes[MAGIC.len() + 5..MAGIC.len() + 9].copy_from_slice(&0u32.to_le_bytes());
        assert_eq!(open(&no_passes, PASSPHRASE), Err(BackupError::NotABackup));
        let mut zero = seal(&contents(), PASSPHRASE).unwrap();
        zero[MAGIC.len()] = 0;
        assert_eq!(open(&zero, PASSPHRASE), Err(BackupError::NotABackup));
    }

    #[test]
    fn a_file_is_replaced_whole_and_a_huge_one_never_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("me.couchbackup");
        write_file(&path, b"old").unwrap();
        write_file(&path, b"new").unwrap();
        assert_eq!(read_file(&path).unwrap(), b"new");
        assert_eq!(
            std::fs::read_dir(dir.path()).unwrap().count(),
            1,
            "no partial left"
        );
        let huge = dir.path().join("huge");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(MAX_FILE_LEN as u64 + 1)
            .unwrap();
        assert_eq!(read_file(&huge), Err(BackupError::NotABackup));
    }

    /// A backup made before M13 (format 1) still opens: its rooms come back
    /// with their founder as owner and no handovers.
    #[test]
    fn a_format_1_backup_still_opens() {
        use couch_proto::ids::RoomNonce;
        use couch_room::{
            SigningKey,
            room::{Identity, Room},
        };

        let room = Room::create(
            Identity {
                key: SigningKey::from_bytes(&[1; 32]),
                name: "Ana".into(),
                addr: Default::default(),
            },
            RoomNonce([1; 16]),
            "Couch",
            Default::default(),
            std::time::Instant::now(),
        )
        .unwrap();
        let remembered = crate::store::RememberedRoom {
            saved: room.saved(),
            password: None,
        };
        let old = ContentsV1 {
            identity: [7; 32],
            rooms: vec![crate::store::legacy::RememberedRoom::from_new(&remembered)],
            files: contents().files.clone(),
        };
        let sealed = seal_body(1, &postcard::to_stdvec(&old).unwrap(), PASSPHRASE).unwrap();
        let opened = open(&sealed, PASSPHRASE).unwrap();
        assert_eq!(opened.identity, [7; 32]);
        assert_eq!(opened.rooms, vec![remembered]);
        assert_eq!(opened.files, contents().files);
        // And what is written now is format 2.
        assert_eq!(seal(&contents(), PASSPHRASE).unwrap()[MAGIC.len()], 2);
    }

    #[test]
    fn its_debug_shows_no_secret() {
        let shown = format!("{:?}", contents());
        assert!(!shown.contains("7, 7"), "{shown}");
        assert!(shown.contains("settings.json"));
    }
}
