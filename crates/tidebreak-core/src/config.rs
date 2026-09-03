//! Application boot configuration.
//!
//! Two layers, deliberately separate:
//!
//! - **Boot config** — this module. The immutable, start-of-day settings needed
//!   *before* the store exists: which [`Profile`] to run and where app data
//!   lives. The profile selects which backend implementations get wired at boot
//!   (SQLite vs Postgres, keychain vs Vault, …).
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
    /// Self-hosted server: Postgres, Vault or environment secrets, object storage.
    SelfHost,
}

/// HashiCorp Vault KV v2 custody for a self-host profile's stored secrets.
///
/// The token is read from `token_file` by the server for every Vault request.
/// Keeping the credential out of this serializable boot config prevents it
/// from entering diagnostics or persisted configuration by accident.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultSecretConfig {
    /// Vault server base URL. HTTPS is required outside literal loopback development.
    pub address: String,
    /// Mounted file containing the Vault token.
    pub token_file: PathBuf,
    /// KV v2 mount path, such as `secret`.
    pub mount: String,
    /// Secret path below the mount, such as `tidebreak/production`.
    pub path: String,
    /// Optional Vault Enterprise or HCP namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub namespace: Option<String>,
}

impl VaultSecretConfig {
    const DEFAULT_MOUNT: &'static str = "secret";
    const DEFAULT_PATH: &'static str = "tidebreak";

    fn from_vars(
        address: Option<String>,
        token_file: Option<OsString>,
        mount: Option<String>,
        path: Option<String>,
        namespace: Option<String>,
    ) -> Result<Option<Self>> {
        let address = address.filter(|value| !value.trim().is_empty());
        let token_file = token_file
            .filter(|value| !value.is_empty())
            .map(PathBuf::from);
        let mount = mount.filter(|value| !value.trim().is_empty());
        let path = path.filter(|value| !value.trim().is_empty());
        let namespace = namespace.filter(|value| !value.trim().is_empty());
        let configured = address.is_some()
            || token_file.is_some()
            || mount.is_some()
            || path.is_some()
            || namespace.is_some();
        if !configured {
            return Ok(None);
        }
        let address = address.ok_or_else(|| {
            AgentError::config(
                "TIDEBREAK_VAULT_ADDR is required when Vault secret custody is configured",
            )
        })?;
        let token_file = token_file.ok_or_else(|| {
            AgentError::config(
                "TIDEBREAK_VAULT_TOKEN_FILE is required when Vault secret custody is configured",
            )
        })?;
        let mount = normalize_vault_path(
            "TIDEBREAK_VAULT_MOUNT",
            mount.as_deref().unwrap_or(Self::DEFAULT_MOUNT),
        )?;
        let path = normalize_vault_path(
            "TIDEBREAK_VAULT_PATH",
            path.as_deref().unwrap_or(Self::DEFAULT_PATH),
        )?;
        Ok(Some(Self {
            address: address.trim().to_string(),
            token_file,
            mount,
            path,
            namespace: namespace.map(|value| value.trim().to_string()),
        }))
    }
}

fn normalize_vault_path(name: &str, value: &str) -> Result<String> {
    let value = value.trim().trim_matches('/');
    if value.is_empty()
        || value
            .split('/')
            .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(AgentError::config(format!(
            "{name} must contain non-empty path segments and no `.` or `..` segment"
        )));
    }
    Ok(value.to_string())
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
    /// S3 bucket and optional prefix used by the self-host blob store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_store_url: Option<String>,
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
    /// Remote credential custody for the self-host profile.
    ///
    /// When absent, provider environment variables remain readable, but
    /// deployment-plane credential writes fail with an operator-facing setup
    /// error instead of reaching the desktop OS keychain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_secrets: Option<VaultSecretConfig>,
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
    /// The confined-sandbox runtime endpoint slug remote sessions are
    /// provisioned through (`docs/slack-sessions.md`). Meaningful only on a
    /// gateway-authenticated hosted machine, where the sandbox calls ride the
    /// same gateway as authentication; absent, remote sessions are off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_endpoint: Option<String>,
    /// The administrator-defined sandbox profile named on every remote spawn.
    /// Required together with [`Config::runtime_endpoint`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_profile: Option<String>,
    /// Concurrent remote sandboxes one owner may hold. The reservation is
    /// atomic, so competing starts cannot exceed this cap.
    #[serde(
        default = "default_runtime_concurrency_cap",
        skip_serializing_if = "runtime_concurrency_cap_is_default"
    )]
    pub runtime_concurrency_cap: usize,
    /// Tidebreak's per-spawn spend ceiling in micro-USD. The runtime profile
    /// may impose a lower ceiling. `None` leaves this ceiling to the profile.
    #[serde(
        default = "default_runtime_spawn_spend_ceiling_microusd",
        skip_serializing_if = "runtime_spawn_spend_ceiling_is_default"
    )]
    pub runtime_spawn_spend_ceiling_microusd: Option<i64>,
    /// Cumulative per-session spend ceiling in micro-USD. `None` removes the
    /// Tidebreak ledger ceiling; the runtime profile still bounds each spawn.
    #[serde(
        default = "default_runtime_session_spend_ceiling_microusd",
        skip_serializing_if = "runtime_session_spend_ceiling_is_default"
    )]
    pub runtime_session_spend_ceiling_microusd: Option<i64>,
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
    /// A built renderer bundle to serve to browsers, so the machine has a
    /// page of its own to land on.
    ///
    /// The directory is the desktop UI's `dist` output — the same bundle the
    /// packaged app loads from its own protocol — and its `index.html` is the
    /// answer for any page navigation the API does not own. The self-host
    /// image sets `TIDEBREAK_UI_DIST` to the copy it carries; absent, the
    /// server serves no pages at all and an unknown path stays a `404`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_dist: Option<PathBuf>,
}

/// The bind every profile uses when no address is configured: loopback, and
/// an ephemeral port the OS picks.
const DEFAULT_BIND: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 0);

const DEFAULT_RUNTIME_CONCURRENCY_CAP: usize = 3;
const DEFAULT_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD: i64 = 5_000_000;
const DEFAULT_RUNTIME_SESSION_SPEND_CEILING_MICROUSD: i64 = 20_000_000;

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

    /// The Model Gateway this deployment authenticates its callers against,
    /// when it is a gateway-authenticated hosted machine.
    ///
    /// One reading of "hosted machine" for every surface that describes the
    /// deployment: the self-host profile with a gateway named by
    /// [`Config::auth_gateway_url`]. The public identity URL, deliberately —
    /// [`Config::auth_gateway_verifier_url`] is a server-to-server detour and
    /// names nothing a person recognizes.
    ///
    /// `None` everywhere else: the desktop profile, and a self-host server on
    /// static tokens. Neither holds a caller's gateway credential.
    #[must_use]
    pub fn hosted_gateway_url(&self) -> Option<&str> {
        if self.profile != Profile::SelfHost {
            return None;
        }
        self.auth_gateway_url.as_deref()
    }

    /// A desktop-profile config rooted at `data_dir`.
    pub fn desktop(data_dir: impl Into<PathBuf>) -> Self {
        Self {
            profile: Profile::Desktop,
            data_dir: data_dir.into(),
            blob_store_url: None,
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
            vault_secrets: None,
            listen_addr: None,
            runtime_endpoint: None,
            runtime_profile: None,
            runtime_concurrency_cap: default_runtime_concurrency_cap(),
            runtime_spawn_spend_ceiling_microusd: default_runtime_spawn_spend_ceiling_microusd(),
            runtime_session_spend_ceiling_microusd: default_runtime_session_spend_ceiling_microusd(
            ),
            code_worktree_root_default: None,
            ui_dist: None,
        }
    }

    /// Load from the environment, falling back to sensible defaults:
    /// `TIDEBREAK_PROFILE` (default `desktop`), `TIDEBREAK_DATA_DIR` (default
    /// `./.tidebreak` under the current directory — desktop/CLI clients should set
    /// this to the platform's app-data location),
    /// `TIDEBREAK_BLOB_STORE_URL` (required for self-host; an S3 bucket and
    /// optional prefix), with the Model Gateway add-on plane's
    /// `GATEWAY_BASE_URL`, `DATABASE_URL`, and `ADD_ON_PUBLIC_URL` standing in
    /// for `TIDEBREAK_AUTH_GATEWAY_URL`, `TIDEBREAK_DATABASE_URL`, and
    /// `TIDEBREAK_PUBLIC_URL` when those are unset (decision 0085),
    /// `TIDEBREAK_CONTAINER_EXECUTION_ENABLED` (default `false`),
    /// `TIDEBREAK_CONTAINER_IMAGE` (defaulting to the server's default agent
    /// image), `TIDEBREAK_AUTH_TOKENS_FILE` or `TIDEBREAK_AUTH_GATEWAY_URL`
    /// (self-host only; exactly one is required there), optional
    /// `TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL` for cluster-internal validation,
    /// `TIDEBREAK_PUBLIC_URL` for machine-bound Gateway credentials, optional
    /// `TIDEBREAK_VAULT_ADDR` and `TIDEBREAK_VAULT_TOKEN_FILE` for self-host
    /// credential custody, plus optional `TIDEBREAK_VAULT_MOUNT`,
    /// `TIDEBREAK_VAULT_PATH`, and `TIDEBREAK_VAULT_NAMESPACE`,
    /// `TIDEBREAK_LISTEN_ADDR` (self-host only; default loopback on an
    /// ephemeral port), and optional `TIDEBREAK_RUNTIME_ENDPOINT` plus
    /// `TIDEBREAK_RUNTIME_PROFILE` together to enable remote sessions. Remote
    /// deployments can also set `TIDEBREAK_RUNTIME_CONCURRENCY_CAP`,
    /// `TIDEBREAK_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD`, and
    /// `TIDEBREAK_RUNTIME_SESSION_SPEND_CEILING_MICROUSD`. Optional
    /// `TIDEBREAK_UI_DIST` names a built renderer bundle to serve to browsers.
    pub fn from_env() -> Result<Self> {
        let vault_secrets = VaultSecretConfig::from_vars(
            std::env::var("TIDEBREAK_VAULT_ADDR").ok(),
            std::env::var_os("TIDEBREAK_VAULT_TOKEN_FILE"),
            std::env::var("TIDEBREAK_VAULT_MOUNT").ok(),
            std::env::var("TIDEBREAK_VAULT_PATH").ok(),
            std::env::var("TIDEBREAK_VAULT_NAMESPACE").ok(),
        )?;
        Self::from_vars(
            std::env::var("TIDEBREAK_PROFILE").ok(),
            std::env::var_os("TIDEBREAK_DATA_DIR"),
            std::env::var("TIDEBREAK_BLOB_STORE_URL").ok(),
            std::env::var("TIDEBREAK_CONTAINER_EXECUTION_ENABLED").ok(),
            std::env::var("TIDEBREAK_CONTAINER_IMAGE").ok(),
            std::env::var_os("TIDEBREAK_AUTH_TOKENS_FILE"),
            plane_fallback(
                std::env::var("TIDEBREAK_AUTH_GATEWAY_URL").ok(),
                std::env::var("GATEWAY_BASE_URL").ok(),
            ),
            std::env::var("TIDEBREAK_AUTH_GATEWAY_VERIFIER_URL").ok(),
            plane_fallback(
                std::env::var("TIDEBREAK_PUBLIC_URL").ok(),
                std::env::var("ADD_ON_PUBLIC_URL").ok(),
            ),
            std::env::var("TIDEBREAK_LISTEN_ADDR").ok(),
            std::env::var("TIDEBREAK_RUNTIME_ENDPOINT").ok(),
            std::env::var("TIDEBREAK_RUNTIME_PROFILE").ok(),
            vault_secrets,
        )?
        .with_runtime_limit_vars(
            std::env::var("TIDEBREAK_RUNTIME_CONCURRENCY_CAP").ok(),
            std::env::var("TIDEBREAK_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD").ok(),
            std::env::var("TIDEBREAK_RUNTIME_SESSION_SPEND_CEILING_MICROUSD").ok(),
        )
        .map(|config| config.with_ui_dist_var(std::env::var_os("TIDEBREAK_UI_DIST")))
    }

    /// Apply `TIDEBREAK_UI_DIST`. Split from [`Config::from_env`] like the
    /// runtime limits, so the empty-means-unset rule is testable without
    /// touching the process environment. Whether the directory actually holds
    /// a bundle is checked where the server binds, so the refusal names the
    /// path and happens before anything listens.
    #[must_use]
    pub fn with_ui_dist_var(mut self, ui_dist: Option<OsString>) -> Self {
        self.ui_dist = ui_dist.filter(|value| !value.is_empty()).map(PathBuf::from);
        self
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
        blob_store_url: Option<String>,
        container_execution_enabled: Option<String>,
        container_image: Option<String>,
        auth_tokens_file: Option<OsString>,
        auth_gateway_url: Option<String>,
        auth_gateway_verifier_url: Option<String>,
        public_url: Option<String>,
        listen_addr: Option<String>,
        runtime_endpoint: Option<String>,
        runtime_profile: Option<String>,
        vault_secrets: Option<VaultSecretConfig>,
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
        let blob_store_url = blob_store_url.filter(|value| !value.trim().is_empty());
        match (profile, blob_store_url.as_deref()) {
            (Profile::SelfHost, None) => {
                return Err(AgentError::config(
                    "TIDEBREAK_BLOB_STORE_URL is required for self-host",
                ));
            }
            (Profile::Desktop, Some(_)) => {
                return Err(AgentError::config(
                    "TIDEBREAK_BLOB_STORE_URL is only valid for self-host",
                ));
            }
            _ => {}
        }
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
        if profile != Profile::SelfHost && vault_secrets.is_some() {
            return Err(AgentError::config(
                "Vault secret custody is available only with TIDEBREAK_PROFILE=self_host",
            ));
        }
        let listen_addr = match listen_addr.filter(|value| !value.is_empty()) {
            None => None,
            Some(value) => Some(value.parse::<SocketAddr>().map_err(|_| {
                AgentError::config(format!(
                    "invalid TIDEBREAK_LISTEN_ADDR {value:?}: expected an address and port, \
                     e.g. `0.0.0.0:8080`"
                ))
            })?),
        };
        let runtime_endpoint = runtime_endpoint.filter(|value| !value.trim().is_empty());
        let runtime_profile = runtime_profile.filter(|value| !value.trim().is_empty());
        if runtime_endpoint.is_some() != runtime_profile.is_some() {
            return Err(AgentError::config(
                "TIDEBREAK_RUNTIME_ENDPOINT and TIDEBREAK_RUNTIME_PROFILE are required together",
            ));
        }
        Ok(Self {
            profile,
            data_dir,
            blob_store_url,
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
            vault_secrets,
            listen_addr,
            runtime_endpoint,
            runtime_profile,
            runtime_concurrency_cap: default_runtime_concurrency_cap(),
            runtime_spawn_spend_ceiling_microusd: default_runtime_spawn_spend_ceiling_microusd(),
            runtime_session_spend_ceiling_microusd: default_runtime_session_spend_ceiling_microusd(
            ),
            code_worktree_root_default: None,
            ui_dist: None,
        })
    }

    /// Apply the three operator-controlled remote runtime limits. Split from
    /// [`Config::from_env`] so tests can cover invalid values without mutating
    /// process-global environment variables.
    fn with_runtime_limit_vars(
        mut self,
        concurrency_cap: Option<String>,
        spawn_spend_ceiling_microusd: Option<String>,
        session_spend_ceiling_microusd: Option<String>,
    ) -> Result<Self> {
        self.runtime_concurrency_cap = parse_positive_usize(
            "TIDEBREAK_RUNTIME_CONCURRENCY_CAP",
            concurrency_cap,
            DEFAULT_RUNTIME_CONCURRENCY_CAP,
        )?;
        self.runtime_spawn_spend_ceiling_microusd = parse_spend_ceiling(
            "TIDEBREAK_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD",
            spawn_spend_ceiling_microusd,
            Some(DEFAULT_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD),
        )?;
        self.runtime_session_spend_ceiling_microusd = parse_spend_ceiling(
            "TIDEBREAK_RUNTIME_SESSION_SPEND_CEILING_MICROUSD",
            session_spend_ceiling_microusd,
            Some(DEFAULT_RUNTIME_SESSION_SPEND_CEILING_MICROUSD),
        )?;
        Ok(self)
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
            Profile::SelfHost => plane_fallback(
                std::env::var("TIDEBREAK_DATABASE_URL").ok(),
                std::env::var("DATABASE_URL").ok(),
            )
            .ok_or_else(|| AgentError::config("TIDEBREAK_DATABASE_URL is required for self-host")),
        }
    }
}

/// The Model Gateway add-on plane's environment contract, honored when the
/// server's own variable is unset (decision 0085). A managed machine is
/// handed `GATEWAY_BASE_URL`, `DATABASE_URL`, and `ADD_ON_PUBLIC_URL` by the
/// plane; the `TIDEBREAK_*` name always wins when both are set, and an empty
/// value counts as unset on either side, so a blank override never hides the
/// plane's value.
fn plane_fallback(own: Option<String>, plane: Option<String>) -> Option<String> {
    own.filter(|value| !value.trim().is_empty())
        .or_else(|| plane.filter(|value| !value.trim().is_empty()))
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

fn default_runtime_concurrency_cap() -> usize {
    DEFAULT_RUNTIME_CONCURRENCY_CAP
}

fn runtime_concurrency_cap_is_default(value: &usize) -> bool {
    *value == DEFAULT_RUNTIME_CONCURRENCY_CAP
}

fn default_runtime_spawn_spend_ceiling_microusd() -> Option<i64> {
    Some(DEFAULT_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD)
}

fn runtime_spawn_spend_ceiling_is_default(value: &Option<i64>) -> bool {
    *value == Some(DEFAULT_RUNTIME_SPAWN_SPEND_CEILING_MICROUSD)
}

fn default_runtime_session_spend_ceiling_microusd() -> Option<i64> {
    Some(DEFAULT_RUNTIME_SESSION_SPEND_CEILING_MICROUSD)
}

fn runtime_session_spend_ceiling_is_default(value: &Option<i64>) -> bool {
    *value == Some(DEFAULT_RUNTIME_SESSION_SPEND_CEILING_MICROUSD)
}

fn parse_positive_usize(name: &str, value: Option<String>, default: usize) -> Result<usize> {
    let Some(value) = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(default);
    };
    value
        .parse::<usize>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .ok_or_else(|| {
            AgentError::config(format!(
                "invalid {name}: expected a positive integer, got {value:?}"
            ))
        })
}

fn parse_spend_ceiling(
    name: &str,
    value: Option<String>,
    default: Option<i64>,
) -> Result<Option<i64>> {
    let Some(value) = value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return Ok(default);
    };
    if value.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    value
        .parse::<i64>()
        .ok()
        .filter(|parsed| *parsed > 0)
        .map(Some)
        .ok_or_else(|| {
            AgentError::config(format!(
                "invalid {name}: expected a positive integer in micro-USD or `none`, got {value:?}"
            ))
        })
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_plane_contract_stands_in_only_when_the_own_variable_is_unset() {
        // Decision 0085: the plane's name fills an unset or blank own
        // variable and never overrides a set one.
        assert_eq!(
            super::plane_fallback(None, Some("https://gateway.example".into())),
            Some("https://gateway.example".into())
        );
        assert_eq!(
            super::plane_fallback(Some("  ".into()), Some("postgres://plane".into())),
            Some("postgres://plane".into())
        );
        assert_eq!(
            super::plane_fallback(Some("https://own".into()), Some("https://plane".into())),
            Some("https://own".into())
        );
        assert_eq!(super::plane_fallback(None, Some(String::new())), None);
        assert_eq!(super::plane_fallback(None, None), None);
    }

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
    fn an_empty_ui_dist_var_means_no_bundle() {
        let config = Config::desktop("/data");
        assert_eq!(
            config
                .clone()
                .with_ui_dist_var(Some(OsString::new()))
                .ui_dist,
            None
        );
        assert_eq!(config.clone().with_ui_dist_var(None).ui_dist, None);
        assert_eq!(
            config
                .with_ui_dist_var(Some(OsString::from("/opt/tidebreak/ui")))
                .ui_dist
                .as_deref(),
            Some(std::path::Path::new("/opt/tidebreak/ui"))
        );
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
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(config.profile, Profile::Desktop);
    }

    #[test]
    fn a_runtime_endpoint_requires_its_profile() {
        // Half-configured remote execution refuses at boot rather than at the
        // first spawn.
        let refused = Config::from_vars(
            None,
            Some(OsString::from("/data")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("primary".into()),
            None,
            None,
        );
        assert!(refused.is_err());
        let config = Config::from_vars(
            None,
            Some(OsString::from("/data")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some("primary".into()),
            Some("tidebreak-remote".into()),
            None,
        )
        .unwrap();
        assert_eq!(config.runtime_endpoint.as_deref(), Some("primary"));
        assert_eq!(config.runtime_profile.as_deref(), Some("tidebreak-remote"));
    }

    #[test]
    fn remote_runtime_limit_vars_are_validated_and_honored() {
        let config = Config::desktop("/data")
            .with_runtime_limit_vars(
                Some("8".into()),
                Some("7500000".into()),
                Some("none".into()),
            )
            .unwrap();
        assert_eq!(config.runtime_concurrency_cap, 8);
        assert_eq!(config.runtime_spawn_spend_ceiling_microusd, Some(7_500_000));
        assert_eq!(config.runtime_session_spend_ceiling_microusd, None);

        for invalid in ["0", "-1", "many"] {
            assert!(
                Config::desktop("/data")
                    .with_runtime_limit_vars(Some(invalid.into()), None, None)
                    .is_err(),
                "concurrency cap {invalid:?} must fail"
            );
        }
        for invalid in ["0", "-1", "5.00"] {
            assert!(
                Config::desktop("/data")
                    .with_runtime_limit_vars(None, Some(invalid.into()), None)
                    .is_err(),
                "spawn ceiling {invalid:?} must fail"
            );
        }
    }

    #[test]
    fn explicit_vars_are_honored() {
        let config = Config::from_vars(
            Some("self_host".into()),
            Some(OsString::from("/data")),
            Some("s3://tidebreak/blobs".into()),
            None,
            None,
            Some(OsString::from("/etc/tidebreak/tokens")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(config.profile, Profile::SelfHost);
        assert_eq!(config.data_dir, PathBuf::from("/data"));
        assert_eq!(
            config.blob_store_url.as_deref(),
            Some("s3://tidebreak/blobs")
        );
        assert_eq!(
            config.auth_tokens_file,
            Some(PathBuf::from("/etc/tidebreak/tokens"))
        );
    }

    #[test]
    fn self_host_requires_a_blob_store_url() {
        let error = Config::from_vars(
            Some("self_host".into()),
            Some(OsString::from("/data")),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("TIDEBREAK_BLOB_STORE_URL is required for self-host"));
    }

    #[test]
    fn desktop_rejects_a_blob_store_url() {
        let error = Config::from_vars(
            Some("desktop".into()),
            Some(OsString::from("/data")),
            Some("s3://tidebreak/blobs".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("TIDEBREAK_BLOB_STORE_URL is only valid for self-host"));
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
            None,
            None,
            None,
            None,
            None,
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
            Some("s3://tidebreak/blobs".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("0.0.0.0:8080".into()),
            None,
            None,
            None,
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
            Some("s3://tidebreak/blobs".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            Some("0.0.0.0".into()),
            None,
            None,
            None
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
        assert_eq!(config.blob_store_url, None);
        assert_eq!(config.exec_scripts_dir, None);
        assert_eq!(config.exec_skills_dir, None);
        assert!(!config.container_execution_enabled);
        assert_eq!(config.container_image, None);
        assert_eq!(config.auth_tokens_file, None);
        assert_eq!(config.auth_gateway_url, None);
        assert_eq!(config.auth_gateway_verifier_url, None);
        assert_eq!(config.public_url, None);
        assert_eq!(config.runtime_concurrency_cap, 3);
        assert_eq!(config.runtime_spawn_spend_ceiling_microusd, Some(5_000_000));
        assert_eq!(
            config.runtime_session_spend_ceiling_microusd,
            Some(20_000_000)
        );
        assert_eq!(config.vault_secrets, None);
    }

    #[test]
    fn container_execution_defaults_off_without_changing_default_json() {
        let config = Config::desktop("/data");
        let json = serde_json::to_value(config).unwrap();
        assert_eq!(json.get("container_execution_enabled"), None);
        assert_eq!(json.get("container_image"), None);
        assert_eq!(json.get("runtime_concurrency_cap"), None);
        assert_eq!(json.get("runtime_spawn_spend_ceiling_microusd"), None);
        assert_eq!(json.get("runtime_session_spend_ceiling_microusd"), None);
    }

    #[test]
    fn an_explicit_missing_runtime_ceiling_roundtrips() {
        let mut config = Config::desktop("/data");
        config.runtime_spawn_spend_ceiling_microusd = None;
        config.runtime_session_spend_ceiling_microusd = None;

        let json = serde_json::to_string(&config).unwrap();
        let restored = serde_json::from_str::<Config>(&json).unwrap();
        assert_eq!(restored.runtime_spawn_spend_ceiling_microusd, None);
        assert_eq!(restored.runtime_session_spend_ceiling_microusd, None);
    }

    #[test]
    fn container_execution_vars_are_validated_and_honored() {
        let config = Config::from_vars(
            None,
            Some(OsString::from("/data")),
            None,
            Some("true".into()),
            Some("tidebreak-sandbox-agent:dev".into()),
            None,
            None,
            None,
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
            None,
            Some("yes".into()),
            None,
            None,
            None,
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
            Some("s3://tidebreak/blobs".into()),
            None,
            None,
            None,
            Some("https://gateway.example.test".into()),
            Some("https://gateway.model-gateway.svc.cluster.local".into()),
            Some("https://tidebreak.example.test".into()),
            None,
            None,
            None,
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
    fn complete_vault_vars_are_normalized() {
        let vault = VaultSecretConfig::from_vars(
            Some(" https://vault.example.test ".into()),
            Some(OsString::from("/run/secrets/vault-token")),
            Some("/secret/".into()),
            Some("/tidebreak/production/".into()),
            Some(" platform/team-a ".into()),
        )
        .unwrap()
        .unwrap();

        assert_eq!(vault.address, "https://vault.example.test");
        assert_eq!(vault.token_file, PathBuf::from("/run/secrets/vault-token"));
        assert_eq!(vault.mount, "secret");
        assert_eq!(vault.path, "tidebreak/production");
        assert_eq!(vault.namespace.as_deref(), Some("platform/team-a"));

        let config = Config::from_vars(
            Some("self_host".into()),
            Some(OsString::from("/data")),
            Some("s3://tidebreak/blobs".into()),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            None,
            Some(vault.clone()),
        )
        .unwrap();
        assert_eq!(config.vault_secrets, Some(vault));
    }

    #[test]
    fn incomplete_vault_vars_are_rejected() {
        let missing_address = VaultSecretConfig::from_vars(
            None,
            Some(OsString::from("/run/secrets/vault-token")),
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(missing_address.contains("TIDEBREAK_VAULT_ADDR"));

        let missing_token_file = VaultSecretConfig::from_vars(
            Some("https://vault.example.test".into()),
            None,
            None,
            None,
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(missing_token_file.contains("TIDEBREAK_VAULT_TOKEN_FILE"));
    }

    #[test]
    fn invalid_vault_paths_are_rejected() {
        for (mount, path) in [
            ("/", "tidebreak"),
            ("secret", "tidebreak//production"),
            ("secret/../other", "tidebreak"),
            ("secret", "./tidebreak"),
        ] {
            let error = VaultSecretConfig::from_vars(
                Some("https://vault.example.test".into()),
                Some(OsString::from("/run/secrets/vault-token")),
                Some(mount.into()),
                Some(path.into()),
                None,
            )
            .unwrap_err()
            .to_string();
            assert!(
                error.contains("TIDEBREAK_VAULT_MOUNT") || error.contains("TIDEBREAK_VAULT_PATH"),
                "unexpected error for mount={mount:?} path={path:?}: {error}"
            );
        }
    }

    #[test]
    fn desktop_profile_rejects_vault_settings() {
        let vault = VaultSecretConfig::from_vars(
            Some("https://vault.example.test".into()),
            Some(OsString::from("/run/secrets/vault-token")),
            None,
            None,
            None,
        )
        .unwrap();
        let error = Config::from_vars(
            None, None, None, None, None, None, None, None, None, None, None, None, vault,
        )
        .unwrap_err()
        .to_string();
        assert!(error.contains("TIDEBREAK_PROFILE=self_host"));
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
