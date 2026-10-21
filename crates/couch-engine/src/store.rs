//! Files in a profile's data folder:
//! - `identity.key`: the endpoint's secret key, sealed.
//! - `settings.json`: the display name. Nothing secret.
//! - `shortcuts.json`: the shortcut keys the user moved off their defaults.
//! - `network.json`: the relay to use (applies when the engine starts).
//! - `audio.json`: the stream window's volume (P6).
//! - `host-audio.json`: how loud guests hear our stream's sound (P7).
//!
//! The room actor rewrites `settings.json` whole, so what the engine itself
//! writes has files of its own.
//! - `rooms/<room id>.room`: one remembered room each, sealed: its saved
//!   state and, for rooms we own, the invites and the password key (never
//!   the password).
//! - `rooms/<room id>.restored`: the room came from a backup and hasn't
//!   caught up with its members yet (M13, `crate::backup`); empty.

use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    sync::Arc,
};

use couch_input::shortcuts::Shortcuts;
use couch_media::audio::MAX_BOOST_PERCENT;
use couch_net::SecretKey;
use couch_proto::{ids::RoomId, ipc::StreamVolume};
use couch_room::room::SavedRoom;
use serde::{Deserialize, Serialize};

use crate::{RelaySetting, backup, secrets::SecretStore};

/// `network.json`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct NetworkSettings {
    pub relay: RelaySetting,
}

const IDENTITY_FILE: &str = "identity.key";
const SETTINGS_FILE: &str = "settings.json";
const SHORTCUTS_FILE: &str = "shortcuts.json";
const NETWORK_FILE: &str = "network.json";
const AUDIO_FILE: &str = "audio.json";
const HOST_AUDIO_FILE: &str = "host-audio.json";
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
    #[error(
        "this profile already has rooms: a backup is restored only into a new profile (or on a new PC)"
    )]
    HasRooms,
}

/// The settings files a backup carries, as they are (`crate::backup`).
/// Not the webview's own storage (the room order, the last session setup).
const BACKED_UP_FILES: [&str; 5] = [
    SETTINGS_FILE,
    SHORTCUTS_FILE,
    NETWORK_FILE,
    AUDIO_FILE,
    HOST_AUDIO_FILE,
];

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

/// What a restore wrote so far, to take back if it fails.
struct Undo {
    rooms: Vec<RoomId>,
    /// Each settings file written, with what it held before.
    files: Vec<(PathBuf, Option<Vec<u8>>)>,
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
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(StoreError::Io { path, source }),
        }
        // The mark after its room: a crash between the two leaves a mark
        // alone (swept at start), never a restored room without one.
        self.clear_restored(id);
        Ok(())
    }

    fn room_path(&self, id: RoomId) -> PathBuf {
        self.dir
            .join(ROOMS_DIR)
            .join(format!("{}.room", id.to_hex()))
    }

    fn restored_path(&self, id: RoomId) -> PathBuf {
        self.dir
            .join(ROOMS_DIR)
            .join(format!("{}.restored", id.to_hex()))
    }

    /// The room came from a backup and hasn't caught up with its members.
    pub fn is_restored(&self, id: RoomId) -> bool {
        self.restored_path(id).exists()
    }

    /// The room has caught up: it may sign again after a restart too.
    pub fn clear_restored(&self, id: RoomId) {
        let path = self.restored_path(id);
        if let Err(e) = fs::remove_file(&path)
            && e.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!("{}: {e}", path.display());
        }
    }

    /// Removes marks whose room isn't there: what a restore cut short, or a
    /// room forgotten between the two, leaves.
    pub fn sweep_restored(&self) {
        let Ok(entries) = fs::read_dir(self.dir.join(ROOMS_DIR)) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "restored")
                && !path.with_extension("room").exists()
            {
                tracing::info!("{}: a mark without its room", path.display());
                if let Err(e) = fs::remove_file(&path) {
                    tracing::warn!("{}: {e}", path.display());
                }
            }
        }
    }

    /// Whether this profile remembers any room, even one it can't read or
    /// one it is leaving: a backup restores only into a profile with none.
    pub fn has_rooms(&self) -> bool {
        fs::read_dir(self.dir.join(ROOMS_DIR)).is_ok_and(|entries| {
            entries
                .flatten()
                .any(|e| e.path().extension().is_some_and(|e| e == "room"))
        })
    }

    /// What a backup of this profile carries (M13): the identity, the rooms
    /// (not those on their way out), and the settings files. An owned
    /// room's password key and invites stay out: an old backup must never
    /// bring back an invite revoked or a password changed since.
    pub fn backup_contents(&self) -> Result<backup::Contents, StoreError> {
        let identity = self.identity()?.to_bytes();
        let rooms = self
            .rooms()
            .into_iter()
            .filter(|r| r.saved.leaving.is_none())
            .map(|mut r| {
                r.password = None;
                r.saved.invites.clear();
                r
            })
            .collect();
        let files = BACKED_UP_FILES
            .iter()
            .filter_map(|name| {
                fs::read(self.dir.join(name))
                    .ok()
                    .map(|bytes| ((*name).to_string(), bytes))
            })
            .collect();
        Ok(backup::Contents {
            identity,
            rooms,
            files,
        })
    }

    /// Puts a backup into this profile, which must have no rooms: its
    /// identity replaces this one's (sealed for this Windows user), each
    /// room is marked to catch up before it signs anything, and the settings
    /// files are written. The engine must start again to use it. A restore
    /// that fails partway is undone: the profile is left as it was.
    pub fn restore(&self, contents: &backup::Contents) -> Result<(), StoreError> {
        if self.has_rooms() {
            return Err(StoreError::HasRooms);
        }
        let mut undo = Undo {
            rooms: Vec::new(),
            files: Vec::new(),
        };
        let result = self.restore_into(contents, &mut undo);
        if result.is_err() {
            for id in &undo.rooms {
                let _ = self.forget_room(*id);
            }
            // Last write first: a file named twice gets its first "before".
            for (path, before) in undo.files.iter().rev() {
                match before {
                    Some(bytes) => drop(write_atomic(path, bytes)),
                    None => drop(fs::remove_file(path)),
                }
            }
        }
        result
    }

    fn restore_into(&self, contents: &backup::Contents, undo: &mut Undo) -> Result<(), StoreError> {
        let path = self.dir.join(IDENTITY_FILE);
        let sealed = self
            .secrets
            .protect(&contents.identity)
            .map_err(|source| StoreError::Io {
                path: path.clone(),
                source,
            })?;
        let mut restored = 0;
        for room in &contents.rooms {
            // What a backup never carries, whatever the file says: a room on
            // its way out, an owned room's password key and invites.
            if room.saved.leaving.is_some() {
                continue;
            }
            let mut room = room.clone();
            room.password = None;
            room.saved.invites.clear();
            let id = couch_room::signing::room_id(room.saved.owner, room.saved.nonce);
            undo.rooms.push(id);
            // The mark first: a room on disk without one would sign at once
            // after a crash here.
            write_atomic(&self.restored_path(id), b"")?;
            self.save_room(id, &room)?;
            restored += 1;
        }
        for (name, bytes) in &contents.files {
            // Only names we write ourselves: a crafted backup can't place a
            // file anywhere else.
            if BACKED_UP_FILES.contains(&name.as_str()) {
                let file = self.dir.join(name);
                undo.files.push((file.clone(), fs::read(&file).ok()));
                write_atomic(&file, bytes)?;
            }
        }
        // The identity last: a failure before it is undone above, and the
        // profile keeps its own identity.
        write_atomic(&path, &sealed)?;
        tracing::info!(rooms = restored, "restored a backup");
        Ok(())
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

/// The stream window's volume (`audio.json`, P6); full volume if there is
/// none or it can't be read. Only the engine writes it.
pub(crate) fn volume(dir: &Path) -> StreamVolume {
    let path = dir.join(AUDIO_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<StreamVolume>(&bytes)
            .map(StreamVolume::clamped)
            .unwrap_or_else(|e| {
                tracing::warn!("ignoring {}: {e}", path.display());
                StreamVolume::default()
            }),
        Err(_) => StreamVolume::default(),
    }
}

pub(crate) fn save_volume(dir: &Path, volume: StreamVolume) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(&volume).expect("volume serialize");
    write_atomic(&dir.join(AUDIO_FILE), &bytes)
}

/// `host-audio.json` (P7): how loud guests hear our stream's sound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct HostAudio {
    /// 0 to 400% of the game's (`couch_media::audio::MAX_BOOST_PERCENT`).
    percent: u16,
}

/// The host's stream level (`host-audio.json`, P7), 0 to 400; 100 if there
/// is none or it can't be read. Only the engine writes it.
pub(crate) fn host_level(dir: &Path) -> u16 {
    let path = dir.join(HOST_AUDIO_FILE);
    match fs::read(&path) {
        Ok(bytes) => serde_json::from_slice::<HostAudio>(&bytes)
            .map(|a| a.percent.min(MAX_BOOST_PERCENT))
            .unwrap_or_else(|e| {
                tracing::warn!("ignoring {}: {e}", path.display());
                100
            }),
        Err(_) => 100,
    }
}

pub(crate) fn save_host_level(dir: &Path, percent: u16) -> Result<(), StoreError> {
    let audio = HostAudio {
        percent: percent.min(MAX_BOOST_PERCENT),
    };
    let bytes = serde_json::to_vec_pretty(&audio).expect("level serialize");
    write_atomic(&dir.join(HOST_AUDIO_FILE), &bytes)
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

    fn owned_room(seed: u8) -> Room {
        Room::create(
            Identity {
                key: SigningKey::from_bytes(&[seed; 32]),
                name: "Ana".into(),
                addr: Default::default(),
            },
            RoomNonce([seed; 16]),
            "Couch",
            Default::default(),
            std::time::Instant::now(),
        )
        .unwrap()
    }

    /// M13: a backup carries the identity, the rooms and the settings, but
    /// never an owned room's password key or invites, nor a room being left.
    #[test]
    fn a_backup_leaves_out_passwords_invites_and_rooms_being_left() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let identity = store.identity().unwrap();
        let mut room = owned_room(1);
        room.create_invite(couch_proto::ids::InviteToken([9; 32]), 1_000, 3600, true)
            .unwrap();
        let mut remembered = RememberedRoom {
            saved: room.saved(),
            password: Some([4; 32]),
        };
        assert_eq!(remembered.saved.invites.len(), 1);
        store.save_room(room.id(), &remembered).unwrap();
        let mut leaving = owned_room(2);
        leaving.leave(std::time::Instant::now()).unwrap();
        store
            .save_room(
                leaving.id(),
                &RememberedRoom {
                    saved: leaving.saved(),
                    password: None,
                },
            )
            .unwrap();
        store
            .save_settings(&Settings {
                display_name: Some("Ana".into()),
            })
            .unwrap();

        let contents = store.backup_contents().unwrap();
        assert_eq!(contents.identity, identity.to_bytes());
        remembered.password = None;
        remembered.saved.invites.clear();
        assert_eq!(contents.rooms, vec![remembered], "only the room being kept");
        assert_eq!(contents.files.len(), 1);
        assert_eq!(contents.files[0].0, SETTINGS_FILE);
    }

    /// M13: restored only into a profile with no rooms; the identity is
    /// replaced, each room marked to catch up, and only our own files
    /// written.
    #[test]
    fn a_backup_restores_into_an_empty_profile_only() {
        let from = tempfile::tempdir().unwrap();
        let source = store(from.path());
        source.identity().unwrap();
        let room = owned_room(1);
        source
            .save_room(
                room.id(),
                &RememberedRoom {
                    saved: room.saved(),
                    password: Some([4; 32]),
                },
            )
            .unwrap();
        let mut contents = source.backup_contents().unwrap();
        contents.files.push(("..\\evil.txt".into(), b"no".to_vec()));

        let to = tempfile::tempdir().unwrap();
        let target = store(to.path());
        let own = target.identity().unwrap();
        target.restore(&contents).unwrap();
        assert_eq!(target.identity().unwrap().to_bytes(), contents.identity);
        assert_ne!(own.to_bytes(), contents.identity);
        assert_eq!(target.rooms().len(), 1);
        assert_eq!(target.rooms()[0].password, None);
        assert!(target.is_restored(room.id()));
        assert!(!to.path().join("..").join("evil.txt").exists());
        // Caught up: the mark goes, and forgetting the room takes it too.
        target.clear_restored(room.id());
        assert!(!target.is_restored(room.id()));

        // A profile with rooms is refused, and left as it was.
        let before = target.identity().unwrap().to_bytes();
        assert!(matches!(
            target.restore(&contents),
            Err(StoreError::HasRooms)
        ));
        assert_eq!(target.identity().unwrap().to_bytes(), before);
    }

    /// M13: whatever a crafted backup holds, no password key, invite or
    /// room being left is restored.
    #[test]
    fn a_restore_drops_what_a_backup_never_carries() {
        let mut room = owned_room(1);
        room.create_invite(couch_proto::ids::InviteToken([9; 32]), 1_000, 3600, true)
            .unwrap();
        let mut leaving = owned_room(2);
        leaving.leave(std::time::Instant::now()).unwrap();
        let contents = backup::Contents {
            identity: [7; 32],
            rooms: vec![
                RememberedRoom {
                    saved: room.saved(),
                    password: Some([4; 32]),
                },
                RememberedRoom {
                    saved: leaving.saved(),
                    password: None,
                },
            ],
            files: Vec::new(),
        };
        let to = tempfile::tempdir().unwrap();
        let target = store(to.path());
        target.restore(&contents).unwrap();
        let rooms = target.rooms();
        assert_eq!(rooms.len(), 1);
        assert_eq!(rooms[0].password, None);
        assert!(rooms[0].saved.invites.is_empty());
        assert!(!target.is_restored(leaving.id()));
    }

    /// M13: a restore that fails partway leaves the profile as it was.
    #[test]
    fn a_failed_restore_is_undone() {
        let room = owned_room(1);
        let contents = backup::Contents {
            identity: [7; 32],
            rooms: vec![RememberedRoom {
                saved: room.saved(),
                password: None,
            }],
            files: vec![
                (SHORTCUTS_FILE.into(), b"{}".to_vec()),
                (SETTINGS_FILE.into(), b"{}".to_vec()),
            ],
        };
        let to = tempfile::tempdir().unwrap();
        let target = store(to.path());
        let own = target.identity().unwrap().to_bytes();
        fs::write(to.path().join(SHORTCUTS_FILE), b"mine").unwrap();
        // settings.json can't be written: a folder is in its way.
        fs::create_dir_all(to.path().join(SETTINGS_FILE).join("x")).unwrap();
        assert!(target.restore(&contents).is_err());
        assert_eq!(target.identity().unwrap().to_bytes(), own);
        assert!(!target.has_rooms());
        assert!(!target.is_restored(room.id()));
        assert_eq!(fs::read(to.path().join(SHORTCUTS_FILE)).unwrap(), b"mine");
    }

    /// A mark left without its room is swept, and counts as no room.
    #[test]
    fn a_mark_without_its_room_is_swept() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(dir.path());
        let room = owned_room(1);
        write_atomic(&store.restored_path(room.id()), b"").unwrap();
        assert!(!store.has_rooms());
        store.sweep_restored();
        assert!(!store.is_restored(room.id()));
        // With its room, it stays.
        write_atomic(&store.restored_path(room.id()), b"").unwrap();
        store
            .save_room(
                room.id(),
                &RememberedRoom {
                    saved: room.saved(),
                    password: None,
                },
            )
            .unwrap();
        store.sweep_restored();
        assert!(store.is_restored(room.id()));
        assert!(store.has_rooms());
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
    fn the_stream_volume_roundtrips_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(volume(dir.path()), StreamVolume::default());
        let quiet = StreamVolume {
            percent: 40,
            muted: true,
        };
        save_volume(dir.path(), quiet).unwrap();
        assert_eq!(volume(dir.path()), quiet);
        fs::write(dir.path().join(AUDIO_FILE), b"{not json").unwrap();
        assert_eq!(volume(dir.path()), StreamVolume::default());
        // A hand-edited level above 100% plays at 100%.
        fs::write(
            dir.path().join(AUDIO_FILE),
            br#"{"percent":250,"muted":false}"#,
        )
        .unwrap();
        assert_eq!(volume(dir.path()).percent, 100);
    }

    #[test]
    fn the_host_level_roundtrips_and_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(host_level(dir.path()), 100);
        save_host_level(dir.path(), 35).unwrap();
        assert_eq!(host_level(dir.path()), 35);
        save_host_level(dir.path(), 300).unwrap();
        assert_eq!(host_level(dir.path()), 300);
        fs::write(dir.path().join(HOST_AUDIO_FILE), br#"{"percent":900}"#).unwrap();
        assert_eq!(host_level(dir.path()), 400, "never above 400");
        fs::write(dir.path().join(HOST_AUDIO_FILE), b"nope").unwrap();
        assert_eq!(host_level(dir.path()), 100);
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
