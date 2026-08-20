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
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

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
#[non_exhaustive]
pub struct Config {
    /// The deployment profile.
    #[serde(default)]
    pub profile: Profile,
    /// Directory holding the app's data (database, blobs, …).
    pub data_dir: PathBuf,
    /// Keychain service name secrets are stored under; `None` uses the
    /// default (`tidebreak`). Host applications whose builds must not share
    /// secret state — a dev build running beside an installed release — set
    /// a distinct service here, mirroring the app-identifier and data-dir
    /// split.
    #[serde(default)]
    pub keychain_service: Option<String>,
    /// The host app's OS bundle identifier, when it runs as one (the desktop
    /// app; its debug and staging builds use a distinct id). On macOS this names
    /// the managed-preferences domain consulted for OS-managed (MDM) policy.
    /// `None` — the CLI, tests, self-host — only disables that macOS reader;
    /// the Windows and Linux readers are machine-scoped and ignore it.
    #[serde(default)]
    pub bundle_id: Option<String>,
    /// Trusted source directory for helper scripts copied into isolated exec
    /// workspaces. Desktop resolves this from its signed application resources;
    /// other embeddings leave it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_scripts_dir: Option<PathBuf>,
    /// Trusted source directory for built-in skill packages staged into
    /// isolated exec workspaces. Desktop resolves this from its signed
    /// application resources; other embeddings leave it absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_skills_dir: Option<PathBuf>,
    /// Trusted source directory for the built-in plugin manifests that group
    /// those skills. Desktop resolves this from its signed application
    /// resources; other embeddings leave it absent and every skill stands
    /// alone in the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_plugins_dir: Option<PathBuf>,
    /// Trusted source directory for built-in reusable prompt packages. Prompts
    /// are user-side text, never staged and never advertised to the model, so
    /// an embedding that leaves this absent simply has no curated prompts —
    /// user-authored ones still load from the data directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exec_prompts_dir: Option<PathBuf>,
    /// Whether newly spawned background agent runs may execute inside a local
    /// container when the configured runtime is available. Disabled by default,
    /// so existing installations keep the in-process scheduler path.
    #[serde(default, skip_serializing_if = "is_false")]
    pub container_execution_enabled: bool,
    /// Optional container image override for sandbox-resident agent runs.
    /// `None` uses the server's default: the published documents agent image
    /// pinned by digest, or the locally built development image while no
    /// digest is recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub container_image: Option<String>,
    /// Path to the self-host profile's static bearer-token file, mapping each
    /// token to the user it authenticates (format documented in the server's
    /// auth module). Consulted only by [`Profile::SelfHost`], which requires
    /// it; the desktop profile authenticates with its per-launch bearer and
    /// ignores this.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_tokens_file: Option<PathBuf>,
    /// Model Gateway base URL used to authenticate self-hosted callers.
    ///
    /// Mutually exclusive with [`Config::auth_tokens_file`]. The server sends
    /// each presented `tidebreak` resource token to the Gateway's live
    /// principal endpoint, so Gateway membership, deactivation, session
    /// revocation, and administrator changes apply without a generated roster.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_gateway_url: Option<String>,
    /// Optional server-to-server Model Gateway URL used only for principal
    /// validation. The public [`Config::auth_gateway_url`] remains the identity
    /// authority exposed to clients; this override supports clusters that
    /// cannot hairpin through that public origin.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_gateway_verifier_url: Option<String>,
    /// Canonical public base URL of this self-hosted Tidebreak machine.
    ///
    /// Gateway-backed authentication hashes this URL into the OAuth resource,
    /// binding every user credential to this one machine. It must be the same
    /// normalized URL desktop clients use to attach (including any ingress
    /// path prefix, without a trailing slash).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub public_url: Option<String>,
    /// The address the API binds, for deployments that must be reachable from
    /// outside the machine (a container publishing a port, say). `None` — the
    /// default — keeps the loopback, ephemeral-port bind every profile has
    /// always used.
    ///
    /// Honoured only by [`Profile::SelfHost`]. Setting it on the desktop
    /// profile is a boot error rather than a silent override: see
    /// [`Config::bind_addr`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listen_addr: Option<SocketAddr>,
    /// Where new code-mode worktrees land when no `code_worktree_root` setting
    /// is stored.
    ///
    /// Worktrees hold uncommitted work on real branches, so a windowed
    /// embedding points this at a visible place in the user's home directory —
    /// the desktop app uses `~/Tidebreak/workspaces` — rather than letting user
    /// work sit in disposable app data. Embeddings that leave it absent (the
    /// CLI, self-host, tests) keep worktrees under
    /// `{data_dir}/code/worktrees`, which is the right answer for a headless
    /// deployment whose data directory *is* its user-visible location. Either
    /// way the stored setting wins once an operator sets one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_worktree_root_default: Option<PathBuf>,
}

/// The bind every profile uses when no address is configured: loopback, and
/// an ephemeral port the OS picks.
const DEFAULT_BIND: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

impl Config {
    /// The per-install directory user-authored skill packages are read from.
    ///
    /// Derived from [`Config::data_dir`] rather than configured separately:
    /// every embedding has a data directory, and keeping user skills beside
    /// the rest of the app's data makes them one readable, editable,
    /// shareable tree (`{data_dir}/skills/<name>/SKILL.md`).
    #[must_use]
    pub fn user_skills_dir(&self) -> PathBuf {
        self.data_dir.join("skills")
    }

    /// The per-install directory user-authored plugins are read from,
    /// alongside `{data_dir}/skills` and derived the same way.
    #[must_use]
    pub fn user_plugins_dir(&self) -> PathBuf {
        self.data_dir.join("plugins")
    }

    /// The per-install directory user-authored prompt packages are read from
    /// (`{data_dir}/prompts/<name>/PROMPT.md`), derived the same way as
    /// [`Config::user_skills_dir`].
    #[must_use]
    pub fn user_prompts_dir(&self) -> PathBuf {
        self.data_dir.join("prompts")
    }

    /// The client-managed writable data directories bundled MCP servers are
    /// given as `PLUGIN_DATA` (`{data_dir}/plugin-data/<plugin>/`).
    ///
    /// A sibling of `{data_dir}/plugins` rather than a directory inside a
    /// plugin's own tree: the Agent Plugins specification requires this
    /// directory to survive an update of the package, and the package tree is
    /// exactly what an update replaces. Keeping it outside also means nothing
    /// a plugin writes at runtime can change what the package claims to be.
    #[must_use]
    pub fn plugin_data_dir(&self) -> PathBuf {
        self.data_dir.join("plugin-data")
    }

    /// A desktop-profile config rooted at `data_dir`.
    pub fn desktop(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            profile: Profile::Desktop,
            data_dir: data_dir.into(),
            keychain_service: None,
            bundle_id: None,
            exec_scripts_dir: None,
            exec_skills_dir: None,
            exec_plugins_dir: None,
            exec_prompts_dir: None,
            container_execution_enabled: false,
            container_image: None,
            auth_tokens_file: None,
            auth_gateway_url: None,
            auth_gateway_verifier_url: None,
            public_url: None,
            listen_addr: None,
            code_worktree_root_default: None,
        }
    }

    /// Load from the environment, falling back to sensible defaults:
    /// `TIDEBREAK_PROFILE` (default `desktop`), `TIDEBREAK_DATA_DIR` (default
    /// `./.tidebreak` under the current directory — desktop/CLI clients should set
    /// this to the platform's app-data location),
    /// `TIDEBREAK_CONTAINER_EXECUTION_ENABLED` (default `false`),
    /// `TIDEBREAK_CONTAINER_IMAGE` (defaulting to the server's default agent
    /// image), `TIDEBREAK_AUTH_TOKENS_FILE` or `TIDEBREAK_AUTH_GATEWAY_URL`
    /// (self-host only; exactly one is required there), optional
    /// `TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL` for cluster-internal validation,
    /// `TIDEBREAK_PUBLIC_URL` for machine-bound Gateway credentials,
    /// and `TIDEBREAK_LISTEN_ADDR` (self-host only; default loopback on an
    /// ephemeral port).
    pub fn from_env() -> Result<Self> {
        Self::from_vars(
            std::env::var("TIDEBREAK_PROFILE").ok(),
            std::env::var_os("TIDEBREAK_DATA_DIR"),
            std::env::var("TIDEBREAK_CONTAINER_EXECUTION_ENABLED").ok(),
            std::env::var("TIDEBREAK_CONTAINER_IMAGE").ok(),
            std::env::var_os("TIDEBREAK_AUTH_TOKENS_FILE"),
            std::env::var("TIDEBREAK_AUTH_GATEWAY_URL").ok(),
            std::env::var("TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL").ok(),
            std::env::var("TIDEBREAK_PUBLIC_URL").ok(),
            std::env::var("TIDEBREAK_LISTEN_ADDR").ok(),
        )
    }

    /// Resolve a config from raw variable values. Split out from [`from_env`] so
    /// the empty-vs-unset handling is testable without mutating process-global
    /// environment (which races other threads' `getenv` in a shared test binary).
    ///
    /// An **empty** value is treated as unset: a caller that exports
    /// `TIDEBREAK_DATA_DIR=` (or an empty profile) gets the documented defaults,
    /// not an empty path rooted at the current directory.
    #[allow(clippy::too_many_arguments)] // mirrors the environment variables one-to-one
    fn from_vars(
        profile: Option<String>,
        data_dir: Option<OsString>,
        container_execution_enabled: Option<String>,
        container_image: Option<String>,
        auth_tokens_file: Option<OsString>,
        auth_gateway_url: Option<String>,
        auth_gateway_verifier_url: Option<String>,
        public_url: Option<String>,
        listen_addr: Option<String>,
    ) -> Result<Self> {
        let profile = match profile.filter(|value| !value.is_empty()).as_deref() {
            None | Some("desktop") => Profile::Desktop,
            Some("self_host" | "selfhost") => Profile::SelfHost,
            Some(other) => return Err(AgentError::config(format!("unknown profile: {other}"))),
        };
        let data_dir = match data_dir.filter(|dir| !dir.is_empty()) {
            Some(dir) => PathBuf::from(dir),
            None => std::env::current_dir()
                .map_err(|e| AgentError::config(format!("no working directory: {e}")))?
                .join(".tidebreak"),
        };
        let container_execution_enabled =
            match container_execution_enabled.filter(|value| !value.is_empty()) {
                None => false,
                Some(value) => value.parse::<bool>().map_err(|_| {
                    AgentError::config(format!(
                        "invalid TIDEBREAK_CONTAINER_EXECUTION_ENABLED: {value}"
                    ))
                })?,
            };
        let container_image = container_image.filter(|value| !value.is_empty());
        let auth_tokens_file = auth_tokens_file
            .filter(|path| !path.is_empty())
            .map(PathBuf::from);
        let auth_gateway_url = auth_gateway_url.filter(|value| !value.trim().is_empty());
        let auth_gateway_verifier_url =
            auth_gateway_verifier_url.filter(|value| !value.trim().is_empty());
        let public_url = public_url.filter(|value| !value.trim().is_empty());
        let listen_addr = match listen_addr.filter(|value| !value.is_empty()) {
            None => None,
            Some(value) => Some(value.parse::<SocketAddr>().map_err(|_| {
                AgentError::config(format!(
                    "invalid TIDEBREAK_LISTEN_ADDR {value:?}: expected an address and port, \
                     e.g. `0.0.0.0:8080`"
                ))
            })?),
        };
        Ok(Self {
            profile,
            data_dir,
            keychain_service: None,
            bundle_id: None,
            exec_scripts_dir: None,
            exec_skills_dir: None,
            exec_plugins_dir: None,
            exec_prompts_dir: None,
            container_execution_enabled,
            container_image,
            auth_tokens_file,
            auth_gateway_url,
            auth_gateway_verifier_url,
            public_url,
            listen_addr,
            code_worktree_root_default: None,
        })
    }

    /// The socket the API should bind for this config.
    ///
    /// Absent configuration means loopback on an ephemeral port, which is what
    /// every profile did before the address was configurable.
    ///
    /// **The desktop profile refuses a configured address rather than
    /// honouring it.** That server's per-launch bearer is the only thing
    /// standing between the agent and any other process on the machine, and
    /// its `Origin`/`Host` checks assume a loopback bind; a desktop server
    /// listening on a routable interface would be a different security posture
    /// reached by an environment variable nobody reviewed. Failing the boot is
    /// the point — silently ignoring the variable would leave an operator
    /// believing they had published a port.
    pub fn bind_addr(&self) -> Result<SocketAddr> {
        match (self.profile, self.listen_addr) {
            (_, None) => Ok(DEFAULT_BIND),
            (Profile::SelfHost, Some(addr)) => Ok(addr),
            (Profile::Desktop, Some(addr)) => Err(AgentError::config(format!(
                "refusing to bind the desktop profile to {addr}: the desktop server is \
                 loopback-only, and its per-launch token is the only thing separating the \
                 agent from other local processes. Unset TIDEBREAK_LISTEN_ADDR, or run \
                 TIDEBREAK_PROFILE=self_host to publish a port"
            ))),
        }
    }

    /// The default `Store` connection URL for this config.
    ///
    /// Desktop derives a SQLite file under `data_dir`; self-host expects the
    /// connection string in `TIDEBREAK_DATABASE_URL` (Postgres).
    pub fn database_url(&self) -> Result<String> {
        match self.profile {
            Profile::Desktop => Ok(format!(
                "sqlite://{}?mode=rwc",
                self.data_dir.join("tidebreak.db").display()
            )),
            Profile::SelfHost => std::env::var("TIDEBREAK_DATABASE_URL").map_err(|_| {
                AgentError::config("TIDEBREAK_DATABASE_URL is required for self-host")
            }),
        }
    }
}

/// OAuth resource bound to one exact, already-canonical Tidebreak public URL.
///
/// Callers canonicalize first because URL parsing belongs at their trust
/// boundary: the server validates operator config, while desktop validates the
/// user-entered remote address. Hashing is shared so both sides cannot drift.
#[must_use]
pub fn tidebreak_machine_resource(canonical_public_url: &str) -> String {
    let digest = Sha256::digest(canonical_public_url.as_bytes());
    let hex: String = digest.iter().map(|byte| format!("{byte:02x}")).collect();
    format!("tidebreak:{hex}")
}

fn is_false(value: &bool) -> bool {
    !*value
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
                dir.path().join("tidebreak.db").display()
            )
        );
    }

    #[test]
    fn empty_data_dir_var_falls_back_to_the_default() {
        // `TIDEBREAK_DATA_DIR=` (set but empty) must behave like unset, not point
        // the store at `tidebreak.db` in the current directory.
        let config = Config::from_vars(
            None,
            Some(OsString::new()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        let expected = std::env::current_dir().unwrap().join(".tidebreak");
        assert_eq!(config.data_dir, expected);
        assert_eq!(config.profile, Profile::Desktop);
    }

    #[test]
    fn empty_profile_var_defaults_to_desktop() {
        let config = Config::from_vars(
            Some(String::new()),
            Some(OsString::from("/data")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(config.profile, Profile::Desktop);
    }

    #[test]
    fn explicit_vars_are_honored() {
        let config = Config::from_vars(
            Some("self_host".into()),
            Some(OsString::from("/data")),
            None,
            None,
            Some(OsString::from("/etc/tidebreak/tokens")),
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(config.profile, Profile::SelfHost);
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(
            config.auth_tokens_file,
            Some(PathBuf::from("/etc/tidebreak/tokens"))
        );
    }

    #[test]
    fn unknown_profile_var_is_an_error() {
        assert!(Config::from_vars(
            Some("bogus".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None
        )
        .is_err());
    }

    /// The desktop server is loopback-only by design, so a configured address
    /// must stop the boot rather than quietly not applying — an operator who
    /// set the variable would otherwise believe they had published a port.
    #[test]
    fn the_desktop_profile_refuses_a_configured_listen_address() {
        let mut config = Config::desktop("/data");
        assert_eq!(config.bind_addr().unwrap(), DEFAULT_BIND);

        config.listen_addr = Some("0.0.0.0:8080".parse().unwrap());
        let refusal = config.bind_addr().unwrap_err().to_string();
        assert!(
            refusal.contains("loopback-only") && refusal.contains("self_host"),
            "the refusal must name the remedy: {refusal}"
        );
    }

    #[test]
    fn self_host_binds_the_configured_address_and_rejects_a_malformed_one() {
        let config = Config::from_vars(
            Some("self_host".into()),
            Some(OsString::from("/data")),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("0.0.0.0:8080".into()),
        )
        .unwrap();
        assert_eq!(
            config.bind_addr().unwrap(),
            "0.0.0.0:8080".parse::<SocketAddr>().unwrap()
        );

        // A port-less host is the likely typo, and it must not silently fall
        // back to the loopback default the operator was trying to escape.
        assert!(Config::from_vars(
            Some("self_host".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("0.0.0.0".into())
        )
        .is_err());
    }

    #[test]
    fn config_roundtrips_through_json() {
        let config = Config::desktop("/data");
        let json = serde_json::to_string(&config).unwrap();
        assert_eq!(serde_json::from_str::<Config>(&json).unwrap(), config);
    }

    #[test]
    fn legacy_config_without_keychain_service_defaults_to_none() {
        let config = serde_json::from_str::<Config>(r#"{"data_dir":"/data"}"#).unwrap();
        assert_eq!(config.keychain_service, None);
        assert_eq!(config.exec_scripts_dir, None);
        assert_eq!(config.exec_skills_dir, None);
        assert!(!config.container_execution_enabled);
        assert_eq!(config.container_image, None);
        assert_eq!(config.auth_tokens_file, None);
        assert_eq!(config.auth_gateway_url, None);
        assert_eq!(config.auth_gateway_verifier_url, None);
        assert_eq!(config.public_url, None);
    }

    #[test]
    fn container_execution_defaults_off_without_changing_default_json() {
        let config = Config::desktop("/data");
        let json = serde_json::to_value(config).unwrap();
        assert_eq!(json.get("container_execution_enabled"), None);
        assert_eq!(json.get("container_image"), None);
    }

    #[test]
    fn container_execution_vars_are_validated_and_honored() {
        let config = Config::from_vars(
            None,
            Some(OsString::from("/data")),
            Some("true".into()),
            Some("tidebreak-sandbox-agent:dev".into()),
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert!(config.container_execution_enabled);
        assert_eq!(
            config.container_image.as_deref(),
            Some("tidebreak-sandbox-agent:dev")
        );
        assert!(Config::from_vars(
            None,
            None,
            Some("yes".into()),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .is_err());
    }

    #[test]
    fn gateway_auth_url_is_loaded_for_self_host() {
        let config = Config::from_vars(
            Some("self_host".into()),
            Some(OsString::from("/data")),
            None,
            None,
            None,
            Some("https://gateway.example.test".into()),
            Some("https://gateway.model-gateway.svc.cluster.local".into()),
            Some("https://tidebreak.example.test".into()),
            None,
        )
        .unwrap();
        assert_eq!(
            config.auth_gateway_url.as_deref(),
            Some("https://gateway.example.test")
        );
        assert_eq!(
            config.auth_gateway_verifier_url.as_deref(),
            Some("https://gateway.model-gateway.svc.cluster.local")
        );
        assert_eq!(
            config.public_url.as_deref(),
            Some("https://tidebreak.example.test")
        );
    }

    #[test]
    fn tidebreak_machine_resource_is_stable_and_url_specific() {
        assert_eq!(
            tidebreak_machine_resource("https://tidebreak.example.test"),
            "tidebreak:3c6444cbec9b33f56b4ed0f1bf7015741c69cf7e516977c52975c6a0012a097b"
        );
        assert_ne!(
            tidebreak_machine_resource("https://tidebreak.example.test"),
            tidebreak_machine_resource("https://other.example.test")
        );
    }
}
