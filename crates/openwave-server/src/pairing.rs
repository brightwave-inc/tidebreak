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

use openwave_connectors::GatewayAuthConfig;
use openwave_core::{AgentError, Result, Store};
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
/// fault: the recorded decision is refuse-forever (gateway migration is the
/// MDM tier's job — OS-asserted policy already outranks provisioned state —
/// or a profile reset), and the shell owes the user an explanation naming
/// the gateway that actually manages this device rather than a log line.
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
        return match policy.gateway_url {
            Some(existing) if existing == base_url => Ok(PendingRegistration::AlreadyManaged),
            Some(existing) => Err(PairingError::Conflict {
                provisioned_url: existing,
            }),
            None => Err(PairingError::Other(AgentError::config(
                "this device's managed policy is misconfigured; contact your administrator",
            ))),
        };
    }
    handle
        .gateway
        .register_pending_pairing(base_url, handle.mcp.clone())
        .await;
    Ok(PendingRegistration::Registered)
}

/// Provision the profile a finished sign-in consented to.
///
/// Called from the sign-in exchange task with a session already minted for
/// `base_url`. Policy is re-resolved under the pairing lock: an authority
/// that claimed the profile while the browser flow ran (an MDM push) wins,
/// and the pairing is refused rather than written under it. Provisioning is
/// the only durable write — the model snapshot is stamped with the
/// deployment it was synced from, so one left behind by any earlier
/// configuration is simply never honored for this gateway.
///
/// The new policy is applied to this process before returning: manual MCP
/// servers running under the previously open profile are taken down here
/// rather than at the supervisor's next sweep, so no locked child keeps
/// serving tools across the window in between.
pub(crate) async fn commit_signed_in_pairing(
    store: &dyn Store,
    os_policy: &dyn crate::managed_policy::OsPolicySource,
    mcp: &McpRuntime,
    base_url: &str,
) -> openwave_core::Result<()> {
    // PAIRING makes the policy re-read and the write atomic against another
    // pairing path; no gateway-state lock, for the same reason the old
    // probe-then-provision path took none — the snapshot stamp is the guard.
    let _guard = PAIRING.lock().await;
    let policy = managed_policy::resolve(store, os_policy).await?;
    if policy.managed && policy.gateway_url.as_deref() != Some(base_url) {
        let authority = policy
            .gateway_url
            .unwrap_or_else(|| "another authority".to_string());
        return Err(AgentError::config(format!(
            "this device became managed by {authority} during sign-in; the pairing was not applied"
        )));
    }
    managed_policy::provision(store, base_url).await?;
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

        let outcome = register_pending_pairing(&handle, "http://gw.invalid")
            .await
            .unwrap();
        assert_eq!(outcome, PendingRegistration::Registered);
        assert!(!resolve(&*store, &NoOsPolicy).await.unwrap().managed);
        assert_eq!(
            gateway.pending_pairing_url().await.as_deref(),
            Some("http://gw.invalid/"),
            "the parked URL is the normalized one the commit will write"
        );

        let outcome = register_pending_pairing(&handle, "http://other.invalid")
            .await
            .unwrap();
        assert_eq!(outcome, PendingRegistration::Registered);
        assert_eq!(
            gateway.pending_pairing_url().await.as_deref(),
            Some("http://other.invalid/")
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
        managed_policy::provision(&*store, "http://managed.invalid/")
            .await
            .unwrap();
        let (handle, _mcp, gateway) = test_handle_with_runtimes(&store);

        let error = register_pending_pairing(&handle, "http://other.invalid")
            .await
            .err()
            .unwrap();
        match &error {
            PairingError::Conflict { provisioned_url } => {
                assert_eq!(provisioned_url, "http://managed.invalid/")
            }
            other => panic!("expected the typed conflict, got {other:?}"),
        }
        assert!(error.to_string().contains("already provisioned"));
        assert_eq!(gateway.pending_pairing_url().await, None);

        let outcome = register_pending_pairing(&handle, "http://managed.invalid")
            .await
            .unwrap();
        assert_eq!(outcome, PendingRegistration::AlreadyManaged);
        assert_eq!(gateway.pending_pairing_url().await, None);

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(
            policy.gateway_url.as_deref(),
            Some("http://managed.invalid/")
        );
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
                gateway_url: "http://old.gateway.test/".to_string(),
                models: vec![providers::CustomModelConfig {
                    id: "stale-model".into(),
                    display_name: None,
                    context_window: 32_768,
                    max_output_tokens: 4_096,
                }],
            },
        )
        .await
        .unwrap();

        commit_signed_in_pairing(&*store, &NoOsPolicy, &mcp, "http://gateway-a.invalid/")
            .await
            .unwrap();

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert!(policy.managed);
        assert_eq!(
            policy.gateway_url.as_deref(),
            Some("http://gateway-a.invalid/")
        );

        // The stale snapshot carries the old deployment's stamp, so the new
        // policy reads no models until sign-in resyncs the entitled set.
        assert!(providers::gateway_models(&*store, &policy)
            .await
            .unwrap()
            .is_empty());

        // A later sign-in against the same gateway re-commits harmlessly.
        commit_signed_in_pairing(&*store, &NoOsPolicy, &mcp, "http://gateway-a.invalid/")
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
        managed_policy::provision(&*store, "http://mdm.invalid/")
            .await
            .unwrap();

        let error = commit_signed_in_pairing(&*store, &NoOsPolicy, &mcp, "http://pending.invalid/")
            .await
            .err()
            .unwrap();
        assert!(error.to_string().contains("http://mdm.invalid/"));
        assert!(!error.to_string().contains("pending.invalid"));

        let policy = resolve(&*store, &NoOsPolicy).await.unwrap();
        assert_eq!(policy.gateway_url.as_deref(), Some("http://mdm.invalid/"));
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
                env: std::collections::BTreeMap::new(),
                env_from: Vec::new(),
                cwd: None,
                url: Some(serve_manual_mcp().await),
                bearer_token_env: None,
                gateway_endpoint: None,
                request_timeout_ms: 60_000,
                enabled: true,
            }],
        })
        .await
        .unwrap();
        assert_eq!(mcp.info().await.servers[0].health, McpHealth::Healthy);
        assert!(mcp.snapshot().get("mcp__private_docs__lookup").is_some());

        commit_signed_in_pairing(&*store, &NoOsPolicy, &mcp, "http://gateway.invalid/")
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
}
