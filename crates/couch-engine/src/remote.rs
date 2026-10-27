//! Remote play on the home PC (M25, ADR 0010): which devices of the user's
//! own may run its sessions from afar, the PIN they unlock with, and how
//! wrong PINs slow the next try.
//!
//! Kept in `remote-play.json`, never in a backup: a restored PC trusts
//! nobody until devices are added again. Only the home PC's own window
//! changes it; nothing on the wire can. The PIN is stored as its Argon2id
//! hash only, and never logged.

use std::{fmt, path::Path};

use argon2::{Algorithm, Argon2, Params, Version};
use couch_proto::ids::MemberId;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use unicode_normalization::UnicodeNormalization;
use zeroize::Zeroize;

use crate::store::{self, StoreError};

pub(crate) const FILE: &str = "remote-play.json";
/// On a device: the home PCs it accepted as its own (`remote-hosts.json`).
/// A PIN goes only to one of these, so a room member offering remote play
/// can't collect it.
pub(crate) const HOSTS_FILE: &str = "remote-hosts.json";

/// Shorter PINs are refused.
pub const MIN_PIN_CHARS: usize = 6;
/// Wrong PINs allowed before the next try must wait.
pub const FREE_TRIES: u32 = 5;
/// The first wait, which doubles with each wrong PIN after it.
pub const FIRST_WAIT_SECS: u64 = 60;
pub const MAX_WAIT_SECS: u64 = 60 * 60;

/// Argon2id's cost for a PIN: 19 MiB, 2 passes, one lane (OWASP's floor;
/// about a tenth of a second). The tries are slowed by the waits above,
/// not by the hash.
const MEMORY_KIB: u32 = 19 * 1024;
const PASSES: u32 = 2;
const LANES: u32 = 1;

/// `remote-play.json`.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RemotePlay {
    /// Remote play is on: trusted devices may unlock.
    pub on: bool,
    pub devices: Vec<Device>,
    pub pin: Option<PinHash>,
    /// Wrong PINs since the last right one, kept across restarts.
    pub failures: u32,
    /// Unix seconds before which no PIN is tried.
    pub wait_until: u64,
}

impl fmt::Debug for RemotePlay {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RemotePlay")
            .field("on", &self.on)
            .field("devices", &self.devices.len())
            .field("pin", &self.pin.as_ref().map(|_| ".."))
            .field("failures", &self.failures)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Device {
    /// Its member id, in hex.
    pub id: String,
    /// What it was called when it was added.
    pub name: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct PinHash {
    salt: [u8; 16],
    hash: [u8; 32],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    #[error("the PIN must be at least {MIN_PIN_CHARS} characters")]
    Short,
    #[error("the PIN must be at most 64 bytes")]
    Long,
}

/// The PIN as both ends see it: trimmed and NFKC-normalized, like room
/// passwords.
fn normalized(pin: &str) -> String {
    pin.trim().nfkc().collect()
}

/// Whether `pin` may be set.
pub fn check_pin(pin: &str) -> Result<(), PinError> {
    let pin = normalized(pin);
    if pin.chars().count() < MIN_PIN_CHARS {
        Err(PinError::Short)
    } else if pin.len() > couch_proto::remote::MAX_PIN_BYTES {
        Err(PinError::Long)
    } else {
        Ok(())
    }
}

fn hash(pin: &str, salt: &[u8; 16]) -> [u8; 32] {
    let params = Params::new(MEMORY_KIB, PASSES, LANES, Some(32)).expect("fixed parameters");
    let mut out = [0u8; 32];
    let mut pin = normalized(pin);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(pin.as_bytes(), salt, &mut out)
        .expect("fixed parameters and a 16-byte salt");
    pin.zeroize();
    out
}

impl PinHash {
    /// `pin`'s hash with a fresh `salt`. Slow: off the actor's thread.
    pub fn new(pin: &str, salt: [u8; 16]) -> Self {
        Self {
            hash: hash(pin, &salt),
            salt,
        }
    }

    /// Whether `pin` is this one. Slow: off the actor's thread.
    pub fn matches(&self, pin: &str) -> bool {
        let other = hash(pin, &self.salt);
        // Constant time: every byte compared, whatever the first differing.
        other
            .iter()
            .zip(&self.hash)
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
    }
}

impl RemotePlay {
    /// Whether `id` may unlock: remote play on, a PIN set, and `id` one of
    /// the devices.
    pub fn trusts(&self, id: MemberId) -> bool {
        let hex = id.to_hex();
        self.on && self.pin.is_some() && self.devices.iter().any(|d| d.id == hex)
    }

    /// How long until a PIN may be tried, at `now` (Unix seconds); `None`
    /// if it may be now.
    pub fn wait_secs(&self, now: u64) -> Option<u32> {
        (self.wait_until > now).then(|| u32::try_from(self.wait_until - now).unwrap_or(u32::MAX))
    }

    /// Notes a PIN tried at `now`: a right one clears the count, a wrong one
    /// adds to it and, past [`FREE_TRIES`], sets a wait that doubles each
    /// time, up to [`MAX_WAIT_SECS`].
    pub fn tried(&mut self, right: bool, now: u64) {
        if right {
            self.failures = 0;
            self.wait_until = 0;
            return;
        }
        self.failures = self.failures.saturating_add(1);
        if self.failures >= FREE_TRIES {
            let doublings = (self.failures - FREE_TRIES).min(16);
            let wait = (FIRST_WAIT_SECS << doublings).min(MAX_WAIT_SECS);
            self.wait_until = now.saturating_add(wait);
        }
    }
}

/// A short code for a device, shown on both ends when it is added so the
/// right member is picked: "AB12 CD34".
pub fn device_code(id: MemberId) -> String {
    let digest = Sha256::new()
        .chain_update(b"couch/device-code/v1")
        .chain_update(id.0)
        .finalize();
    let hex: String = digest[..4].iter().map(|b| format!("{b:02X}")).collect();
    format!("{} {}", &hex[..4], &hex[4..])
}

/// The settings; none (remote play off) if there are none or they can't be
/// read.
pub(crate) fn load(dir: &Path) -> RemotePlay {
    let path = dir.join(FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("ignoring {}: {e}", path.display());
            RemotePlay::default()
        }),
        Err(_) => RemotePlay::default(),
    }
}

pub(crate) fn save(dir: &Path, settings: &RemotePlay) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(settings).expect("settings serialize");
    store::write_atomic(&dir.join(FILE), &bytes)
}

/// On a device: the home PCs it accepted; none if there are none or they
/// can't be read.
pub(crate) fn hosts(dir: &Path) -> Vec<Device> {
    let path = dir.join(HOSTS_FILE);
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            tracing::warn!("ignoring {}: {e}", path.display());
            Vec::new()
        }),
        Err(_) => Vec::new(),
    }
}

pub(crate) fn save_hosts(dir: &Path, hosts: &[Device]) -> Result<(), StoreError> {
    let bytes = serde_json::to_vec_pretty(hosts).expect("hosts serialize");
    store::write_atomic(&dir.join(HOSTS_FILE), &bytes)
}

/// Now, in Unix seconds, for the waits.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_pin_is_checked_normalized_and_hashed() {
        assert_eq!(check_pin(" 12345 "), Err(PinError::Short));
        assert_eq!(check_pin(&"1".repeat(65)), Err(PinError::Long));
        assert!(check_pin("246810").is_ok());
        let hash = PinHash::new("246810", [7; 16]);
        assert!(hash.matches("246810"));
        assert!(hash.matches("  246810 "), "trimmed");
        // Full-width digits are the same PIN after NFKC.
        assert!(hash.matches("２４６８１０"));
        assert!(!hash.matches("246811"));
        assert!(
            !format!(
                "{:?}",
                RemotePlay {
                    pin: Some(hash),
                    ..RemotePlay::default()
                }
            )
            .contains("salt")
        );
    }

    #[test]
    fn wrong_pins_wait_longer_each_time() {
        let mut s = RemotePlay::default();
        let now = 1_000_000;
        for _ in 0..FREE_TRIES - 1 {
            s.tried(false, now);
            assert_eq!(s.wait_secs(now), None, "free tries");
        }
        s.tried(false, now);
        assert_eq!(s.wait_secs(now), Some(60));
        s.tried(false, now + 60);
        assert_eq!(s.wait_secs(now + 60), Some(120));
        for _ in 0..40 {
            s.tried(false, now);
        }
        assert_eq!(s.wait_secs(now), Some(MAX_WAIT_SECS as u32), "capped");
        assert_eq!(s.wait_secs(now + MAX_WAIT_SECS), None, "over");
        s.tried(true, now);
        assert_eq!((s.failures, s.wait_secs(now)), (0, None));
    }

    #[test]
    fn only_listed_devices_are_trusted_while_on_with_a_pin() {
        let laptop = MemberId([1; 32]);
        let mut s = RemotePlay {
            on: true,
            devices: vec![Device {
                id: laptop.to_hex(),
                name: "Laptop".into(),
            }],
            pin: None,
            ..RemotePlay::default()
        };
        assert!(!s.trusts(laptop), "no PIN yet");
        s.pin = Some(PinHash::new("246810", [1; 16]));
        assert!(s.trusts(laptop));
        assert!(!s.trusts(MemberId([2; 32])));
        s.on = false;
        assert!(!s.trusts(laptop));
    }

    #[test]
    fn device_codes_are_short_and_stable() {
        let code = device_code(MemberId([1; 32]));
        assert_eq!(code.len(), 9);
        assert_eq!(code, device_code(MemberId([1; 32])));
        assert_ne!(code, device_code(MemberId([2; 32])));
    }

    #[test]
    fn settings_roundtrip_and_stay_out_of_backups() {
        let dir = tempfile::tempdir().unwrap();
        let s = RemotePlay {
            on: true,
            pin: Some(PinHash::new("246810", [3; 16])),
            failures: 2,
            ..RemotePlay::default()
        };
        save(dir.path(), &s).unwrap();
        assert_eq!(load(dir.path()), s);
        assert!(!store::backed_up(FILE) && !store::backed_up(HOSTS_FILE));
        let pc = Device {
            id: MemberId([5; 32]).to_hex(),
            name: "Marcus-PC".into(),
        };
        save_hosts(dir.path(), std::slice::from_ref(&pc)).unwrap();
        assert_eq!(hosts(dir.path()), [pc]);
    }
}
