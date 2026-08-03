//! Managed-mode policy: whether this profile is provisioned by a model
//! gateway, and on whose authority.
//!
//! Policy and session are separate layers with separate lifecycles: the
//! gateway session (keychain) comes and goes with sign-in, while the policy
//! (this module) persists across restarts and sign-out. Resolution honors a
//! fixed precedence — an OS-managed source (MDM) over sticky provisioned
//! state over the open default — so a device-management assertion can never
//! be shadowed by local state.
//!
//! Nothing here changes behavior yet. Lockdown of the BYOK and MCP write
//! paths, the settings surfaces, and the sign-in gate all read this policy
//! in follow-up slices. The provisioning write path is crate-internal by
//! design: its only intended callers are the deep-link pairing flow and
//! tests — it is deliberately not reachable from any renderer-writable
//! route, which is what makes the state sticky rather than a setting.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use openwave_core::{AgentError, Config, PermissionMode, Result, Store};
use serde::{Deserialize, Serialize};

const SETTING_KEY: &str = "managed_policy_v1";

/// The key every OS artifact stores the asserted URL under: the Windows
/// registry value and the macOS managed-preferences key share this name.
const MANAGED_GATEWAY_URL_KEY: &str = "GatewayURL";

/// The key an OS artifact stores the permission-mode ceiling under, shared by
/// the Windows registry value and the macOS managed-preferences key. The value
/// is a mode token (`plan`, `ask`, `auto`, `allow`); a chat may run at or
/// below it, never above.
const MANAGED_PERMISSION_MODE_KEY: &str = "MaximumPermissionMode";

/// The key an OS artifact stores the local-MCP allowance under, shared by the
/// Windows registry value and the macOS managed-preferences key. When true,
/// managed policy leaves local stdio MCP servers to the user; remote manual
/// (`url`) servers stay locked. Absent reads as false — deny is the managed
/// default, and the organization opts in explicitly.
const MANAGED_ALLOW_LOCAL_MCP_KEY: &str = "AllowLocalMcpServers";

/// Which authority asserted the active policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ManagedPolicySource {
    /// OS-managed device policy (MDM); not removable by the user in place.
    Os,
    /// Sticky state written when the app was paired with a gateway.
    Provisioned,
    /// No policy: the open, bring-your-own-key experience.
    Unmanaged,
}

/// Renderer-safe resolved policy. Carries only what surfaces need to render
/// managed state: the verdict, the locked gateway URL, and its authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ts_rs::TS)]
pub(crate) struct ManagedPolicy {
    pub(crate) managed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) gateway_url: Option<String>,
    pub(crate) source: ManagedPolicySource,
    /// True when `source` asserted management but its gateway URL is missing,
    /// unreadable, or invalid. The profile stays managed with no usable URL —
    /// fail closed — and surfaces can name the authority that needs repair
    /// instead of showing an opaque error.
    pub(crate) misconfigured: bool,
    /// A deep-link pairing awaiting the sign-in that is its consent. Runtime
    /// state merged in by the `/policy` route from [`GatewayRuntime`]
    /// (crate::gateway_runtime), never part of the durable resolution —
    /// [`resolve`] always leaves it `None` — and only ever present while the
    /// profile is unmanaged.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) pending_gateway_url: Option<String>,
    /// The highest permission mode any chat may run under, when the OS policy
    /// asserts one. A ceiling, not a fixed mode: the reader may always pick a
    /// stricter mode, and clearing back to the default is always allowed.
    /// Asserted per key, so it binds even when no gateway URL is deployed and
    /// the profile is otherwise unmanaged.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub(crate) permission_mode_ceiling: Option<PermissionMode>,
    /// True when the OS policy explicitly allows local stdio MCP servers on a
    /// managed profile. False by default — the managed lockdown covers every
    /// manual transport unless the organization opts in — and carries no
    /// meaning while unmanaged, where nothing is locked to begin with.
    pub(crate) allow_local_mcp_servers: bool,
}

impl ManagedPolicy {
    /// The fail-closed projection of an authority whose assertion cannot be
    /// honored: managed, no gateway, explicitly misconfigured.
    fn misconfigured(source: ManagedPolicySource) -> Self {
        Self {
            managed: true,
            gateway_url: None,
            source,
            misconfigured: true,
            pending_gateway_url: None,
            permission_mode_ceiling: None,
            allow_local_mcp_servers: false,
        }
    }

    /// Clamp a chat's stored mode to the asserted ceiling. `None` reads as
    /// the default (`Ask`), same as everywhere else the stored mode is
    /// interpreted, so a ceiling below the default binds unset chats too.
    pub(crate) fn clamp_permission_mode(
        &self,
        mode: Option<PermissionMode>,
    ) -> Option<PermissionMode> {
        match self.permission_mode_ceiling {
            Some(ceiling) if mode.unwrap_or(PermissionMode::Ask) > ceiling => Some(ceiling),
            _ => mode,
        }
    }

    /// Whether the reader may select `mode` under this policy.
    pub(crate) fn permits_permission_mode(&self, mode: PermissionMode) -> bool {
        self.permission_mode_ceiling
            .is_none_or(|ceiling| mode <= ceiling)
    }
}

/// An OS-managed policy reader. One per platform — macOS managed preferences,
/// Windows registry policy, Linux policy file — selected at boot by
/// [`platform_source`].
pub(crate) trait OsPolicySource: Send + Sync {
    /// The OS-asserted gateway base URL, when the platform declares one.
    ///
    /// `Ok(None)` means the platform asserts no policy. `Err` means a policy
    /// artifact exists but cannot be read or decoded — [`resolve`] projects
    /// that as a misconfigured managed profile, never as unmanaged.
    fn gateway_url(&self) -> Result<Option<String>>;

    /// The OS-asserted permission-mode ceiling, when the platform declares
    /// one. Same error contract as [`Self::gateway_url`]: `Err` means an
    /// artifact exists but its assertion cannot be honored — [`resolve`]
    /// fails that closed by clamping to the default mode rather than
    /// dropping the ceiling.
    fn permission_mode_ceiling(&self) -> Result<Option<PermissionMode>> {
        Ok(None)
    }

    /// The OS-asserted local-MCP allowance, when the platform declares one.
    /// Same error contract as [`Self::gateway_url`]: `Err` means an artifact
    /// exists but its assertion cannot be honored — [`resolve`] fails that
    /// closed to deny rather than honoring a broken opt-in.
    fn allow_local_mcp_servers(&self) -> Result<Option<bool>> {
        Ok(None)
    }
}

/// The source that asserts nothing: non-desktop platforms, embeddings without
/// a policy domain, and directly assembled test state.
pub(crate) struct NoOsPolicy;

impl OsPolicySource for NoOsPolicy {
    fn gateway_url(&self) -> Result<Option<String>> {
        Ok(None)
    }
}

/// Select this platform's OS policy reader.
///
/// Called from the production boot path (`bind_inner`), not from `AppState`
/// construction, so directly assembled state (tests, custom embedders) stays
/// hermetic and reads nothing from the host OS.
#[cfg(target_os = "macos")]
pub(crate) fn platform_source(config: &Config) -> Arc<dyn OsPolicySource> {
    // Managed preferences are keyed by the embedding's bundle id; an
    // embedding without one (the CLI, tests) has no policy domain to read.
    match &config.bundle_id {
        Some(bundle_id) => Arc::new(ManagedPreferencesSource::for_bundle_id(bundle_id)),
        None => Arc::new(NoOsPolicy),
    }
}

#[cfg(windows)]
pub(crate) fn platform_source(_config: &Config) -> Arc<dyn OsPolicySource> {
    Arc::new(RegistryPolicySource)
}

#[cfg(target_os = "linux")]
pub(crate) fn platform_source(_config: &Config) -> Arc<dyn OsPolicySource> {
    Arc::new(PolicyFileSource::at("/etc/openwave/managed-policy.json"))
}

#[cfg(not(any(target_os = "macos", windows, target_os = "linux")))]
pub(crate) fn platform_source(_config: &Config) -> Arc<dyn OsPolicySource> {
    Arc::new(NoOsPolicy)
}

/// Managed (MDM-forced) preferences for the embedding's bundle id.
///
/// `cfprefsd` materializes forced domains as plists under
/// `/Library/Managed Preferences`: the user channel in a per-user directory,
/// the device channel at the root. The reader parses those artifacts directly
/// rather than through `CFPreferences`, which keeps the extraction portable
/// and testable; the user channel is consulted first, matching the
/// framework's search order.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) struct ManagedPreferencesSource {
    /// Candidate plist paths in precedence order.
    paths: Vec<PathBuf>,
    /// The uid a channel plist must be owned by to be honored — root in
    /// production, where MDM materializes the artifacts. Tests point it at
    /// themselves so channel behavior can be exercised with files a test
    /// can actually create.
    trusted_owner: u32,
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl ManagedPreferencesSource {
    pub(crate) fn for_bundle_id(bundle_id: &str) -> Self {
        let root = PathBuf::from("/Library/Managed Preferences");
        let mut paths = Vec::new();
        // The user channel is scoped by the *effective* uid's account name,
        // resolved from the user database — never `$USER`, which any launcher
        // controls and could point at another account's channel or, via path
        // separators, outside the managed-preferences tree entirely.
        if let Some(user) = effective_user_name() {
            if is_safe_path_component(&user) {
                paths.push(root.join(&user).join(format!("{bundle_id}.plist")));
            } else {
                tracing::warn!(
                    "resolved account name {user:?} is not a safe path component; \
                     skipping the user-scoped managed-preferences channel"
                );
            }
        }
        paths.push(root.join(format!("{bundle_id}.plist")));
        Self {
            paths,
            trusted_owner: 0,
        }
    }

    /// Test seam: the production paths are fixed OS locations, so tests
    /// inject their own channel files here. Ownership is still held to the
    /// production requirement (root) unless the test relaxes it.
    #[cfg(test)]
    fn with_paths(paths: Vec<PathBuf>) -> Self {
        Self {
            paths,
            trusted_owner: 0,
        }
    }

    /// Search the channels for one key's value, in precedence order.
    ///
    /// A broken channel falls through instead of aborting the search: an
    /// unreadable user-channel artifact must not hide the device-channel
    /// policy the organization actually deployed. Misconfigured is reported
    /// only when no channel yields a usable value and at least one had a
    /// present-but-broken artifact. Each key searches the channels
    /// independently, matching CFPreferences' per-key domain search.
    fn channel_value<T>(&self, extract: impl Fn(&[u8]) -> Result<Option<T>>) -> Result<Option<T>> {
        let mut broken = None;
        for path in &self.paths {
            let outcome = trusted_plist_bytes(path, self.trusted_owner)
                .and_then(|bytes| bytes.as_deref().map_or(Ok(None), &extract));
            match outcome {
                Ok(Some(value)) => return Ok(Some(value)),
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!("managed-preferences channel skipped: {error}");
                    broken.get_or_insert(error);
                }
            }
        }
        match broken {
            Some(error) => Err(error),
            None => Ok(None),
        }
    }
}

/// The account name of the effective uid, from the user database rather than
/// the environment. `None` when the uid has no passwd entry.
#[cfg(unix)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn effective_user_name() -> Option<String> {
    let mut passwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let mut buffer = vec![0u8; 1024];
    loop {
        let status = unsafe {
            libc::getpwuid_r(
                libc::geteuid(),
                &mut passwd,
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < (1 << 16) {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return None;
        }
        let name = unsafe { std::ffi::CStr::from_ptr(passwd.pw_name) };
        return name
            .to_str()
            .ok()
            .map(str::to_owned)
            .filter(|name| !name.is_empty());
    }
}

#[cfg(not(unix))]
#[allow(dead_code)]
fn effective_user_name() -> Option<String> {
    None
}

/// A resolved account name gets joined into a filesystem path; refuse any
/// shape that could traverse rather than trust the user database blindly.
fn is_safe_path_component(name: &str) -> bool {
    !name.is_empty() && name != "." && !name.contains("..") && !name.contains(['/', '\\', '\0'])
}

impl OsPolicySource for ManagedPreferencesSource {
    // Reading the forced-domain files directly assumes the app stays
    // unsandboxed: the App Sandbox reports paths it can't reach as EPERM
    // rather than ENOENT, which this reader would take for a broken channel.
    // If OpenWave ever adopts the App Sandbox, this must move to the
    // sandbox-safe CFPreferences API.
    fn gateway_url(&self) -> Result<Option<String>> {
        self.channel_value(gateway_url_from_managed_plist)
    }

    fn permission_mode_ceiling(&self) -> Result<Option<PermissionMode>> {
        self.channel_value(permission_mode_from_managed_plist)
    }

    fn allow_local_mcp_servers(&self) -> Result<Option<bool>> {
        self.channel_value(allow_local_mcp_from_managed_plist)
    }
}

/// Read one managed-preferences channel's bytes: an absent file is `None`.
/// Only a file owned by `trusted_owner` (root in production) is honored —
/// MDM materializes these as root, and a plist planted by an unprivileged
/// user must never assert device policy. Ownership is taken from the opened
/// handle, so the check and the read cannot be raced apart.
// Portable so unit tests exercise it on every platform; the production
// caller is the macOS reader.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[cfg_attr(not(unix), allow(unused_variables))]
fn trusted_plist_bytes(path: &Path, trusted_owner: u32) -> Result<Option<Vec<u8>>> {
    use std::io::Read;

    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AgentError::config(format!(
                "managed preferences {} are unreadable: {error}",
                path.display()
            )))
        }
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let owner = file
            .metadata()
            .map_err(|error| {
                AgentError::config(format!(
                    "managed preferences {} are unreadable: {error}",
                    path.display()
                ))
            })?
            .uid();
        if owner != trusted_owner {
            return Err(AgentError::config(format!(
                "managed preferences {} are owned by uid {owner}, not uid {trusted_owner}; refusing to honor them",
                path.display()
            )));
        }
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes).map_err(|error| {
        AgentError::config(format!(
            "managed preferences {} are unreadable: {error}",
            path.display()
        ))
    })?;
    Ok(Some(bytes))
}

/// Extract one string key from a managed-preferences plist (binary or XML).
/// A domain that forces other keys without this one asserts nothing for it
/// in this channel — `None`, not an error. Split from the reader so the
/// format is unit-testable without a profile.
// Live via the reader only where the platform wires it; tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn string_from_managed_plist(bytes: &[u8], key: &str) -> Result<Option<String>> {
    match value_from_managed_plist(bytes, key)? {
        None => Ok(None),
        Some(value) => match value.as_string() {
            Some(raw) => Ok(Some(raw.to_owned())),
            None => Err(AgentError::config(format!(
                "managed preferences {key} is not a string"
            ))),
        },
    }
}

/// Look one key up in a managed-preferences plist (binary or XML).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn value_from_managed_plist(bytes: &[u8], key: &str) -> Result<Option<plist::Value>> {
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|_| AgentError::config("managed preferences plist is unreadable"))?;
    let Some(dictionary) = value.as_dictionary() else {
        return Err(AgentError::config(
            "managed preferences plist is not a dictionary",
        ));
    };
    Ok(dictionary.get(key).cloned())
}

/// Extract and validate `GatewayURL` from a managed-preferences plist.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn gateway_url_from_managed_plist(bytes: &[u8]) -> Result<Option<String>> {
    string_from_managed_plist(bytes, MANAGED_GATEWAY_URL_KEY)?
        .map(|raw| asserted_gateway_url(&raw))
        .transpose()
}

/// Extract and validate `MaximumPermissionMode` from a managed-preferences
/// plist.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn permission_mode_from_managed_plist(bytes: &[u8]) -> Result<Option<PermissionMode>> {
    string_from_managed_plist(bytes, MANAGED_PERMISSION_MODE_KEY)?
        .map(|raw| asserted_permission_mode(&raw))
        .transpose()
}

/// Extract `AllowLocalMcpServers` from a managed-preferences plist: a native
/// plist boolean as profile tooling authors it, or the shared string token
/// for hand-built artifacts.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn allow_local_mcp_from_managed_plist(bytes: &[u8]) -> Result<Option<bool>> {
    match value_from_managed_plist(bytes, MANAGED_ALLOW_LOCAL_MCP_KEY)? {
        None => Ok(None),
        Some(plist::Value::Boolean(flag)) => Ok(Some(flag)),
        Some(value) => match value.as_string() {
            Some(raw) => asserted_policy_flag(raw).map(Some),
            None => Err(AgentError::config(format!(
                "managed preferences {MANAGED_ALLOW_LOCAL_MCP_KEY} is not a boolean"
            ))),
        },
    }
}

/// Machine policy from the Windows registry:
/// `HKLM\Software\Policies\Brightwave\OpenWave`, value `GatewayURL` — the key
/// GPO/Intune administrative templates deploy to. Registry access has no
/// portable seam, so only the value check ([`asserted_gateway_url`]) is
/// unit-tested and this reader stays a thin shell over `winreg`.
#[cfg(windows)]
pub(crate) struct RegistryPolicySource;

/// Read one string value from the machine policy key. An absent key or
/// absent value asserts no policy for that name.
#[cfg(windows)]
fn registry_policy_value(name: &str) -> Result<Option<String>> {
    // KEY_WOW64_64KEY pins the native 64-bit view: policy lives in the
    // real Policies hive, and a future 32-bit build must not be silently
    // redirected to Wow6432Node.
    let key = match winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
        .open_subkey_with_flags(
            r"Software\Policies\Brightwave\OpenWave",
            winreg::enums::KEY_READ | winreg::enums::KEY_WOW64_64KEY,
        ) {
        Ok(key) => key,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AgentError::config(format!(
                "managed policy registry key is unreadable: {error}"
            )))
        }
    };
    match key.get_value::<String, _>(name) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
        Err(error) => Err(AgentError::config(format!(
            "managed policy registry value is unreadable: {error}"
        ))),
    }
}

#[cfg(windows)]
impl OsPolicySource for RegistryPolicySource {
    fn gateway_url(&self) -> Result<Option<String>> {
        registry_policy_value(MANAGED_GATEWAY_URL_KEY)?
            .map(|raw| asserted_gateway_url(&raw))
            .transpose()
    }

    fn permission_mode_ceiling(&self) -> Result<Option<PermissionMode>> {
        registry_policy_value(MANAGED_PERMISSION_MODE_KEY)?
            .map(|raw| asserted_permission_mode(&raw))
            .transpose()
    }

    fn allow_local_mcp_servers(&self) -> Result<Option<bool>> {
        registry_policy_value(MANAGED_ALLOW_LOCAL_MCP_KEY)?
            .map(|raw| asserted_policy_flag(&raw))
            .transpose()
    }
}

/// A JSON policy file: `{"gateway_url": "https://…"}`. Linux wires this at
/// `/etc/openwave/managed-policy.json`; the reader itself is portable so its
/// whole contract is testable on any OS.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub(crate) struct PolicyFileSource {
    path: PathBuf,
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
impl PolicyFileSource {
    pub(crate) fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl PolicyFileSource {
    /// Read and decode the policy file; an absent file asserts nothing.
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    fn read(&self) -> Result<Option<PolicyFilePayload>> {
        let bytes = match std::fs::read(&self.path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AgentError::config(format!(
                    "managed policy file {} is unreadable: {error}",
                    self.path.display()
                )))
            }
        };
        decode_policy_json(&bytes).map(Some)
    }
}

impl OsPolicySource for PolicyFileSource {
    fn gateway_url(&self) -> Result<Option<String>> {
        self.read()?
            .and_then(|file| file.gateway_url)
            .map(|raw| asserted_gateway_url(&raw))
            .transpose()
    }

    fn permission_mode_ceiling(&self) -> Result<Option<PermissionMode>> {
        self.read()?
            .and_then(|file| file.maximum_permission_mode)
            .map(|raw| asserted_permission_mode(&raw))
            .transpose()
    }

    fn allow_local_mcp_servers(&self) -> Result<Option<bool>> {
        Ok(self.read()?.and_then(|file| file.allow_local_mcp_servers))
    }
}

/// The policy-file payload: `{"gateway_url": "https://…",
/// "maximum_permission_mode": "ask", "allow_local_mcp_servers": true}`, each
/// key optional but at least one required.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
#[derive(Deserialize)]
struct PolicyFilePayload {
    #[serde(default)]
    gateway_url: Option<String>,
    #[serde(default)]
    maximum_permission_mode: Option<String>,
    #[serde(default)]
    allow_local_mcp_servers: Option<bool>,
}

/// Decode the policy-file payload. Split from the reader so the format is
/// testable without a filesystem. A present file that names none of the
/// recognized keys is a misconfiguration, not "no policy" — a deployed
/// artifact whose keys are misspelled must fail closed, never read as the
/// open experience.
// Live via the reader only where the platform wires it; tested everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn decode_policy_json(bytes: &[u8]) -> Result<PolicyFilePayload> {
    let file: PolicyFilePayload = serde_json::from_slice(bytes)
        .map_err(|_| AgentError::config("managed policy file is not the expected JSON shape"))?;
    if file.gateway_url.is_none()
        && file.maximum_permission_mode.is_none()
        && file.allow_local_mcp_servers.is_none()
    {
        return Err(AgentError::config(
            "managed policy file names no recognized policy keys",
        ));
    }
    Ok(file)
}

/// The shared shape check for a value read from any OS artifact: registry
/// values and plist strings arrive padded or blank more readily than typed
/// config does, so every reader trims before validation and refuses a blank
/// assertion (present-but-empty is a misconfiguration, not "no policy").
fn asserted_gateway_url(raw: &str) -> Result<String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(AgentError::config(
            "managed policy asserts an empty gateway URL",
        ));
    }
    Ok(value.to_string())
}

/// The shared token check for a permission-mode ceiling read from any OS
/// artifact: trimmed, then held to the chat-mode vocabulary. Anything else —
/// including blank — is a misconfiguration, never silently ignored.
fn asserted_permission_mode(raw: &str) -> Result<PermissionMode> {
    let value = raw.trim();
    PermissionMode::from_str(value).ok_or_else(|| {
        AgentError::config(format!(
            "managed policy asserts an unknown permission mode {value:?}; \
             expected one of plan, ask, auto, allow"
        ))
    })
}

/// The shared token check for a boolean policy value read from an OS artifact
/// that carries strings (the registry's `REG_SZ`, a hand-authored plist):
/// trimmed, then held to `true`/`false`. Anything else — including blank — is
/// a misconfiguration, never silently ignored.
// Live via the readers only where the platform wires it; tested everywhere.
#[cfg_attr(not(any(target_os = "macos", windows)), allow(dead_code))]
fn asserted_policy_flag(raw: &str) -> Result<bool> {
    match raw.trim() {
        "true" => Ok(true),
        "false" => Ok(false),
        value => Err(AgentError::config(format!(
            "managed policy asserts an unknown flag value {value:?}; expected true or false"
        ))),
    }
}

/// The durable provisioned state, stored as one setting.
#[derive(Serialize, Deserialize)]
struct ProvisionedPolicy {
    gateway_url: String,
}

/// Resolve the active policy: OS-managed over provisioned over unmanaged.
///
/// A present-but-invalid policy resolves managed-but-misconfigured
/// ([`ManagedPolicy::misconfigured`]), never silently unmanaged: a profile
/// that claims to be managed must not quietly revert to the open experience
/// on a decode or validation failure, and surfaces need a legible state to
/// render rather than an opaque error on every read. Both authorities pass
/// through [`validated_gateway_url`], so consumers always see one URL shape
/// regardless of which authority asserted it — no platform reader has to
/// remember to validate.
///
/// The OS artifact is read on every call, deliberately: an MDM push or
/// removal becomes visible on the next `/policy` read without an app
/// restart, and the artifacts are tiny.
pub(crate) async fn resolve(
    store: &dyn Store,
    os_policy: &dyn OsPolicySource,
) -> Result<ManagedPolicy> {
    let mut policy = resolve_gateway(store, os_policy).await?;
    // The ceiling is asserted per key, independent of the gateway verdict: an
    // MDM profile can cap the mode without deploying a gateway URL, and the
    // cap rides on whatever policy the gateway resolution produced. A broken
    // ceiling value fails closed to the default mode — `Auto`/`Allow` stay
    // locked out — rather than open or by bricking the profile.
    policy.permission_mode_ceiling = match os_policy.permission_mode_ceiling() {
        Ok(ceiling) => ceiling,
        Err(error) => {
            tracing::warn!(
                "OS-managed permission-mode ceiling is present but unusable: {error}; \
                 clamping to the default mode"
            );
            Some(PermissionMode::Ask)
        }
    };
    // Same per-key shape for the local-MCP allowance, with the opposite
    // failure direction: an opt-in whose artifact is broken fails closed to
    // the deny default rather than granting the allowance.
    policy.allow_local_mcp_servers = match os_policy.allow_local_mcp_servers() {
        Ok(flag) => flag.unwrap_or(false),
        Err(error) => {
            tracing::warn!(
                "OS-managed local-MCP allowance is present but unusable: {error}; \
                 failing closed to deny"
            );
            false
        }
    };
    Ok(policy)
}

/// The gateway half of [`resolve`]: managed verdict, URL, and authority.
async fn resolve_gateway(
    store: &dyn Store,
    os_policy: &dyn OsPolicySource,
) -> Result<ManagedPolicy> {
    match os_policy.gateway_url() {
        Ok(Some(gateway_url)) => return Ok(asserted(ManagedPolicySource::Os, &gateway_url)),
        Err(error) => {
            // The projection stays minimal; this warning is the admin's
            // field diagnostic for what exactly is broken.
            tracing::warn!("OS-managed policy is present but unusable: {error}");
            return Ok(ManagedPolicy::misconfigured(ManagedPolicySource::Os));
        }
        Ok(None) => {}
    }
    if let Some(value) = store.get_setting(SETTING_KEY).await? {
        let Ok(saved) = serde_json::from_value::<ProvisionedPolicy>(value) else {
            tracing::warn!("stored provisioned policy does not decode; resolving misconfigured");
            return Ok(ManagedPolicy::misconfigured(
                ManagedPolicySource::Provisioned,
            ));
        };
        return Ok(asserted(
            ManagedPolicySource::Provisioned,
            &saved.gateway_url,
        ));
    }
    Ok(ManagedPolicy {
        managed: false,
        gateway_url: None,
        source: ManagedPolicySource::Unmanaged,
        misconfigured: false,
        pending_gateway_url: None,
        permission_mode_ceiling: None,
        allow_local_mcp_servers: false,
    })
}

/// Project one authority's asserted URL: valid means managed with the
/// normalized URL, invalid means managed and misconfigured — fail closed.
fn asserted(source: ManagedPolicySource, gateway_url: &str) -> ManagedPolicy {
    match validated_gateway_url(gateway_url) {
        Ok(gateway_url) => ManagedPolicy {
            managed: true,
            gateway_url: Some(gateway_url),
            source,
            misconfigured: false,
            pending_gateway_url: None,
            permission_mode_ceiling: None,
            allow_local_mcp_servers: false,
        },
        Err(error) => {
            tracing::warn!("{source:?}-asserted gateway URL fails the contract: {error}");
            ManagedPolicy::misconfigured(source)
        }
    }
}

/// The one gateway-URL contract for every policy authority: http/https, no
/// embedded credentials, normalized to the parsed form. Shared with the
/// provider write path so a locked base URL compares in the same shape.
pub(crate) fn validated_gateway_url(gateway_url: &str) -> Result<String> {
    Ok(openwave_connectors::GatewayAuthConfig::new(gateway_url)?
        .base_url()
        .to_string())
}

/// Persist sticky provisioned state for `gateway_url`.
///
/// A pairing payload cannot smuggle an invalid or credentialed origin into
/// durable policy (the URL contract), and it cannot silently re-point an
/// already-provisioned profile at a different gateway: re-provisioning the
/// same gateway is idempotent, a conflicting one is refused. Re-pairing is
/// a real product flow, but it runs through [`reprovision`] — which demands
/// the caller name the row it believes it is replacing — never through this
/// write path.
pub(crate) async fn provision(store: &dyn Store, gateway_url: &str) -> Result<()> {
    let gateway_url = validated_gateway_url(gateway_url)?;
    if let Some(existing) = provisioned_url(store).await? {
        if existing == gateway_url {
            return Ok(());
        }
        return Err(AgentError::config(
            "this profile is already provisioned to a different gateway",
        ));
    }
    store
        .set_setting(
            SETTING_KEY,
            &serde_json::to_value(ProvisionedPolicy { gateway_url })?,
        )
        .await
}

/// Replace the provisioned gateway, compare-and-swap style: the write lands
/// only if the row still holds `expected_current` — the URL the user's
/// re-pair confirmation actually named. A row that changed in between (a
/// competing pairing; deletion) refuses rather than overwrites, because the
/// consent on record was given against a different state. Callers serialize
/// the read-check-write under the pairing lock; this check is the belt to
/// that suspender, and the part a unit seam can hold still.
pub(crate) async fn reprovision(
    store: &dyn Store,
    new_url: &str,
    expected_current: &str,
) -> Result<()> {
    let new_url = validated_gateway_url(new_url)?;
    if provisioned_url(store).await?.as_deref() != Some(expected_current) {
        return Err(AgentError::config(
            "the gateway managing this profile changed while re-pairing; nothing was changed",
        ));
    }
    store
        .set_setting(
            SETTING_KEY,
            &serde_json::to_value(ProvisionedPolicy {
                gateway_url: new_url,
            })?,
        )
        .await
}

/// The provisioned gateway URL currently on record, if readable.
///
/// Unreadable stored state reads as `None`, not as an error: [`provision`]
/// treats it as repairable rather than honoring it as a conflict, and the
/// pairing pre-check must agree with that judgment.
pub(crate) async fn provisioned_url(store: &dyn Store) -> Result<Option<String>> {
    Ok(store
        .get_setting(SETTING_KEY)
        .await?
        .and_then(|value| serde_json::from_value::<ProvisionedPolicy>(value).ok())
        .map(|saved| saved.gateway_url))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use openwave_core::DbStore;

    use super::*;

    struct OsAsserted(&'static str);

    impl OsPolicySource for OsAsserted {
        fn gateway_url(&self) -> Result<Option<String>> {
            Ok(Some(self.0.to_string()))
        }
    }

    /// A reader whose policy artifact exists but cannot be decoded.
    struct OsUnreadable;

    impl OsPolicySource for OsUnreadable {
        fn gateway_url(&self) -> Result<Option<String>> {
            Err(AgentError::config("artifact present but unreadable"))
        }
    }

    async fn test_store() -> (Arc<dyn Store>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("policy.db").display()
            ))
            .await
            .unwrap(),
        );
        (store, directory)
    }

    #[tokio::test]
    async fn resolution_prefers_os_policy_over_provisioned_over_open() {
        let (store, _directory) = test_store().await;

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(!policy.managed);
        assert_eq!(policy.source, ManagedPolicySource::Unmanaged);
        assert!(policy.gateway_url.is_none());
        assert!(!policy.misconfigured);

        provision(&*store, "https://gw.example").await.unwrap();
        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(policy.managed);
        assert_eq!(policy.source, ManagedPolicySource::Provisioned);
        assert_eq!(policy.gateway_url.as_deref(), Some("https://gw.example/"));

        // The OS authority passes through the same validation and
        // normalization as the provisioned one: no trailing slash in, one
        // URL shape out.
        let policy = resolve(&*store, &OsAsserted("https://mdm.example"))
            .await
            .unwrap();
        assert_eq!(policy.source, ManagedPolicySource::Os);
        assert_eq!(policy.gateway_url.as_deref(), Some("https://mdm.example/"));
    }

    /// Carried from the #753 review: a broken authority must fail closed as
    /// a legible misconfigured state — still managed, no usable URL, naming
    /// the authority — instead of erroring every `/policy` read. The OS
    /// authority holds precedence even when broken: a valid provisioned URL
    /// underneath must not resurface.
    #[tokio::test]
    async fn a_broken_authority_resolves_misconfigured_never_open() {
        let (store, _directory) = test_store().await;
        provision(&*store, "https://gw.example").await.unwrap();

        for os_policy in [
            &OsAsserted("http://user:pw@mdm.example") as &dyn OsPolicySource,
            &OsUnreadable,
        ] {
            let policy = resolve(&*store, os_policy).await.unwrap();
            assert!(policy.managed && policy.misconfigured);
            assert_eq!(policy.source, ManagedPolicySource::Os);
            assert!(policy.gateway_url.is_none());
        }

        // A degenerate stored provisioned value gets the same projection on
        // its own authority.
        store
            .set_setting(SETTING_KEY, &serde_json::json!({ "gateway_url": "" }))
            .await
            .unwrap();
        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(policy.managed && policy.misconfigured);
        assert_eq!(policy.source, ManagedPolicySource::Provisioned);
        assert!(policy.gateway_url.is_none());
    }

    #[tokio::test]
    async fn provisioning_holds_the_url_to_the_gateway_contract() {
        let (store, _directory) = test_store().await;
        // The contract itself is asserted in the connectors crate; here only
        // that a rejected write leaves the profile unmanaged.
        assert!(provision(&*store, "http://user:pw@gw.example")
            .await
            .is_err());
        assert!(!resolve(&*store, &NoOsPolicy).await.unwrap().managed);
    }

    #[tokio::test]
    async fn a_conflicting_re_provision_is_refused() {
        let (store, _directory) = test_store().await;
        provision(&*store, "https://corp.gateway").await.unwrap();
        // Same gateway (modulo normalization): idempotent.
        provision(&*store, "https://corp.gateway/").await.unwrap();
        // Different gateway: refused, and the original pairing survives.
        let error = provision(&*store, "https://evil.example")
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("already provisioned"));
        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(policy.gateway_url.as_deref(), Some("https://corp.gateway/"));
    }

    /// The Linux reader end to end through resolution: absent file is the
    /// open experience, a valid file is OS-managed, a corrupt file fails
    /// closed as misconfigured. Portable — the path is injected.
    #[tokio::test]
    async fn policy_file_reader_resolves_absent_valid_and_corrupt_files() {
        let (store, _directory) = test_store().await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("managed-policy.json");
        let reader = PolicyFileSource::at(&path);

        assert!(!resolve(&*store, &reader).await.unwrap().managed);

        std::fs::write(&path, br#"{ "gateway_url": "https://corp.gateway" }"#).unwrap();
        let policy = resolve(&*store, &reader).await.unwrap();
        assert!(policy.managed && !policy.misconfigured);
        assert_eq!(policy.source, ManagedPolicySource::Os);
        assert_eq!(policy.gateway_url.as_deref(), Some("https://corp.gateway/"));
        assert_eq!(policy.permission_mode_ceiling, None);

        // The ceiling is per key: a file asserting only the mode cap leaves
        // the gateway side unmanaged, and one asserting both carries both.
        std::fs::write(&path, br#"{ "maximum_permission_mode": "ask" }"#).unwrap();
        let policy = resolve(&*store, &reader).await.unwrap();
        assert!(!policy.managed);
        assert_eq!(policy.permission_mode_ceiling, Some(PermissionMode::Ask));

        std::fs::write(
            &path,
            br#"{ "gateway_url": "https://corp.gateway", "maximum_permission_mode": "auto" }"#,
        )
        .unwrap();
        let policy = resolve(&*store, &reader).await.unwrap();
        assert!(policy.managed && !policy.misconfigured);
        assert_eq!(policy.permission_mode_ceiling, Some(PermissionMode::Auto));
        // The local-MCP allowance defaults to deny; the org asserts it as a
        // native JSON boolean alongside whatever else the file carries.
        assert!(!policy.allow_local_mcp_servers);
        std::fs::write(
            &path,
            br#"{ "gateway_url": "https://corp.gateway", "allow_local_mcp_servers": true }"#,
        )
        .unwrap();
        let policy = resolve(&*store, &reader).await.unwrap();
        assert!(policy.managed && policy.allow_local_mcp_servers);

        for corrupt in [&b"not json"[..], br#"{ "gateway": "wrong shape" }"#] {
            std::fs::write(&path, corrupt).unwrap();
            let policy = resolve(&*store, &reader).await.unwrap();
            assert!(policy.managed && policy.misconfigured);
            assert_eq!(policy.source, ManagedPolicySource::Os);
        }
    }

    /// The macOS extraction over both plist encodings MDM profiles produce,
    /// plus every refusal shape. The key-absent case must be `None` (not an
    /// error) so a domain forcing unrelated keys never reads as broken.
    #[test]
    fn managed_plist_extraction_covers_both_encodings_and_refusals() {
        let xml = |body: &str| {
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>{body}</dict></plist>"#
            )
        };

        let asserted = xml("<key>GatewayURL</key><string> https://corp.gateway </string>");
        assert_eq!(
            gateway_url_from_managed_plist(asserted.as_bytes()).unwrap(),
            // Padding from hand-edited profiles is trimmed before validation.
            Some("https://corp.gateway".to_string())
        );

        let mut binary = Vec::new();
        let mut dictionary = plist::Dictionary::new();
        dictionary.insert(
            "GatewayURL".into(),
            plist::Value::String("https://corp.gateway".into()),
        );
        plist::Value::Dictionary(dictionary)
            .to_writer_binary(std::io::Cursor::new(&mut binary))
            .unwrap();
        assert_eq!(
            gateway_url_from_managed_plist(&binary).unwrap(),
            Some("https://corp.gateway".to_string())
        );

        let unrelated = xml("<key>OtherSetting</key><true/>");
        assert_eq!(
            gateway_url_from_managed_plist(unrelated.as_bytes()).unwrap(),
            None
        );

        let non_string = xml("<key>GatewayURL</key><integer>7</integer>");
        let blank = xml("<key>GatewayURL</key><string>   </string>");
        for broken in [
            b"not a plist".as_slice(),
            non_string.as_bytes(),
            blank.as_bytes(),
        ] {
            assert!(gateway_url_from_managed_plist(broken).is_err());
        }

        // The mode ceiling shares the extraction path: same trimming, absent
        // key is `None`, and a token outside the mode vocabulary refuses.
        let capped = xml("<key>MaximumPermissionMode</key><string> auto </string>");
        assert_eq!(
            permission_mode_from_managed_plist(capped.as_bytes()).unwrap(),
            Some(PermissionMode::Auto)
        );
        assert_eq!(
            permission_mode_from_managed_plist(unrelated.as_bytes()).unwrap(),
            None
        );
        let unknown = xml("<key>MaximumPermissionMode</key><string>yolo</string>");
        assert!(permission_mode_from_managed_plist(unknown.as_bytes()).is_err());

        // The local-MCP allowance reads the native plist boolean profiles
        // author, or the shared string token; absent is `None`, and a token
        // outside true/false refuses.
        let allowed = xml("<key>AllowLocalMcpServers</key><true/>");
        assert_eq!(
            allow_local_mcp_from_managed_plist(allowed.as_bytes()).unwrap(),
            Some(true)
        );
        let token = xml("<key>AllowLocalMcpServers</key><string> true </string>");
        assert_eq!(
            allow_local_mcp_from_managed_plist(token.as_bytes()).unwrap(),
            Some(true)
        );
        assert_eq!(
            allow_local_mcp_from_managed_plist(unrelated.as_bytes()).unwrap(),
            None
        );
        let bad_flag = xml("<key>AllowLocalMcpServers</key><string>yes</string>");
        assert!(allow_local_mcp_from_managed_plist(bad_flag.as_bytes()).is_err());
    }

    /// The ceiling's failure direction: a present-but-broken assertion clamps
    /// to the default mode instead of dropping the ceiling (open) or bricking
    /// the profile, and a valid one rides on the resolved policy whatever the
    /// gateway verdict was.
    #[tokio::test]
    async fn a_broken_ceiling_fails_closed_to_the_default_mode() {
        struct CeilingOnly(Result<Option<PermissionMode>>);

        impl OsPolicySource for CeilingOnly {
            fn gateway_url(&self) -> Result<Option<String>> {
                Ok(None)
            }
            fn permission_mode_ceiling(&self) -> Result<Option<PermissionMode>> {
                match &self.0 {
                    Ok(value) => Ok(*value),
                    Err(error) => Err(AgentError::config(error.to_string())),
                }
            }
        }

        let (store, _directory) = test_store().await;
        let policy = resolve(&*store, &CeilingOnly(Ok(Some(PermissionMode::Ask))))
            .await
            .unwrap();
        assert!(!policy.managed);
        assert_eq!(policy.permission_mode_ceiling, Some(PermissionMode::Ask));

        let policy = resolve(
            &*store,
            &CeilingOnly(Err(AgentError::config("artifact present but unreadable"))),
        )
        .await
        .unwrap();
        assert_eq!(policy.permission_mode_ceiling, Some(PermissionMode::Ask));

        // The clamp the gate applies: over-ceiling comes down (including the
        // unset default when the ceiling sits below it), at-or-below stays
        // the reader's choice.
        assert_eq!(
            policy.clamp_permission_mode(Some(PermissionMode::Allow)),
            Some(PermissionMode::Ask)
        );
        assert_eq!(
            policy.clamp_permission_mode(Some(PermissionMode::Plan)),
            Some(PermissionMode::Plan)
        );
        assert_eq!(policy.clamp_permission_mode(None), None);
        let plan_capped = ManagedPolicy {
            permission_mode_ceiling: Some(PermissionMode::Plan),
            ..policy
        };
        assert_eq!(
            plan_capped.clamp_permission_mode(None),
            Some(PermissionMode::Plan)
        );
    }

    /// The channel-fallthrough decision, end to end through resolution: a
    /// broken user-channel artifact must not hide the device-channel policy
    /// the organization actually deployed, and misconfigured is reported
    /// only when no channel yields a usable value. A refactor restoring
    /// abort-on-first-error fails here.
    #[tokio::test]
    async fn a_broken_user_channel_falls_through_to_the_device_channel() {
        let (store, _directory) = test_store().await;
        let directory = tempfile::tempdir().unwrap();
        let user_path = directory.path().join("user").join("app.plist");
        let device_path = directory.path().join("app.plist");
        std::fs::create_dir_all(user_path.parent().unwrap()).unwrap();
        std::fs::write(&user_path, b"not a plist").unwrap();
        std::fs::write(
            &device_path,
            br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>GatewayURL</key><string>https://corp.gateway</string></dict></plist>"#,
        )
        .unwrap();

        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut reader = ManagedPreferencesSource::with_paths(vec![user_path, device_path.clone()]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            // Trust files owned like the ones this test just wrote.
            reader.trusted_owner = std::fs::metadata(&device_path).unwrap().uid();
        }

        let policy = resolve(&*store, &reader).await.unwrap();
        assert!(policy.managed && !policy.misconfigured);
        assert_eq!(policy.source, ManagedPolicySource::Os);
        assert_eq!(policy.gateway_url.as_deref(), Some("https://corp.gateway/"));

        // With no device channel behind it, the broken user channel is what
        // the reader has to say: misconfigured, never silently unmanaged.
        std::fs::remove_file(&device_path).unwrap();
        let policy = resolve(&*store, &reader).await.unwrap();
        assert!(policy.managed && policy.misconfigured);
        assert!(policy.gateway_url.is_none());
    }

    /// The ownership refusal, in the direction testable without root: a
    /// channel plist owned by the (non-root) test user must be refused
    /// rather than honored as device policy.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_channel_plist_not_owned_by_root_is_refused() {
        use std::os::unix::fs::MetadataExt;

        let (store, _directory) = test_store().await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("app.plist");
        std::fs::write(
            &path,
            br#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict><key>GatewayURL</key><string>https://planted.example</string></dict></plist>"#,
        )
        .unwrap();
        if std::fs::metadata(&path).unwrap().uid() == 0 {
            // Running as root: the file is genuinely root-owned and the
            // refusal cannot be observed.
            return;
        }

        let reader = ManagedPreferencesSource::with_paths(vec![path]);
        let policy = resolve(&*store, &reader).await.unwrap();
        assert!(policy.managed && policy.misconfigured);
        assert_eq!(policy.source, ManagedPolicySource::Os);
        assert!(policy.gateway_url.is_none());
    }

    /// The guard between the user database and the filesystem join: no
    /// resolved account name may escape the managed-preferences tree.
    #[test]
    fn a_resolved_account_name_never_traverses_the_preferences_tree() {
        assert!(is_safe_path_component("abaas"));
        assert!(is_safe_path_component("svc-mdm.local"));
        for hostile in [
            "",
            ".",
            "..",
            "../../tmp/evil",
            "a/b",
            "a\\b",
            "x..y",
            "nul\0byte",
        ] {
            assert!(!is_safe_path_component(hostile), "accepted {hostile:?}");
        }
    }

    /// The value check shared by the Windows registry reader (whose registry
    /// access itself has no portable seam): padded values are trimmed, blank
    /// ones are a misconfiguration rather than "no policy".
    #[test]
    fn an_asserted_value_is_trimmed_and_a_blank_one_refused() {
        assert_eq!(
            asserted_gateway_url(" https://corp.gateway \r\n").unwrap(),
            "https://corp.gateway"
        );
        assert!(asserted_gateway_url("   ").is_err());
    }
}
