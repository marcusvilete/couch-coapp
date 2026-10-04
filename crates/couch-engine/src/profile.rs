//! Profiles let several instances run on one machine with separate identities,
//! which is how multi-member flows are tested without a second PC.

use std::path::{Path, PathBuf};

/// Environment variable naming the profile. Defaults to `default`.
pub const PROFILE_ENV: &str = "COUCH_PROFILE";

#[derive(Debug, thiserror::Error)]
pub enum ProfileError {
    #[error("profile name {0:?} must be 1 to 32 letters, digits, '-' or '_'")]
    InvalidName(String),
    #[error("no per-user data directory is available on this system")]
    NoDataDir,
}

/// One identity's folder: key, remembered rooms, logs, webview data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    name: String,
    data_dir: PathBuf,
}

impl Profile {
    /// Profile `name` stored under `base/profiles/name`. Names are
    /// lowercased: Windows folder names ignore case, and `Alice` must be the
    /// same profile as `alice`, not a second app on the same identity.
    pub fn new(name: &str, base: impl AsRef<Path>) -> Result<Self, ProfileError> {
        let valid = (1..=32).contains(&name.len())
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !valid {
            return Err(ProfileError::InvalidName(name.to_string()));
        }
        let name = name.to_ascii_lowercase();
        Ok(Self {
            data_dir: base.as_ref().join("profiles").join(&name),
            name,
        })
    }

    /// The profile named by `COUCH_PROFILE`, under the per-user local data
    /// directory (`%LOCALAPPDATA%\CouchCoApp` on Windows).
    pub fn from_env() -> Result<Self, ProfileError> {
        let name = std::env::var(PROFILE_ENV).unwrap_or_else(|_| "default".to_string());
        let base = dirs::data_local_dir().ok_or(ProfileError::NoDataDir)?;
        Self::new(&name, base.join("CouchCoApp"))
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn is_default(&self) -> bool {
        self.name == "default"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_cannot_escape_the_profiles_folder() {
        for bad in ["", "..", "a/b", r"a\b", "with space", &"x".repeat(33)] {
            assert!(Profile::new(bad, "base").is_err(), "{bad:?} was accepted");
        }
    }

    #[test]
    fn names_ignore_case() {
        let profile = Profile::new("Default", "base").unwrap();
        assert!(profile.is_default());
        assert_eq!(
            Profile::new("ALICE", "base").unwrap(),
            Profile::new("alice", "base").unwrap()
        );
    }

    #[test]
    fn data_lives_under_profiles() {
        let profile = Profile::new("alice", "base").unwrap();
        assert_eq!(
            profile.data_dir(),
            Path::new("base").join("profiles").join("alice")
        );
        assert!(!profile.is_default());
    }
}
