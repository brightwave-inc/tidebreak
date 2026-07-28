//! Application boot configuration.
//!
//! Two layers, deliberately separate:
//!
//! - **Boot config** — this module. The immutable, start-of-day settings needed
//!   *before* the store exists: which [`Profile`] to run and where app data
//!   lives. The profile selects which backend implementations get wired at boot
//!   (SQLite vs Postgres, keychain vs KMS, …).
//! - **Runtime settings** — the mutable, per-user settings that live in the
//!   `Store`'s `setting` table (enabled model provider + chosen model, approval
//!   policy, preferences). Reached with `Store::get_setting` / `set_setting`;
//!   they change while the app runs, so they don't belong here.

use std::ffi::OsString;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::error::{AgentError, Result};

/// Which deployment shape the app runs as; selects the concrete backends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Profile {
    /// Single-user desktop: SQLite, OS keychain, local filesystem. The default.
    #[default]
    Desktop,
    /// Self-hosted server: Postgres, env/KMS secrets, object storage.
    SelfHost,
}

/// Boot configuration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Config {
    /// The deployment profile.
    #[serde(default)]
    pub profile: Profile,
    /// Directory holding the app's data (database, blobs, …).
    pub data_dir: PathBuf,
    /// Keychain service name secrets are stored under; `None` uses the
    /// default (`openwave`). Embedders whose builds must not share secret
    /// state — a dev build running beside an installed release — set a
    /// distinct service here, mirroring the app-identifier and data-dir
    /// split.
    #[serde(default)]
    pub keychain_service: Option<String>,
}

impl Config {
    /// A desktop-profile config rooted at `data_dir`.
    pub fn desktop(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            profile: Profile::Desktop,
            data_dir: data_dir.into(),
            keychain_service: None,
        }
    }

    /// Load from the environment, falling back to sensible defaults:
    /// `OPENWAVE_PROFILE` (default `desktop`) and `OPENWAVE_DATA_DIR` (default
    /// `./.openwave` under the current directory — desktop/CLI clients should set
    /// this to the platform's app-data location).
    pub fn from_env() -> Result<Self> {
        Self::from_vars(
            std::env::var("OPENWAVE_PROFILE").ok(),
            std::env::var_os("OPENWAVE_DATA_DIR"),
        )
    }

    /// Resolve a config from raw variable values. Split out from [`from_env`] so
    /// the empty-vs-unset handling is testable without mutating process-global
    /// environment (which races other threads' `getenv` in a shared test binary).
    ///
    /// An **empty** value is treated as unset: a caller that exports
    /// `OPENWAVE_DATA_DIR=` (or an empty profile) gets the documented defaults,
    /// not an empty path rooted at the current directory.
    fn from_vars(profile: Option<String>, data_dir: Option<OsString>) -> Result<Self> {
        let profile = match profile.filter(|value| !value.is_empty()).as_deref() {
            None | Some("desktop") => Profile::Desktop,
            Some("self_host" | "selfhost") => Profile::SelfHost,
            Some(other) => return Err(AgentError::config(format!("unknown profile: {other}"))),
        };
        let data_dir = match data_dir.filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => std::env::current_dir()
                .map_err(|e| AgentError::config(format!("no working directory: {e}")))?
                .join(".openwave"),
        };
        Ok(Self {
            profile,
            data_dir,
            keychain_service: None,
        })
    }

    /// The default `Store` connection URL for this config.
    ///
    /// Desktop derives a SQLite file under `data_dir`; self-host expects the
    /// connection string in `OPENWAVE_DATABASE_URL` (Postgres).
    pub fn database_url(&self) -> Result<String> {
        match self.profile {
            Profile::Desktop => Ok(format!(
                "sqlite://{}?mode=rwc",
                self.data_dir.join("openwave.db").display()
            )),
            Profile::SelfHost => std::env::var("OPENWAVE_DATABASE_URL")
                .map_err(|_| AgentError::config("OPENWAVE_DATABASE_URL is required for self-host")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_derives_a_sqlite_url_under_data_dir() {
        // A real, platform-correct absolute path (hard-coded `/tmp/...` isn't
        // portable to Windows); assert the SQLite wrapper around it.
        let dir = tempfile::tempdir().unwrap();
        let config = Config::desktop(dir.path());
        assert_eq!(config.profile, Profile::Desktop);
        assert_eq!(
            config.database_url().unwrap(),
            format!(
                "sqlite://{}?mode=rwc",
                dir.path().join("openwave.db").display()
            )
        );
    }

    #[test]
    fn empty_data_dir_var_falls_back_to_the_default() {
        // `OPENWAVE_DATA_DIR=` (set but empty) must behave like unset, not point
        // the store at `openwave.db` in the current directory.
        let config = Config::from_vars(None, Some(OsString::new())).unwrap();
        let expected = std::env::current_dir().unwrap().join(".openwave");
        assert_eq!(config.data_dir, expected);
        assert_eq!(config.profile, Profile::Desktop);
    }

    #[test]
    fn empty_profile_var_defaults_to_desktop() {
        let config = Config::from_vars(Some(String::new()), Some(OsString::from("/data"))).unwrap();
        assert_eq!(config.profile, Profile::Desktop);
    }

    #[test]
    fn explicit_vars_are_honored() {
        let config =
            Config::from_vars(Some("self_host".into()), Some(OsString::from("/data"))).unwrap();
        assert_eq!(config.profile, Profile::SelfHost);
        assert_eq!(config.data_dir, PathBuf::from("/data"));
    }

    #[test]
    fn unknown_profile_var_is_an_error() {
        assert!(Config::from_vars(Some("bogus".into()), None).is_err());
    }

    #[test]
    fn config_roundtrips_through_json() {
        let config = Config::desktop("/data");
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), config);
    }

    #[test]
    fn profile_defaults_to_desktop() {
        assert_eq!(Profile::default(), Profile::Desktop);
    }
}
