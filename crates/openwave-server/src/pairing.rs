//! Deep-link pairing: park a gateway, and provision only when a sign-in
//! consents to it.
//!
//! The desktop shell's `openwave://provision` handler lands here. A valid
//! link is *registered*, not honored: the gateway URL is parked
//! process-ephemerally and the sign-in gate presents it, and only a
//! completed browser sign-in against that gateway — the user's consent —
//! runs the managed-policy write path, from the sign-in exchange task.
//! Policy is the only gateway source — there is no provider row to point —
//! so provisioning is the whole write. Registration is exported for native
//! embedders only — pairing changes policy, and policy must never be
//! reachable from a renderer-writable route; the renderer's influence is
//! limited to completing or dismissing the sign-in it can already perform.

use std::sync::Arc;

use crate::connectors::GatewayAuthConfig;
use openwave_core::{AgentError, Result, SecretProvider, Store};
use tokio::sync::Mutex;

use crate::managed_policy;
use crate::mcp_config::McpRuntime;

/// Serializes pairing end to end. [`managed_policy::provision`] is a
/// check-then-write over the settings store, and the [`Store`] API offers no
/// cross-call transaction to make it atomic; two links decoded concurrently
/// could otherwise both pass the conflict check and the second would
/// overwrite the first. One desktop process owns the store (the server's
/// instance lock guarantees it), so a process-local mutex is enough to make
/// the check-then-write effectively atomic.
static PAIRING: Mutex<()> = Mutex::const_new(());

/// The process-local handles pairing needs.
///
/// Pairing writes policy, and policy has live effects — the manual MCP
/// servers this process may already be running are locked the moment it
/// commits. The shell's deep-link task therefore carries this rather than a
/// bare store, so it cannot pair without also being able to apply what
/// pairing decided. Obtained from
/// [`Server::pairing_handle`](crate::Server::pairing_handle).
#[derive(Clone)]
pub struct PairingHandle {
    store: Arc<dyn Store>,
    mcp: Arc<McpRuntime>,
    /// The one process-wide gateway runtime: the pending pairing parks there
    /// so the sign-in surface and the `/policy` projection see the same slot
    /// the shell registered into.
    gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
}

impl PairingHandle {
    pub(crate) fn new(
        store: Arc<dyn Store>,
        mcp: Arc<McpRuntime>,
        gateway: Arc<crate::gateway_runtime::GatewayRuntime>,
    ) -> Self {
        Self {
            store,
            mcp,
            gateway,
        }
    }

    /// The durable store this profile pairs against.
    pub fn store(&self) -> Arc<dyn Store> {
        self.store.clone()
    }
}

/// Why a pairing did not provision this profile.
///
/// The conflict is its own variant because it is a product refusal, not a
/// fault: the shell owes the user an explanation naming the gateway that
/// actually manages this device rather than a log line. What the shell may
/// offer next depends on the conflicting authority: the provisioned row is
/// user-consented state, so the shell can escalate to an explicit re-pair
/// confirmation ([`register_replacing_pairing`]); an OS (MDM) assertion is
/// refuse-forever — gateway migration at that tier is the MDM's job.
/// Every other failure — invalid URL, unreachable gateway, bad manifest,
/// store errors — stays the [`AgentError`] it was.
#[derive(Debug)]
#[non_exhaustive]
pub enum PairingError {
    /// The link named a different gateway than the one this profile is
    /// provisioned to. Carries the provisioned base URL — normalized and
    /// secret-free by the gateway-URL contract — so callers can name it;
    /// user-facing surfaces should reduce it to the origin.
    Conflict {
        /// The normalized base URL this profile is provisioned to.
        provisioned_url: String,
        /// Whether the shell may offer re-pairing: true when the conflicting
        /// authority is the user-consented provisioned row, false when the
        /// OS (MDM) asserts the gateway and no local flow may replace it.
        replaceable: bool,
    },
    /// Any other failure. No policy was written and nothing is pending.
    Other(AgentError),
}

impl std::fmt::Display for PairingError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { .. } => write!(
                f,
                "this profile is already provisioned to a different gateway"
            ),
            Self::Other(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for PairingError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Conflict { .. } => None,
            Self::Other(error) => Some(error),
        }
    }
}

impl From<AgentError> for PairingError {
    fn from(error: AgentError) -> Self {
        Self::Other(error)
    }
}

/// What registering a provision link decided.
#[derive(Debug, PartialEq, Eq)]
pub enum PendingRegistration {
    /// The pairing is parked and awaiting the sign-in that is its consent;
    /// the sign-in gate presents it on its next policy poll.
    Registered,
    /// This gateway already manages the profile — nothing to do. The gate
    /// already presents itself if no session exists.
    AlreadyManaged,
}

/// Validate a provision link's gateway URL and park it until a sign-in
/// consents to it.
///
/// Nothing durable is written here, and the gateway is not probed: a deep
/// link is an unauthenticated remote trigger, so before the user acts it
/// must neither change policy nor cause this process to issue requests to
/// an attacker-chosen URL. The commit happens in the sign-in exchange task
/// ([`commit_signed_in_pairing`]) only after the user completes a browser
/// sign-in against this gateway. A link for a gateway other than the one
/// that manages this profile is refused as the typed
/// [`PairingError::Conflict`] — the shell surfaces that refusal, naming the
/// managing gateway.
pub async fn register_pending_pairing(
    handle: &PairingHandle,
    gateway_url: &str,
) -> Result<PendingRegistration, PairingError> {
    let config = GatewayAuthConfig::new(gateway_url)?;
    let base_url = config.base_url().to_string();
    let _guard = PAIRING.lock().await;
    let policy = handle.gateway.policy().await?;
    if policy.managed {
        let replaceable = policy.source == crate::managed_policy::ManagedPolicySource::Provisioned;
        return match policy.gateway_url {
            Some(existing) if existing == base_url => Ok(PendingRegistration::AlreadyManaged),
            Some(existing) => Err(PairingError::Conflict {
                provisioned_url: existing,
                replaceable,
            }),
            None => Err(PairingError::Other(AgentError::config(
                "this device's managed policy is misconfigured; contact your administrator",
            ))),
        };
    }
    handle
        .gateway
        .register_pending_pairing(base_url, handle.mcp.clone(), None)
        .await;
    Ok(PendingRegistration::Registered)
}

/// Park a re-pairing the user explicitly confirmed: once a sign-in consents,
/// replace the provisioned gateway `expected_current` with `gateway_url`.
///
/// The shell calls this only after a native confirmation naming both origins
/// — it is the escalation a replaceable [`PairingError::Conflict`] invites,
/// never a first move. Like plain registration, nothing durable is written
/// here and the gateway is not probed: the parked pairing carries the URL
/// the confirmation named, and the commit re-checks it under the pairing
/// lock, so a row that changed in between refuses rather than overwrites. A
/// state that moved since the confirmation — the provisioned URL is no
/// longer `expected_current`, or an OS authority claimed the profile — is
/// refused here the same way, so the shell reports a raced failure instead
/// of parking a pairing whose consent no longer describes the device.
pub async fn register_replacing_pairing(
    handle: &PairingHandle,
    gateway_url: &str,
    expected_current: &str,
) -> Result<PendingRegistration, PairingError> {
    let config = GatewayAuthConfig::new(gateway_url)?;
    let base_url = config.base_url().to_string();
    let _guard = PAIRING.lock().await;
    let policy = handle.gateway.policy().await?;
    if policy.managed {
        let replaceable = policy.source == crate::managed_policy::ManagedPolicySource::Provisioned;
        match policy.gateway_url {
            Some(existing) if existing == base_url => {
                return Ok(PendingRegistration::AlreadyManaged)
            }
            Some(existing) if !replaceable || existing != expected_current => {
                return Err(PairingError::Conflict {
                    provisioned_url: existing,
                    replaceable,
                })
            }
            Some(_) => {}
            None => {
                return Err(PairingError::Other(AgentError::config(
                    "this device's managed policy is misconfigured; contact your administrator",
                )))
            }
        }
        handle
            .gateway
            .register_pending_pairing(
                base_url,
                handle.mcp.clone(),
                Some(expected_current.to_string()),
            )
            .await;
        return Ok(PendingRegistration::Registered);
    }
    // The row vanished between the conflict and the confirmation (a profile
    // reset). What the user confirmed — pair with this gateway — still
    // describes the device; it just no longer replaces anything.
    handle
        .gateway
        .register_pending_pairing(base_url, handle.mcp.clone(), None)
        .await;
    Ok(PendingRegistration::Registered)
}

/// What a deprovision link would act on, for the shell's dialog.
///
/// A probe, not a lock: the shell reads this to decide which dialog to show,
/// and [`deprovision_provisioned_gateway`] re-checks everything under the
/// pairing lock before deleting anything.
#[derive(Debug, PartialEq, Eq)]
pub enum DeprovisionTarget {
    /// The sticky provisioned row: user-consented state the shell may
    /// confirm-and-disconnect. Carries the normalized base URL the
    /// confirmation must name — and the CAS anchors to.
    Provisioned {
        /// The normalized base URL this profile is provisioned to.
        gateway_url: String,
    },
    /// The OS (MDM) asserts the gateway: never locally removable.
    OsManaged {
        /// The asserted base URL, when the artifact is readable.
        gateway_url: Option<String>,
    },
    /// Nothing to disconnect: the open, bring-your-own-key experience.
    Unprovisioned,
    /// An authority asserts management but its state cannot be honored —
    /// there is no URL a confirmation could name, so the shell surfaces a
    /// repair message instead of a disconnect.
    Misconfigured {
        /// Whether the broken authority is the OS (MDM) tier.
        source_is_os: bool,
    },
}

/// Resolve what an `openwave://deprovision` link would act on.
pub async fn deprovision_target(handle: &PairingHandle) -> Result<DeprovisionTarget> {
    let policy = handle.gateway.policy().await?;
    let source_is_os = policy.source == crate::managed_policy::ManagedPolicySource::Os;
    Ok(if !policy.managed {
        DeprovisionTarget::Unprovisioned
    } else if source_is_os {
        DeprovisionTarget::OsManaged {
            gateway_url: policy.gateway_url,
        }
    } else {
        match policy.gateway_url {
            Some(gateway_url) => DeprovisionTarget::Provisioned { gateway_url },
            None => DeprovisionTarget::Misconfigured { source_is_os },
        }
    })
}

/// Delete the provisioned row the user's native confirmation named, and
/// retire everything that stood on it: the pending pairing, any in-flight
/// sign-in, and the stored gateway session (best-effort revoke, unconditional
/// local clear).
///
/// The consent ceremony mirrors provisioning's: as the browser sign-in is
/// the consent to provision, the shell's native confirmation — which named
/// `expected_current` — is the consent to deprovision, and the delete is
/// compare-and-swap on exactly that URL under the same pairing lock as every
/// other policy write. An OS (MDM) assertion refuses as the non-replaceable
/// [`PairingError::Conflict`]; a row that moved since the confirmation
/// refuses as the replaceable one, naming what the row now holds; a profile
/// already unmanaged is a raced success, not an error.
pub async fn deprovision_provisioned_gateway(
    handle: &PairingHandle,
    expected_current: &str,
) -> Result<(), PairingError> {
    let _guard = PAIRING.lock().await;
    let policy = handle.gateway.policy().await?;
    if policy.source == crate::managed_policy::ManagedPolicySource::Os {
        return Err(PairingError::Conflict {
            provisioned_url: policy.gateway_url.unwrap_or_default(),
            replaceable: false,
        });
    }
    if !policy.managed {
        // The row vanished between the confirmation and this call (a
        // profile reset): what the user asked for is already true.
        return Ok(());
    }
    match policy.gateway_url {
        Some(existing) if existing != expected_current => {
            return Err(PairingError::Conflict {
                provisioned_url: existing,
                replaceable: true,
            })
        }
        Some(_) => {}
        None => {
            return Err(PairingError::Other(AgentError::config(
                "this device's managed policy is misconfigured; contact your administrator",
            )))
        }
    }
    managed_policy::deprovision(&*handle.store, expected_current).await?;
    handle.gateway.abandon_sign_in_and_pairing().await;
    // Retire against the newly-open policy. A revoke that fails must not
    // resurrect the disconnect — the local clear inside is unconditional and
    // the server-side session dies at refresh-token expiry — so only a store
    // read could error here, and that is worth surfacing.
    handle.gateway.retire_session_for_current_policy().await?;
    Ok(())
}

/// Provision the profile a finished sign-in consented to.
///
/// Called from the sign-in exchange task with a session already minted for
/// `base_url`. Policy is re-resolved under the pairing lock: an authority
/// that claimed the profile while the browser flow ran (an MDM push) wins,
/// and the pairing is refused rather than written under it. A *replacing*
/// pairing carries the provisioned URL its confirmation named in `replaces`,
/// and may write over exactly that row — the compare-and-swap in
/// [`managed_policy::reprovision`] — so a row that moved to anything else
/// mid-flow still wins and the pairing is refused. Provisioning is the only
/// durable policy write — the model snapshot is stamped with the deployment
/// it was synced from, so one left behind by any earlier configuration is
/// simply never honored for this gateway.
///
/// The new policy is applied to this process before returning: manual MCP
/// servers running under the previous profile are taken down here rather
/// than at the supervisor's next sweep, and a stored session the new policy
/// no longer stands behind — the replaced gateway's — is retired
/// (best-effort revoke, unconditional local clear) before the exchange task
/// stores the new one, so its refresh token does not stay live at a gateway
/// this profile no longer answers to.
pub(crate) async fn commit_signed_in_pairing(
    store: &dyn Store,
    os_policy: &dyn crate::managed_policy::OsPolicySource,
    secrets: Arc<dyn SecretProvider>,
    mcp: &McpRuntime,
    base_url: &str,
    replaces: Option<&str>,
) -> openwave_core::Result<()> {
    // PAIRING makes the policy re-read and the write atomic against another
    // pairing path; no gateway-state lock, for the same reason the old
    // probe-then-provision path took none — the snapshot stamp is the guard.
    let _guard = PAIRING.lock().await;
    let policy = managed_policy::resolve(store, os_policy).await?;
    if policy.managed && policy.gateway_url.as_deref() != Some(base_url) {
        let replacing = policy.source == crate::managed_policy::ManagedPolicySource::Provisioned
            && replaces.is_some()
            && policy.gateway_url.as_deref() == replaces;
        if !replacing {
            let authority = policy
                .gateway_url
                .unwrap_or_else(|| "another authority".to_string());
            return Err(AgentError::config(format!(
                "this device became managed by {authority} during sign-in; the pairing was not applied"
            )));
        }
        managed_policy::reprovision(store, base_url, replaces.expect("checked above")).await?;
    } else {
        managed_policy::provision(store, base_url).await?;
    }
    let policy = managed_policy::resolve(store, os_policy).await?;
    crate::gateway_runtime::retire_superseded_gateway_session(secrets, &policy).await?;
    mcp.enforce_manual_lockdown().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use std::collections::HashMap;

    use openwave_core::{DbStore, SecretProvider};
    use serde_json::json;

    use openwave_core::ToolRegistry;

    use super::*;
    use crate::managed_policy::{resolve, NoOsPolicy};
    use crate::mcp_config::{
        GatewayEndpointAccess, GatewayEndpoints, McpHealth, McpServerDefinition, McpServersConfig,
        MANAGED_DISABLED_DIAGNOSTIC,
    };
    use crate::providers;

    /// The signed-out stand-in: pairing never resolves an endpoint.
    struct NoGateway;

    #[async_trait::async_trait]
    impl GatewayEndpoints for NoGateway {
        async fn endpoint(&self, _slug: &str) -> Result<GatewayEndpointAccess> {
            Err(AgentError::config("no gateway session in tests"))
        }
    }

    #[derive(Default)]
    struct TestSecrets(std::sync::Mutex<HashMap<String, String>>);

    #[async_trait::async_trait]
    impl SecretProvider for TestSecrets {
        async fn get_secret(&self, key: &str) -> Result<Option<String>> {
            Ok(self.0.lock().unwrap().get(key).cloned())
        }
        async fn set_secret(&self, key: &str, value: &str) -> Result<()> {
            self.0.lock().unwrap().insert(key.into(), value.into());
            Ok(())
        }
        async fn delete_secret(&self, key: &str) -> Result<()> {
            self.0.lock().unwrap().remove(key);
            Ok(())
        }
    }

    fn test_secrets() -> Arc<dyn SecretProvider> {
        Arc::new(TestSecrets::default())
    }

    /// The handle plus the runtimes behind it, for the assertions that are
    /// about what pairing *did* to this process rather than what it wrote.
    fn test_handle_with_runtimes(
        store: &Arc<dyn Store>,
    ) -> (
        PairingHandle,
        Arc<McpRuntime>,
        Arc<crate::gateway_runtime::GatewayRuntime>,
    ) {
        let mcp = Arc::new(McpRuntime::new(
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(TestSecrets::default()),
            Arc::new(NoGateway),
            Arc::new(NoOsPolicy),
        ));
        let gateway = crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            Arc::new(TestSecrets::default()),
            Arc::new(NoOsPolicy),
        );
        (
            PairingHandle::new(store.clone(), mcp.clone(), gateway.clone()),
            mcp,
            gateway,
        )
    }

    /// A minimal Streamable HTTP MCP server, standing in for a manual server
    /// the profile was already running when the provision link arrived.
    async fn serve_manual_mcp() -> String {
        async fn handler(body: String) -> ([(&'static str, &'static str); 1], String) {
            let request: serde_json::Value = serde_json::from_str(&body).unwrap();
            let id = request.get("id").cloned().unwrap_or_default();
            let result = match request["method"].as_str().unwrap_or_default() {
                "initialize" => json!({
                    "protocolVersion": openwave_mcp::PROTOCOL_VERSION,
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "pairing-fixture", "version": "1"}
                }),
                "tools/list" => json!({
                    "tools": [{
                        "name": "lookup",
                        "description": "Look something up",
                        "inputSchema": {"type": "object"}
                    }]
                }),
                _ => json!({}),
            };
            (
                [("content-type", "application/json")],
                json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
            )
        }

        let app = axum::Router::new().route("/mcp", axum::routing::post(handler));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/mcp")
    }

    async fn test_store() -> (Arc<dyn Store>, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let store: Arc<dyn Store> = Arc::new(
            DbStore::connect(&format!(
                "sqlite://{}?mode=rwc",
                directory.path().join("pairing.db").display()
            ))
            .await
            .unwrap(),
        );
        (store, directory)
    }

    /// The write-path guard the retired confirmation dialog used to be:
    /// registering a link parks it and changes nothing durable — no policy
    /// write, and no request to the link's URL (the bases here resolve to
    /// nothing servable). The latest link wins the slot, and a dismissal
    /// empties it.
    #[tokio::test]
    async fn a_registered_pairing_writes_nothing_until_sign_in() {
        let (store, _directory) = test_store().await;
        let (handle, _mcp, gateway) = test_handle_with_runtimes(&store);

        let outcome = register_pending_pairing(&handle, "https://gw.invalid")
            .await
            .unwrap();
        assert_eq!(outcome, PendingRegistration::Registered);
        assert!(!resolve(&*store, &NoOsPolicy).await.unwrap().managed);
        assert_eq!(
            gateway.pending_pairing_url().await.as_deref(),
            Some("https://gw.invalid/"),
            "the parked URL is the normalized one the commit will write"
        );

        let outcome = register_pending_pairing(&handle, "https://other.invalid")
            .await
            .unwrap();
        assert_eq!(outcome, PendingRegistration::Registered);
        assert_eq!(
            gateway.pending_pairing_url().await.as_deref(),
            Some("https://other.invalid/")
        );

        gateway.dismiss_pending_pairing().await;
        assert_eq!(gateway.pending_pairing_url().await, None);
        assert!(!resolve(&*store, &NoOsPolicy).await.unwrap().managed);
    }

    /// A link for a gateway other than the managing one is the typed
    /// refusal, and parks nothing; the managing gateway's own link is a
    /// no-op rather than a second pending prompt.
    #[tokio::test]
    async fn registration_refuses_a_gateway_other_than_the_managing_one() {
        let (store, _directory) = test_store().await;
        managed_policy::provision(&*store, "https://managed.invalid/")
            .await
            .unwrap();
        let (handle, _mcp, gateway) = test_handle_with_runtimes(&store);

        let error = register_pending_pairing(&handle, "https://other.invalid")
            .await
            .err()
            .unwrap();
        match &error {
            PairingError::Conflict {
                provisioned_url,
                replaceable,
            } => {
                assert_eq!(provisioned_url, "https://managed.invalid/");
                assert!(
                    replaceable,
                    "a provisioned row is user-consented state, so the shell may offer re-pairing"
                );
            }
            other => panic!("expected the typed conflict, got {other:?}"),
        }
        assert!(error.to_string().contains("already provisioned"));
        assert_eq!(gateway.pending_pairing_url().await, None);

        let outcome = register_pending_pairing(&handle, "https://managed.invalid")
            .await
            .unwrap();
        assert_eq!(outcome, PendingRegistration::AlreadyManaged);
        assert_eq!(gateway.pending_pairing_url().await, None);

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(
            policy.gateway_url.as_deref(),
            Some("https://managed.invalid/")
        );
    }

    /// The confirmed re-pair's registration seam: parking demands the caller
    /// name the row it replaces, a stale expectation is the typed conflict
    /// (and clobbers nothing already parked), and an OS-asserted gateway is
    /// never replaceable — from either registration path.
    #[tokio::test]
    async fn a_replacing_registration_holds_the_row_to_the_confirmed_expectation() {
        let (store, _directory) = test_store().await;
        managed_policy::provision(&*store, "https://managed.invalid/")
            .await
            .unwrap();
        let (handle, _mcp, gateway) = test_handle_with_runtimes(&store);

        let outcome =
            register_replacing_pairing(&handle, "https://new.invalid", "https://managed.invalid/")
                .await
                .unwrap();
        assert_eq!(outcome, PendingRegistration::Registered);
        assert_eq!(
            gateway.pending_pairing_url().await.as_deref(),
            Some("https://new.invalid/")
        );
        // Parking is process-ephemeral: the durable row has not moved.
        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(
            policy.gateway_url.as_deref(),
            Some("https://managed.invalid/")
        );

        // A confirmation that raced a row change: the expectation no longer
        // matches, so the registration is the typed conflict naming what the
        // row now holds — and the already-parked pairing is not clobbered.
        let error = register_replacing_pairing(
            &handle,
            "https://new.invalid",
            "https://elsewhere.invalid/",
        )
        .await
        .err()
        .unwrap();
        match &error {
            PairingError::Conflict {
                provisioned_url,
                replaceable: true,
            } => assert_eq!(provisioned_url, "https://managed.invalid/"),
            other => panic!("expected the replaceable conflict, got {other:?}"),
        }
        assert_eq!(
            gateway.pending_pairing_url().await.as_deref(),
            Some("https://new.invalid/")
        );
    }

    /// An OS (MDM) assertion outranks pairing in both directions: plain
    /// registration refuses with a conflict the shell must not escalate, and
    /// even the confirmed replacing path refuses the same way.
    #[tokio::test]
    async fn an_os_asserted_gateway_is_never_replaceable() {
        struct OsAsserted;

        impl crate::managed_policy::OsPolicySource for OsAsserted {
            fn gateway_url(&self) -> Result<Option<String>> {
                Ok(Some("https://mdm.invalid/".to_string()))
            }
        }

        let (store, _directory) = test_store().await;
        let mcp = Arc::new(McpRuntime::new(
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(TestSecrets::default()),
            Arc::new(NoGateway),
            Arc::new(OsAsserted),
        ));
        let gateway = crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            test_secrets(),
            Arc::new(OsAsserted),
        );
        let handle = PairingHandle::new(store.clone(), mcp, gateway.clone());

        for error in [
            register_pending_pairing(&handle, "https://other.invalid")
                .await
                .err()
                .unwrap(),
            register_replacing_pairing(&handle, "https://other.invalid", "https://mdm.invalid/")
                .await
                .err()
                .unwrap(),
        ] {
            match &error {
                PairingError::Conflict {
                    provisioned_url,
                    replaceable: false,
                } => assert_eq!(provisioned_url, "https://mdm.invalid/"),
                other => panic!("expected the non-replaceable conflict, got {other:?}"),
            }
        }
        assert_eq!(gateway.pending_pairing_url().await, None);
    }

    #[tokio::test]
    async fn a_commit_provisions_policy_and_a_stale_snapshot_is_never_honored() {
        let (store, _directory) = test_store().await;
        let (_handle, mcp, _gateway) = test_handle_with_runtimes(&store);

        // A model snapshot synced from some earlier deployment must not
        // survive as this gateway's model set.
        providers::write_gateway_snapshot(
            &*store,
            &providers::GatewayModelSnapshot {
                gateway_url: "https://old.gateway.test/".to_string(),
                models: vec![providers::CustomModelConfig {
                    id: "stale-model".into(),
                    upstream_id: None,
                    display_name: None,
                    context_window: 32_768,
                    max_output_tokens: 4_096,
                    ..Default::default()
                }],
                model_protocols: Default::default(),
            },
        )
        .await
        .unwrap();

        commit_signed_in_pairing(
            &*store,
            &NoOsPolicy,
            test_secrets(),
            &mcp,
            "https://gateway-a.invalid/",
            None,
        )
        .await
        .unwrap();

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(policy.managed);
        assert_eq!(
            policy.gateway_url.as_deref(),
            Some("https://gateway-a.invalid/")
        );

        // The stale snapshot carries the old deployment's stamp, so the new
        // policy reads no models until sign-in resyncs the entitled set.
        assert!(providers::gateway_models(&*store, &policy)
            .await
            .unwrap()
            .is_empty());

        // A later sign-in against the same gateway re-commits harmlessly.
        commit_signed_in_pairing(
            &*store,
            &NoOsPolicy,
            test_secrets(),
            &mcp,
            "https://gateway-a.invalid/",
            None,
        )
        .await
        .unwrap();
    }

    /// An authority that claimed the profile while the browser flow ran
    /// wins: the commit refuses rather than writing under it, and the
    /// refusal names the authority.
    #[tokio::test]
    async fn a_commit_refuses_a_profile_claimed_mid_flow() {
        let (store, _directory) = test_store().await;
        let (_handle, mcp, _gateway) = test_handle_with_runtimes(&store);
        managed_policy::provision(&*store, "https://mdm.invalid/")
            .await
            .unwrap();

        let error = commit_signed_in_pairing(
            &*store,
            &NoOsPolicy,
            test_secrets(),
            &mcp,
            "https://pending.invalid/",
            None,
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("https://mdm.invalid/"));
        assert!(!error.to_string().contains("pending.invalid"));

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(policy.gateway_url.as_deref(), Some("https://mdm.invalid/"));
    }

    /// A minimal gateway that answers session revocation, standing in for
    /// the *old* deployment during a re-pair: the commit must revoke the
    /// superseded session there. Returns the base URL and the raw bodies
    /// posted to `/oauth/revoke`.
    async fn serve_revocable_gateway() -> (String, Arc<std::sync::Mutex<Vec<String>>>) {
        let revoked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = revoked.clone();
        let app = axum::Router::new()
            .route(
                "/api/v1/meta",
                axum::routing::get(|| async {
                    axum::Json(json!({
                        "api_version": "v1",
                        "installation_id": "install-1",
                        "gateway_version": "1.0.0",
                        "public_url": "http://gateway.test",
                        "auth_mode": "oidc",
                    }))
                }),
            )
            .route(
                "/oauth/revoke",
                axum::routing::post(move |body: String| {
                    let recorded = recorded.clone();
                    async move {
                        recorded.lock().unwrap().push(body);
                        axum::Json(json!({}))
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), revoked)
    }

    /// The whole point of the slice, at the commit seam: a replacing commit
    /// swaps the provisioned row to the new gateway and retires the old
    /// deployment's session — revoked at the old gateway and gone locally —
    /// before the exchange task would store the new one.
    #[tokio::test]
    async fn a_replacing_commit_swaps_the_row_and_retires_the_old_session() {
        let (old_base, revoked) = serve_revocable_gateway().await;
        let old_url = format!("{old_base}/");
        let (store, _directory) = test_store().await;
        managed_policy::provision(&*store, &old_url).await.unwrap();
        let (_handle, mcp, _gateway) = test_handle_with_runtimes(&store);
        let secrets = test_secrets();
        let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": old_url,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_old",
            "access_tokens": {}
        }))
        .unwrap();
        crate::connectors::CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();

        commit_signed_in_pairing(
            &*store,
            &NoOsPolicy,
            secrets.clone(),
            &mcp,
            "https://new-gw.invalid/",
            Some(&old_url),
        )
        .await
        .unwrap();

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(policy.managed);
        assert_eq!(
            policy.gateway_url.as_deref(),
            Some("https://new-gw.invalid/")
        );
        assert!(
            revoked
                .lock()
                .unwrap()
                .iter()
                .any(|body| body.contains("mg_rt_old")),
            "the old session's refresh token must be revoked at the old gateway"
        );
        assert!(
            !crate::connectors::has_stored_credentials(&*secrets).await,
            "the old session must be cleared before the new one is stored"
        );
    }

    /// The compare-and-swap the confirmation rests on: a provisioned row
    /// that moved to a third gateway while the browser flow ran wins, and
    /// the replacing commit refuses rather than writing over consent the
    /// user never gave.
    #[tokio::test]
    async fn a_replacing_commit_refuses_a_row_that_moved_mid_flow() {
        let (store, _directory) = test_store().await;
        managed_policy::provision(&*store, "https://old.invalid/")
            .await
            .unwrap();
        let (_handle, mcp, _gateway) = test_handle_with_runtimes(&store);
        // Another pairing path re-pointed the row after the user's
        // confirmation named https://old.invalid/.
        store
            .set_setting(
                "managed_policy_v1",
                &json!({"gateway_url": "https://third.invalid/"}),
            )
            .await
            .unwrap();

        let error = commit_signed_in_pairing(
            &*store,
            &NoOsPolicy,
            test_secrets(),
            &mcp,
            "https://new-gw.invalid/",
            Some("https://old.invalid/"),
        )
        .await
        .err()
        .unwrap();
        assert!(error.to_string().contains("https://third.invalid/"));

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(
            policy.gateway_url.as_deref(),
            Some("https://third.invalid/")
        );
    }

    /// The commit applies the policy it writes, not just persists it: a
    /// manual MCP server this process was already running when the sign-in
    /// finished must stop serving tools before `commit_signed_in_pairing`
    /// returns, rather than at the supervisor's next sweep. Dropping the
    /// enforcement call fails here.
    #[tokio::test]
    async fn a_commit_takes_down_a_manual_mcp_server_this_process_is_running() {
        let (store, _directory) = test_store().await;
        let (_handle, mcp, _gateway) = test_handle_with_runtimes(&store);

        // A real connection, established while the profile was still open.
        mcp.replace(McpServersConfig {
            servers: vec![McpServerDefinition {
                name: "private_docs".to_string(),
                command: None,
                args: Vec::new(),
                env: std::collections::BTreeSet::new(),
                env_values: std::collections::BTreeMap::new(),
                env_from: Vec::new(),
                cwd: None,
                url: Some(serve_manual_mcp().await),
                bearer_token_env: None,
                gateway_endpoint: None,
                request_timeout_ms: 60_000,
                enabled: true,
                plugin: None,
                launch: None,
            }],
        })
        .await
        .unwrap();
        assert_eq!(mcp.info().await.servers[0].health, McpHealth::Healthy);
        assert!(mcp.snapshot().get("mcp__private_docs__lookup").is_some());

        commit_signed_in_pairing(
            &*store,
            &NoOsPolicy,
            test_secrets(),
            &mcp,
            "https://gateway.invalid/",
            None,
        )
        .await
        .unwrap();

        assert!(
            mcp.snapshot().get("mcp__private_docs__lookup").is_none(),
            "pairing must stop the server serving tools, not only record policy"
        );
        let info = mcp.info().await;
        assert_eq!(info.servers[0].health, McpHealth::Disabled);
        assert_eq!(
            info.servers[0].diagnostic.as_deref(),
            Some(MANAGED_DISABLED_DIAGNOSTIC)
        );
    }

    /// The disconnect's whole write: the CAS'd delete lands, resolution
    /// returns the open experience, and the parked pairing an earlier link
    /// left behind is dropped so the gate has nothing to stand on.
    #[tokio::test]
    async fn a_deprovision_deletes_the_row_and_drops_the_pending_pairing() {
        let (store, _directory) = test_store().await;
        let (handle, mcp, gateway) = test_handle_with_runtimes(&store);
        managed_policy::provision(&*store, "https://managed.invalid/")
            .await
            .unwrap();
        gateway
            .register_pending_pairing("https://parked.invalid/".to_string(), mcp.clone(), None)
            .await;

        assert_eq!(
            deprovision_target(&handle).await.unwrap(),
            DeprovisionTarget::Provisioned {
                gateway_url: "https://managed.invalid/".to_string()
            }
        );
        deprovision_provisioned_gateway(&handle, "https://managed.invalid/")
            .await
            .unwrap();

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(!policy.managed && !policy.misconfigured);
        assert!(store
            .get_setting("managed_policy_v1")
            .await
            .unwrap()
            .is_none());
        assert_eq!(gateway.pending_pairing_url().await, None);
        assert_eq!(
            deprovision_target(&handle).await.unwrap(),
            DeprovisionTarget::Unprovisioned
        );
    }

    /// An OS (MDM) assertion refuses disconnection outright — the probe says
    /// so and the write path refuses the same way — and a sticky row parked
    /// underneath survives untouched.
    #[tokio::test]
    async fn a_deprovision_refuses_an_os_asserted_gateway() {
        struct OsAsserted;

        impl crate::managed_policy::OsPolicySource for OsAsserted {
            fn gateway_url(&self) -> Result<Option<String>> {
                Ok(Some("https://mdm.invalid/".to_string()))
            }
        }

        let (store, _directory) = test_store().await;
        managed_policy::provision(&*store, "https://sticky.invalid/")
            .await
            .unwrap();
        let mcp = Arc::new(McpRuntime::new(
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(TestSecrets::default()),
            Arc::new(NoGateway),
            Arc::new(OsAsserted),
        ));
        let gateway = crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            test_secrets(),
            Arc::new(OsAsserted),
        );
        let handle = PairingHandle::new(store.clone(), mcp, gateway);

        assert_eq!(
            deprovision_target(&handle).await.unwrap(),
            DeprovisionTarget::OsManaged {
                gateway_url: Some("https://mdm.invalid/".to_string())
            }
        );
        for expected in ["https://mdm.invalid/", "https://sticky.invalid/"] {
            let error = deprovision_provisioned_gateway(&handle, expected)
                .await
                .err()
                .unwrap();
            match &error {
                PairingError::Conflict {
                    provisioned_url,
                    replaceable: false,
                } => assert_eq!(provisioned_url, "https://mdm.invalid/"),
                other => panic!("expected the non-replaceable conflict, got {other:?}"),
            }
        }
        assert_eq!(
            managed_policy::provisioned_url(&*store).await.unwrap(),
            Some("https://sticky.invalid/".to_string()),
            "an MDM refusal must not consume the sticky row underneath"
        );
    }

    /// The compare-and-swap the confirmation rests on, in the delete
    /// direction: a row that moved since the dialog named it refuses —
    /// naming what the row now holds — and deletes nothing; a row that
    /// vanished entirely is a raced success.
    #[tokio::test]
    async fn a_deprovision_refuses_a_row_that_moved_and_tolerates_one_that_vanished() {
        let (store, _directory) = test_store().await;
        let (handle, _mcp, _gateway) = test_handle_with_runtimes(&store);
        managed_policy::provision(&*store, "https://managed.invalid/")
            .await
            .unwrap();

        let error = deprovision_provisioned_gateway(&handle, "https://elsewhere.invalid/")
            .await
            .err()
            .unwrap();
        match &error {
            PairingError::Conflict {
                provisioned_url,
                replaceable: true,
            } => assert_eq!(provisioned_url, "https://managed.invalid/"),
            other => panic!("expected the replaceable conflict, got {other:?}"),
        }
        assert_eq!(
            managed_policy::provisioned_url(&*store).await.unwrap(),
            Some("https://managed.invalid/".to_string())
        );

        store.delete_setting("managed_policy_v1").await.unwrap();
        deprovision_provisioned_gateway(&handle, "https://managed.invalid/")
            .await
            .unwrap();
    }

    /// Disconnecting retires the stored session the way a re-pair retires a
    /// superseded one: revoked at the gateway it was minted by, and gone
    /// locally. Composes with sign-out — which deliberately does not
    /// deprovision — so the sign-out-first path ends in the same open state.
    #[tokio::test]
    async fn a_deprovision_retires_the_stored_session() {
        let (base, revoked) = serve_revocable_gateway().await;
        let gateway_url = format!("{base}/");
        let (store, _directory) = test_store().await;
        managed_policy::provision(&*store, &gateway_url)
            .await
            .unwrap();
        let secrets = test_secrets();
        let mcp = Arc::new(McpRuntime::new(
            Arc::new(ToolRegistry::new()),
            store.clone(),
            Arc::new(TestSecrets::default()),
            Arc::new(NoGateway),
            Arc::new(NoOsPolicy),
        ));
        let gateway = crate::gateway_runtime::GatewayRuntime::new(
            store.clone(),
            secrets.clone(),
            Arc::new(NoOsPolicy),
        );
        let handle = PairingHandle::new(store.clone(), mcp, gateway);
        let credentials: crate::connectors::GatewayCredentials = serde_json::from_value(json!({
            "base_url": gateway_url,
            "installation_id": "install-1",
            "user_id": "user-1",
            "refresh_token": "mg_rt_disconnect",
            "access_tokens": {}
        }))
        .unwrap();
        crate::connectors::CredentialVault::new(secrets.clone())
            .save(&credentials)
            .await
            .unwrap();

        deprovision_provisioned_gateway(&handle, &gateway_url)
            .await
            .unwrap();

        assert!(!resolve(&*store, &NoOsPolicy).await.unwrap().managed);
        assert!(
            revoked
                .lock()
                .unwrap()
                .iter()
                .any(|body| body.contains("mg_rt_disconnect")),
            "the session's refresh token must be revoked at the gateway"
        );
        assert!(
            !crate::connectors::has_stored_credentials(&*secrets).await,
            "the keychain session must not survive the disconnect"
        );
    }
}
