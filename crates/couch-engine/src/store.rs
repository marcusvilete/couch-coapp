//! Files in a profile's data folder:
//! - `identity.key`: the endpoint's secret key, sealed.
//! - `settings.json`: the display name. Nothing secret.
//! - `shortcuts.json`: the shortcut keys the user moved off their defaults.
//! - `network.json`: the relay to use (applies when the engine starts).
//!
//! The room actor rewrites `settings.json` whole, so what the engine itself
//! writes has files of its own.
//! - `rooms/<room id>.room`: one remembered room each, sealed: its saved
//!   state and, for rooms we own, the invites and the password key (never
//!   the password).

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use couch_input::shortcuts::Shortcuts;
use couch_net::SecretKey;
use couch_proto::ids::RoomId;
use couch_room::room::SavedRoom;
use serde::{Deserialize, Serialize};

use crate::{RelaySetting, secrets::SecretStore};

/// `network.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NetworkSettings {
    pub relay: RelaySetting,
}

const IDENTITY_FILE: &str = "identity.key";
const SETTINGS_FILE: &str = "settings.json";
const SHORTCUTS_FILE: &str = "shortcuts.json";
const NETWORK_FILE: &str = "network.json";
const ROOMS_DIR: &str = "rooms";
/// Leads every room file, before the postcard body, so a format change is
/// recognized rather than misread.
const ROOM_MAGIC: &[u8] = b"CCR2";

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("{path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("{0} is damaged or was sealed by another Windows user")]
    Unreadable(PathBuf),
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    pub display_name: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RememberedRoom {
    pub saved: SavedRoom,
    /// The password key, kept by the owner only: it alone admits newcomers.
    pub password: Option<[u8; 32]>,
}

impl std::fmt::Debug for RememberedRoom {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RememberedRoom")
            .field("saved", &self.saved)
            .field("password", &self.password.map(|_| ".."))
            .finish()
    }
}

pub struct Store {
    dir: PathBuf,
    secrets: Arc<dyn SecretStore>,
}

impl Store {
    pub fn new(dir: impl Into<PathBuf>, secrets: Arc<dyn SecretStore>) -> Self {
        Self {
            dir: dir.into(),
            secrets,
        }
    }

    /// This profile's key, created on first use. A key that can't be read
    /// is an error rather than a silent new identity, which would orphan
    /// every room.
    pub fn identity(&self) -> Result<SecretKey, StoreError> {
        let path = self.dir.join(IDENTITY_FILE);
        match fs::read(&path) {
            Ok(sealed) => {
                let bytes = self
                    .secrets
                    .unprotect(&sealed)
                    .map_err(|_| StoreError::Unreadable(path.clone()))?;
                let bytes: [u8; 32] = bytes
                    .try_into()
                    .map_err(|_| StoreError::Unreadable(path.clone()))?;
                Ok(SecretKey::from_bytes(&bytes))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let key = SecretKey::generate();
                let sealed =
                    self.secrets
                        .protect(&key.to_bytes())
                        .map_err(|source| StoreError::Io {
                            path: path.clone(),
                            source,
                        })?;
                write_atomic(&path, &sealed)?;
                tracing::info!(id = %key.public().fmt_short(), "created a new identity");
                Ok(key)
            }
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    pub fn settings(&self) -> Settings {
        let path = self.dir.join(SETTINGS_FILE);
        match fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                tracing::warn!("ignoring {}: {e}", path.display());
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    pub fn save_settings(&self, settings: &Settings) -> Result<(), StoreError> {
        let bytes = serde_json::to_vec_pretty(settings).expect("settings serialize");
        write_atomic(&self.dir.join(SETTINGS_FILE), &bytes)
    }

    /// Every remembered room that can be read. Unreadable files are logged
    /// and left alone.
    pub fn rooms(&self) -> Vec<RememberedRoom> {
        let Ok(entries) = fs::read_dir(self.dir.join(ROOMS_DIR)) else {
            return Vec::new();
        };
        let mut rooms = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "room") {
                continue;
            }
            match self.read_room(&path) {
                Ok(room) => rooms.push(room),
                Err(e) => tracing::warn!("skipping a remembered room: {e}"),
            }
        }
        rooms
    }

    fn read_room(&self, path: &Path) -> Result<RememberedRoom, StoreError> {
        let unreadable = || StoreError::Unreadable(path.to_path_buf());
        let sealed = fs::read(path).map_err(|source| StoreError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let bytes = self.secrets.unprotect(&sealed).map_err(|_| unreadable())?;
        let body = bytes.strip_prefix(ROOM_MAGIC).ok_or_else(unreadable)?;
        let (room, rest) = postcard::take_from_bytes(body).map_err(|_| unreadable())?;
        if !rest.is_empty() {
            return Err(unreadable());
        }
        Ok(room)
    }

    pub fn save_room(&self, id: RoomId, room: &RememberedRoom) -> Result<(), StoreError> {
        let path = self.room_path(id);
        let mut bytes = ROOM_MAGIC.to_vec();
        bytes.extend(postcard::to_stdvec(room).expect("room serializes"));
        let sealed = self
            .secrets
            .protect(&bytes)
            .map_err(|source| StoreError::Io {
                path: path.clone(),
                source,
            })?;
        write_atomic(&path, &sealed)
    }

    pub fn forget_room(&self, id: RoomId) -> Result<(), StoreError> {
        let path = self.room_path(id);
        match fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(StoreError::Io { path, source }),
        }
    }

    fn room_path(&self, id: RoomId) -> PathBuf {
        self.dir
            .join(ROOMS_DIR)
            .join(format!("{}.room", id.to_hex()))
    }
}

/// The user's shortcut keys (`shortcuts.json`: the rows moved off their
/// defaults, by row id). Keys that no longer fit are logged and left at
/// their defaults. Only the engine writes it.
pub(crate) fn shortcuts(dir: &Path) -> Shortcuts {
    let path = dir.join(SHORTCUTS_FILE);
    let keys: BTreeMap<String, u16> = match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("ignoring {}: {e}", path.display());
            BTreeMap::new()
        }),
        Err(_) => BTreeMap::new(),
    };
    let (shortcuts, skipped) = Shortcuts::with_keys(&keys);
    for (id, e) in skipped {
        tracing::warn!(
            "shortcut {id} in {}: {e}; its default applies",
            path.display()
        );
    }
    shortcuts
}

pub(crate) fn save_shortcuts(dir: &Path, shortcuts: &Shortcuts) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(&shortcuts.changed()).expect("keys serialize");
    write_atomic(&dir.join(SHORTCUTS_FILE), &bytes)
}

/// The network settings (`network.json`); the defaults if there are none or
/// they can't be read. Only the engine writes it.
pub(crate) fn network(dir: &Path) -> NetworkSettings {
    let path = dir.join(NETWORK_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("ignoring {}: {e}", path.display());
            NetworkSettings::default()
        }),
        Err(_) => NetworkSettings::default(),
    }
}

pub(crate) fn save_network(dir: &Path, network: &NetworkSettings) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(network).expect("settings serialize");
    write_atomic(&dir.join(NETWORK_FILE), &bytes)
}

/// Writes a sibling temporary file and renames it over `path`, so a crash
/// leaves the old file or the new one, never half of one.
fn write_atomic(path: &Path, bytes: &[u8]) -> Result<(), StoreError> {
    let io = |source| StoreError::Io {
        path: path.to_path_buf(),
        source,
    };
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(io)?;
    }
    let tmp = path.with_extension("tmp");
    fs::write(&tmp, bytes).map_err(io)?;
    fs::rename(&tmp, path).map_err(io)
}

#[cfg(test)]
mod tests {
    use couch_proto::ids::RoomNonce;
    use couch_room::{
        SigningKey,
        room::{Identity, Room},
    };

    use super::*;
    use crate::secrets::Unprotected;

    fn store(dir: &Path) -> Store {
        Store::new(dir, Arc::new(Unprotected))
    }

    #[test]
    fn the_identity_is_created_once_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let first = store(dir.path()).identity().unwrap();
        let again = store(dir.path()).identity().unwrap();
        assert_eq!(first.to_bytes(), again.to_bytes());
    }

    #[test]
    fn a_damaged_identity_is_an_error_not_a_new_key() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(IDENTITY_FILE), b"short").unwrap();
        assert!(matches!(
            store(dir.path()).identity(),
            Err(StoreError::Unreadable(_))
        ));
    }

    #[test]
    fn rooms_roundtrip_and_are_forgotten() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let room = Room::create(
            Identity {
                key: SigningKey::from_bytes(&[1; 32]),
                name: "Ana".into(),
                addr: Default::default(),
            },
            RoomNonce([2; 16]),
            "Couch",
            Default::default(),
            std::time::Instant::now(),
        )
        .unwrap();
        let remembered = RememberedRoom {
            saved: room.saved(),
            password: Some([4; 32]),
        };
        store.save_room(room.id(), &remembered).unwrap();
        assert_eq!(store.rooms(), vec![remembered.clone()]);
        // Saving again replaces.
        store.save_room(room.id(), &remembered).unwrap();
        assert_eq!(store.rooms().len(), 1);
        store.forget_room(room.id()).unwrap();
        assert!(store.rooms().is_empty());
        store.forget_room(room.id()).unwrap();
    }

    #[test]
    fn foreign_files_are_skipped() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join(ROOMS_DIR)).unwrap();
        fs::write(dir.path().join(ROOMS_DIR).join("x.room"), b"not a room").unwrap();
        fs::write(dir.path().join(ROOMS_DIR).join("notes.txt"), b"hi").unwrap();
        assert!(store(dir.path()).rooms().is_empty());
    }

    #[test]
    fn settings_default_and_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        assert_eq!(store.settings(), Settings::default());
        let settings = Settings {
            display_name: Some("Ana".into()),
        };
        store.save_settings(&settings).unwrap();
        assert_eq!(store.settings(), settings);
    }

    #[test]
    fn shortcut_keys_roundtrip_and_bad_ones_fall_back() {
        use couch_input::{HidUsage, shortcuts::Shortcut};

        let dir = tempfile::tempdir().unwrap();
        assert_eq!(shortcuts(dir.path()), Shortcuts::default());
        let mut moved = Shortcuts::default();
        moved.set("panic", HidUsage(0x13)).unwrap();
        save_shortcuts(dir.path(), &moved).unwrap();
        assert_eq!(shortcuts(dir.path()), moved);
        // Only what moved is written.
        let text = fs::read_to_string(dir.path().join(SHORTCUTS_FILE)).unwrap();
        assert!(text.contains("panic") && !text.contains("fullscreen"));

        // Esc isn't allowed: that row keeps its default, the rest applies.
        fs::write(
            dir.path().join(SHORTCUTS_FILE),
            br#"{"panic": 41, "fullscreen": 13}"#,
        )
        .unwrap();
        let table = shortcuts(dir.path());
        assert!(table.is_default("panic"));
        assert_eq!(table.binding(Shortcut::Fullscreen).key, HidUsage(13));
        fs::write(dir.path().join(SHORTCUTS_FILE), b"[").unwrap();
        assert_eq!(shortcuts(dir.path()), Shortcuts::default());
    }

    #[test]
    fn network_settings_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(network(dir.path()).relay, RelaySetting::Builtin);
        // A choice saved before M7 keeps applying.
        fs::write(dir.path().join(NETWORK_FILE), br#"{"relay": "public"}"#).unwrap();
        assert_eq!(network(dir.path()).relay, RelaySetting::Public);
        let custom = NetworkSettings {
            relay: RelaySetting::Custom("https://relay.example.com".into()),
        };
        save_network(dir.path(), &custom).unwrap();
        assert_eq!(network(dir.path()), custom);
        let text = fs::read_to_string(dir.path().join(NETWORK_FILE)).unwrap();
        assert!(
            text.contains(r#""custom": "https://relay.example.com""#),
            "{text}"
        );
        fs::write(dir.path().join(NETWORK_FILE), b"{}").unwrap();
        assert_eq!(network(dir.path()), NetworkSettings::default());
    }
}
