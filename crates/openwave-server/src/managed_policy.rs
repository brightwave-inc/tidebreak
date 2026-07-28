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
use std::path::PathBuf;
use std::sync::Arc;

use openwave_core::{AgentError, Config, Result, Store};
use serde::{Deserialize, Serialize};

const SETTING_KEY: &str = "managed_policy_v1";

/// The key every OS artifact stores the asserted URL under: the Windows
/// registry value and the macOS managed-preferences key share this name.
const MANAGED_GATEWAY_URL_KEY: &str = "GatewayURL";

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
        }
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
}

#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
impl ManagedPreferencesSource {
    pub(crate) fn for_bundle_id(bundle_id: &str) -> Self {
        let root = PathBuf::from("/Library/Managed Preferences");
        let mut paths = Vec::new();
        if let Some(user) = std::env::var_os("USER").filter(|user| !user.is_empty()) {
            paths.push(root.join(user).join(format!("{bundle_id}.plist")));
        }
        paths.push(root.join(format!("{bundle_id}.plist")));
        Self { paths }
    }
}

impl OsPolicySource for ManagedPreferencesSource {
    fn gateway_url(&self) -> Result<Option<String>> {
        for path in &self.paths {
            let bytes = match std::fs::read(path) {
                Ok(bytes) => bytes,
                Err(error) if error.kind() == ErrorKind::NotFound => continue,
                Err(error) => {
                    return Err(AgentError::config(format!(
                        "managed preferences {} are unreadable: {error}",
                        path.display()
                    )))
                }
            };
            // A domain that forces other keys without GatewayURL asserts no
            // gateway here; keep looking in the next channel.
            if let Some(url) = gateway_url_from_managed_plist(&bytes)? {
                return Ok(Some(url));
            }
        }
        Ok(None)
    }
}

/// Extract `GatewayURL` from a managed-preferences plist (binary or XML).
/// Split from the reader so the format is unit-testable without a profile.
// Live via the reader only where the platform wires it; tested everywhere.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn gateway_url_from_managed_plist(bytes: &[u8]) -> Result<Option<String>> {
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes))
        .map_err(|_| AgentError::config("managed preferences plist is unreadable"))?;
    let Some(dictionary) = value.as_dictionary() else {
        return Err(AgentError::config(
            "managed preferences plist is not a dictionary",
        ));
    };
    match dictionary.get(MANAGED_GATEWAY_URL_KEY) {
        None => Ok(None),
        Some(value) => match value.as_string() {
            Some(url) => asserted_gateway_url(url).map(Some),
            None => Err(AgentError::config(
                "managed preferences GatewayURL is not a string",
            )),
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

#[cfg(windows)]
impl OsPolicySource for RegistryPolicySource {
    fn gateway_url(&self) -> Result<Option<String>> {
        let key = match winreg::RegKey::predef(winreg::enums::HKEY_LOCAL_MACHINE)
            .open_subkey(r"Software\Policies\Brightwave\OpenWave")
        {
            Ok(key) => key,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(AgentError::config(format!(
                    "managed policy registry key is unreadable: {error}"
                )))
            }
        };
        match key.get_value::<String, _>(MANAGED_GATEWAY_URL_KEY) {
            Ok(value) => asserted_gateway_url(&value).map(Some),
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(None),
            Err(error) => Err(AgentError::config(format!(
                "managed policy registry value is unreadable: {error}"
            ))),
        }
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

impl OsPolicySource for PolicyFileSource {
    fn gateway_url(&self) -> Result<Option<String>> {
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
        gateway_url_from_policy_json(&bytes).map(Some)
    }
}

/// Decode the policy-file payload. Split from the reader so the format is
/// testable without a filesystem.
// Live via the reader only where the platform wires it; tested everywhere.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn gateway_url_from_policy_json(bytes: &[u8]) -> Result<String> {
    #[derive(Deserialize)]
    struct PolicyFile {
        gateway_url: String,
    }
    let file: PolicyFile = serde_json::from_slice(bytes)
        .map_err(|_| AgentError::config("managed policy file is not the expected JSON shape"))?;
    asserted_gateway_url(&file.gateway_url)
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
pub(crate) async fn resolve(
    store: &dyn Store,
    os_policy: &dyn OsPolicySource,
) -> Result<ManagedPolicy> {
    match os_policy.gateway_url() {
        Ok(Some(gateway_url)) => return Ok(asserted(ManagedPolicySource::Os, &gateway_url)),
        Err(_) => return Ok(ManagedPolicy::misconfigured(ManagedPolicySource::Os)),
        Ok(None) => {}
    }
    if let Some(value) = store.get_setting(SETTING_KEY).await? {
        let Ok(saved) = serde_json::from_value::<ProvisionedPolicy>(value) else {
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
        },
        Err(_) => ManagedPolicy::misconfigured(source),
    }
}

/// The one gateway-URL contract for every policy authority: http/https, no
/// embedded credentials, normalized to the parsed form.
fn validated_gateway_url(gateway_url: &str) -> Result<String> {
    Ok(openwave_connectors::GatewayAuthConfig::new(gateway_url)?
        .base_url()
        .to_string())
}

/// Persist sticky provisioned state for `gateway_url`.
///
/// A pairing payload cannot smuggle an invalid or credentialed origin into
/// durable policy (the URL contract), and it cannot silently re-point an
/// already-provisioned profile at a different gateway: re-provisioning the
/// same gateway is idempotent, a conflicting one is refused. If re-pairing
/// ever becomes a product flow, it belongs behind an explicit user
/// confirmation in the deep-link slice, not in this write path.
// The deep-link pairing flow is the intended production caller; until that
// slice lands, only tests exercise this write path.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) async fn provision(store: &dyn Store, gateway_url: &str) -> Result<()> {
    let gateway_url = validated_gateway_url(gateway_url)?;
    if let Some(value) = store.get_setting(SETTING_KEY).await? {
        // Unreadable existing state is not honored as a conflict: this write
        // path is its only repair.
        if let Ok(existing) = serde_json::from_value::<ProvisionedPolicy>(value) {
            if existing.gateway_url == gateway_url {
                return Ok(());
            }
            return Err(AgentError::config(
                "this profile is already provisioned to a different gateway",
            ));
        }
    }
    store
        .set_setting(
            SETTING_KEY,
            &serde_json::to_value(ProvisionedPolicy { gateway_url })?,
        )
        .await
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
