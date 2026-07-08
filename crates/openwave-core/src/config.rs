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
}

impl Config {
    /// A desktop-profile config rooted at `data_dir`.
    pub fn desktop(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            profile: Profile::Desktop,
            data_dir: data_dir.into(),
        }
    }

    /// Load from the environment, falling back to sensible defaults:
    /// `OPENWAVE_PROFILE` (default `desktop`) and `OPENWAVE_DATA_DIR` (default
    /// `./.openwave` under the current directory — desktop/CLI clients should set
    /// this to the platform's app-data location).
    pub fn from_env() -> Result<Self> {
        let profile = match std::env::var("OPENWAVE_PROFILE").ok().as_deref() {
            None | Some("desktop") => Profile::Desktop,
            Some("self_host" | "selfhost") => Profile::SelfHost,
            Some(other) => return Err(AgentError::config(format!("unknown profile: {other}"))),
        };
        let data_dir = match std::env::var_os("OPENWAVE_DATA_DIR") {
            Some(dir) => PathBuf::from(dir),
            None => std::env::current_dir()
                .map_err(|e| AgentError::config(format!("no working directory: {e}")))?
                .join(".openwave"),
        };
        Ok(Self { profile, data_dir })
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
        let config = Config::desktop("/tmp/ow");
        assert_eq!(config.profile, Profile::Desktop);
        assert_eq!(
            config.database_url().unwrap(),
            "sqlite:///tmp/ow/openwave.db?mode=rwc"
        );
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
