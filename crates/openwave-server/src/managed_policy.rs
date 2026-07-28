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

use openwave_core::{AgentError, Result, Store};
use serde::{Deserialize, Serialize};

const SETTING_KEY: &str = "managed_policy_v1";

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
}

/// An OS-managed policy reader. Platform implementations (macOS managed
/// preferences, Windows registry policy, Linux `/etc/openwave/`) arrive in a
/// follow-up slice; the seam exists now so the precedence order is fixed and
/// testable before any reader ships.
pub(crate) trait OsPolicySource: Send + Sync {
    /// The OS-asserted gateway base URL, when the platform declares one.
    fn gateway_url(&self) -> Option<String>;
}

/// The default source on every platform until a reader ships.
pub(crate) struct NoOsPolicy;

impl OsPolicySource for NoOsPolicy {
    fn gateway_url(&self) -> Option<String> {
        None
    }
}

/// The durable provisioned state, stored as one setting.
#[derive(Serialize, Deserialize)]
struct ProvisionedPolicy {
    gateway_url: String,
}

/// Resolve the active policy: OS-managed over provisioned over unmanaged.
///
/// A present-but-invalid policy is an error, not silently unmanaged: a
/// profile that claims to be managed must never quietly revert to the open
/// experience on a decode or validation failure. Both authorities pass
/// through [`validated_gateway_url`], so consumers always see one URL shape
/// regardless of which authority asserted it — no platform reader has to
/// remember to validate.
pub(crate) async fn resolve(
    store: &dyn Store,
    os_policy: &dyn OsPolicySource,
) -> Result<ManagedPolicy> {
    if let Some(gateway_url) = os_policy.gateway_url() {
        return Ok(ManagedPolicy {
            managed: true,
            gateway_url: Some(validated_gateway_url(&gateway_url)?),
            source: ManagedPolicySource::Os,
        });
    }
    if let Some(value) = store.get_setting(SETTING_KEY).await? {
        let saved: ProvisionedPolicy = serde_json::from_value(value)
            .map_err(|_| AgentError::config("saved managed policy is unreadable"))?;
        return Ok(ManagedPolicy {
            managed: true,
            gateway_url: Some(validated_gateway_url(&saved.gateway_url)?),
            source: ManagedPolicySource::Provisioned,
        });
    }
    Ok(ManagedPolicy {
        managed: false,
        gateway_url: None,
        source: ManagedPolicySource::Unmanaged,
    })
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
        fn gateway_url(&self) -> Option<String> {
            Some(self.0.to_string())
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
        assert!(resolve(&*store, &OsAsserted("http://user:pw@mdm.example"))
            .await
            .is_err());
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

    #[tokio::test]
    async fn a_degenerate_stored_value_never_resolves_managed() {
        let (store, _directory) = test_store().await;
        store
            .set_setting(SETTING_KEY, &serde_json::json!({ "gateway_url": "" }))
            .await
            .unwrap();
        assert!(resolve(&*store, &NoOsPolicy).await.is_err());
    }
}
