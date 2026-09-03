//! Pairing, sign-in, sign-out, status, and the gateway connection.

use super::*;

/// Retire a stored gateway session the resolved policy no longer stands
/// behind.
///
/// Two ways a session ends up superseded. An unmanaged profile can carry one
/// signed in under the retired additive mode: nothing reaches it any more —
/// the whole sign-in surface is managed-only, and the renderer has no
/// gateway page. And a managed profile's policy authority can re-point the
/// deployment (an MDM push): the old deployment's session is filtered out
/// of every route and token path, but its refresh token stays live at the
/// old gateway. Either way no surface will ever revoke it, so boot owns
/// that cleanup.
///
/// Revocation is best-effort and bounded: an unreachable gateway can no more
/// hold this hostage than it can a normal sign-out (the server-side session
/// still dies at refresh-token expiry), and boot must not stall on it. The
/// local clear afterwards is unconditional, so the session is gone locally
/// whether or not the gateway ever answered — and because it is gone, this
/// whole step runs at most once per superseded session.
///
/// An unreadable stored blob is left alone: it carries no usable refresh
/// token, so it is not the live zombie this exists to kill. So is the
/// session under a managed policy with no usable URL — that is a
/// misconfigured policy to repair, not a supersession, and the session may
/// well match it once repaired.
pub async fn retire_superseded_gateway_session(
    secrets: Arc<dyn SecretProvider>,
    policy: &GatewayPolicy,
) -> Result<()> {
    /// Long enough for a healthy gateway to answer a revoke, short enough
    /// that a dead one is a hiccup at boot rather than a hang.
    const REVOKE_TIMEOUT: Duration = Duration::from_secs(5);

    let vault = CredentialVault::new(secrets.clone());
    let Ok(Some(credentials)) = vault.load().await else {
        return Ok(());
    };
    if policy.managed {
        let Some(gateway_url) = policy.gateway_url.as_deref() else {
            return Ok(());
        };
        if credentials.matches_base_url(gateway_url) {
            return Ok(());
        }
        tracing::warn!(
            "retiring the model-gateway session for {}: the managed policy \
             now resolves {gateway_url}; sign in there to connect",
            credentials.base_url
        );
    } else {
        tracing::warn!(
            "clearing a model-gateway session left by the retired additive \
             configuration ({}); pair via your gateway's page to sign in again",
            credentials.base_url
        );
    }
    // The connection owns revoke-then-clear (the refresh token never leaves
    // the connectors crate), so the session is retired through the same path
    // an explicit sign-out takes.
    if let Ok(config) = GatewayAuthConfig::new(&credentials.base_url) {
        if let Ok(auth) = GatewayAuth::new(config) {
            let connection = GatewayConnection::new(auth, CredentialVault::new(secrets.clone()));
            let _ = tokio::time::timeout(REVOKE_TIMEOUT, connection.sign_out()).await;
        }
    }
    // Unconditional: a gateway that never answered, or a stored base URL that
    // no longer parses, must not leave the credential behind.
    CredentialVault::new(secrets).clear().await
}

/// Compare the durable parts of two credentials while ignoring only cached
/// access tokens. The refresh token is intentionally included: a replacement
/// session for the same user and installation must still invalidate a catalog
/// response authorized by the old session. A concurrent token rotation may
/// conservatively reject a sync; the next background tick retries safely.
pub(super) fn same_gateway_session(left: &GatewayCredentials, right: &GatewayCredentials) -> bool {
    fn durable(credentials: &GatewayCredentials) -> Option<serde_json::Value> {
        let mut value = serde_json::to_value(credentials).ok()?;
        value.as_object_mut()?.remove("access_tokens");
        Some(value)
    }

    durable(left) == durable(right)
}

/// The one refusal for every managed-only gateway surface: unmanaged
/// profiles have no gateway (policy is the only source), and a managed
/// policy without a usable URL is misconfigured rather than open.
pub(super) fn require_managed(policy: &GatewayPolicy) -> Result<String> {
    if !policy.managed {
        return Err(AgentError::config(
            "this profile is not connected to a model gateway; \
             pair via your gateway's page to connect",
        ));
    }
    policy.gateway_url.clone().ok_or_else(|| {
        AgentError::config(
            "the managed gateway policy has no usable gateway URL; repair the policy authority",
        )
    })
}

impl GatewayRuntime {
    /// Park a shell-validated pairing until a sign-in consents to it,
    /// replacing any earlier one — the latest link is the one the user acted
    /// on. Invalidate any in-flight browser flow the same way `sign_out`
    /// does: an exchange started against a replaced pairing must abandon
    /// rather than commit it.
    pub async fn register_pending_pairing(
        &self,
        base_url: String,
        mcp: Arc<dyn GatewayMcpControl>,
        commit: Arc<dyn GatewayPairingCommit>,
        replaces: Option<String>,
    ) {
        let mut sign_in = self.sign_in.lock().await;
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *sign_in = SignInProgress::Idle;
        *self.pending_pairing.lock().await = Some(PendingPairing {
            base_url,
            mcp,
            commit,
            replaces,
        });
    }

    /// The pending pairing's gateway URL, for the `/policy` projection.
    pub async fn pending_pairing_url(&self) -> Option<String> {
        self.pending_pairing
            .lock()
            .await
            .as_ref()
            .map(|pending| pending.base_url.clone())
    }

    /// Decline the pending pairing: clear it and invalidate any browser flow
    /// it started. Renderer-reachable, deliberately — declining changes
    /// nothing durable, so the failure direction is safe. With nothing
    /// pending it is a strict no-op: the generation must not move, or a
    /// stray dismiss could abandon a legitimate managed sign-in mid-flight.
    pub async fn dismiss_pending_pairing(&self) {
        let mut sign_in = self.sign_in.lock().await;
        let mut pending = self.pending_pairing.lock().await;
        if pending.is_none() {
            return;
        }
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *sign_in = SignInProgress::Idle;
        *pending = None;
    }

    /// The renderer-facing connection status, derived from policy alone: a
    /// profile is gateway-connected exactly when managed policy asserts it,
    /// so an unmanaged profile reads no gateway whatever legacy rows
    /// persist, and a managed policy whose URL is missing (misconfigured)
    /// reads none, honestly.
    pub async fn status(&self) -> Result<GatewayStatus> {
        // One policy read for the whole projection: the renderer polls this
        // every couple of seconds while a sign-in is pending.
        let policy = self.policy()?;
        let base_url = policy.gateway_url.clone();
        let credentials = match self.connection_for(&policy).await? {
            Some(connection) => connection.stored_credentials().await?,
            None => None,
        };
        let snapshot = self.model_state.snapshot(&policy).await?;
        Ok(GatewayStatus {
            base_url,
            signed_in: credentials.is_some(),
            account_hint: credentials
                .as_ref()
                .and_then(|credentials| credentials.account_hint.clone()),
            installation_id: credentials
                .as_ref()
                .map(|credentials| credentials.installation_id.clone()),
            model_count: snapshot
                .as_ref()
                .map(|snapshot| snapshot.models.len())
                .unwrap_or_default(),
            member_catalog: snapshot.and_then(|snapshot| snapshot.member_catalog),
            sign_in: self.sign_in.lock().await.clone(),
        })
    }

    /// The hosted machine this profile's gateway offers, for the address
    /// field on the settings panel.
    ///
    /// Read from the gateway's unauthenticated `/api/v1/meta` rather than
    /// from the provision link: the link fires once at pair time, so a
    /// profile paired earlier would never see the value and a machine that
    /// moved would leave a stale one behind. Meta is re-read every boot.
    ///
    /// Every failure reads as no offer — an unmanaged profile, a gateway
    /// older than the field, a gateway that does not answer. The value is a
    /// hint for a text field and never authorization: attaching still runs
    /// discovery, which holds the machine to naming this same gateway.
    pub async fn offered_machine(&self) -> GatewayMachineOffer {
        GatewayMachineOffer {
            url: self.read_offered_machine().await,
        }
    }

    /// The offer, memoized per gateway. Only a valid, present offer is
    /// remembered, so a boot that raced either the network or the gateway
    /// rollout retries on the next ask. An unmanaged profile that pairs
    /// mid-session also reads its new gateway rather than a prior absence.
    pub(super) async fn read_offered_machine(&self) -> Option<String> {
        let policy = self.policy().ok()?;
        if !policy.managed {
            return None;
        }
        let base_url = policy.gateway_url?;
        let mut memo = self.machine_offer.lock().await;
        if let Some((read_from, offer)) = memo.as_ref() {
            if *read_from == base_url {
                return Some(offer.clone());
            }
        }
        let connection = self.connection_at(base_url.clone()).await.ok()?;
        let meta = connection.auth().meta().await.ok()?;
        // The reader sees this in a text box, so hold it to the same URL
        // rules the connect path enforces: a value that could never be
        // connected to is worse than an empty field.
        let offer = meta
            .tidebreak_machine_url
            .filter(|url| GatewayAuthConfig::new(url).is_ok());
        if let Some(offer) = offer.as_ref() {
            *memo = Some((base_url, offer.clone()));
        }
        offer
    }

    /// Start a browser sign-in and return the URL to open.
    ///
    /// A pending pairing wins the target: the sign-in runs against the
    /// pairing's gateway, and a successful exchange is what commits the
    /// provision — the sign-in the user chose to complete is the pairing's
    /// consent, so nothing durable exists until it succeeds. That holds on a
    /// managed profile too: a pending pairing can only exist there through
    /// the shell's confirmed re-pair flow, never a bare deep link, so
    /// honoring it is honoring that confirmation. With nothing pending, a
    /// managed profile's sign-in targets the policy's gateway, and an
    /// unmanaged one keeps the legible refusal.
    ///
    /// The exchange completes in a background task: on success the session is
    /// stored (after any pairing commit), the entitled models synced, and the
    /// entitled MCP endpoints auto-mounted into `mcp`; on failure the status
    /// surface carries the bounded error until the next attempt.
    pub async fn begin_sign_in(
        self: &Arc<Self>,
        mcp: Arc<dyn GatewayMcpControl>,
    ) -> Result<String> {
        let policy = self.policy()?;
        let pairing = self.pending_pairing.lock().await.clone();
        let connection = match &pairing {
            Some(pending) => self.connection_at(pending.base_url.clone()).await?,
            None => self.connection_at(require_managed(&policy)?).await?,
        };
        let pending = connection.auth().start_sign_in().await?;
        let authorization_url = pending.authorization_url().to_string();
        let mcp = pairing
            .as_ref()
            .map(|pending| pending.mcp.clone())
            .unwrap_or(mcp);
        let generation = {
            let mut sign_in = self.sign_in.lock().await;
            let generation = self
                .sign_in_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                + 1;
            *sign_in = SignInProgress::Pending {
                authorization_url: authorization_url.clone(),
            };
            generation
        };

        let runtime = self.clone();
        tokio::spawn(async move {
            let finished = pending.finish(SIGN_IN_TIMEOUT).await;
            let session = match finished {
                Ok(session) => session,
                Err(error) => {
                    let mut sign_in = runtime.sign_in.lock().await;
                    if runtime
                        .sign_in_generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == generation
                    {
                        *sign_in = SignInProgress::Failed {
                            message: error.to_string(),
                        };
                    }
                    return;
                }
            };

            // Lock order for every locally controlled authority mutation is
            // model authority -> pairing (when policy may move) -> sign-in
            // state. The write lease waits for already-authorized request legs
            // to dispatch, then excludes new ones through the policy commit,
            // old-session retirement, and new-session store. Acquiring the
            // pairing lock before sign-in state also matches registration and
            // deprovision, avoiding the former sign-in/pairing inversion.
            let _authority = runtime.lock_model_authority_mutation().await;
            let _pairing = GATEWAY_PAIRING_WRITES.lock().await;
            let mut sign_in = runtime.sign_in.lock().await;
            if runtime
                .sign_in_generation
                .load(std::sync::atomic::Ordering::SeqCst)
                != generation
            {
                return;
            }

            // A pairing commits before its session persists: if the provision
            // cannot be written (an MDM push claimed the profile mid-flow), no
            // session lands on a profile the pairing's gateway does not manage.
            let committed = match &pairing {
                Some(pending) => runtime.commit_pairing_locked(pending).await,
                None => Ok(()),
            };
            let stored = match committed {
                Ok(()) => connection.store_session(&session).await,
                Err(error) => Err(error),
            };
            let stored = match stored {
                Ok(()) => {
                    *sign_in = SignInProgress::Idle;
                    true
                }
                Err(error) => {
                    *sign_in = SignInProgress::Failed {
                        message: error.to_string(),
                    };
                    false
                }
            };
            drop(sign_in);
            drop(_pairing);
            drop(_authority);

            if !stored {
                return;
            }
            // These are post-commit refreshes, not authority mutations. Run
            // them after releasing the writer so healthy inference is not
            // serialized behind model and endpoint network reads.
            if let Err(error) = runtime.sync_models().await {
                tracing::warn!(
                    "gateway model sync after sign-in failed \
                     (the background sync will retry): {}",
                    error.message()
                );
            }
            if let Err(error) = runtime.reconcile_endpoint_mounts(&*mcp).await {
                tracing::warn!(
                    "gateway endpoint auto-mount after sign-in failed \
                     (the background sync will retry): {error}"
                );
            }
            mcp.refresh_connected_app_roster().await;
        });
        Ok(authorization_url)
    }

    /// Commit the pairing a finishing sign-in consented to, then clear it.
    /// Runs from the exchange task with the sign-in state lock held, so it
    /// cannot interleave with a dismissal or re-registration.
    pub(super) async fn commit_pairing_locked(&self, pending: &PendingPairing) -> Result<()> {
        pending
            .commit
            .commit(&pending.base_url, pending.replaces.as_deref())
            .await?;
        *self.pending_pairing.lock().await = None;
        Ok(())
    }

    /// Take the exclusive side of the request-leg authority fence.
    ///
    /// Callers that also need the pairing or sign-in locks must take them
    /// after this guard, in that order. The owned guard lets pairing code hold
    /// the fence across its compare-and-swap and session retirement without
    /// exposing the lock itself.
    pub async fn lock_model_authority_mutation(&self) -> OwnedRwLockWriteGuard<()> {
        self.model_sync.clone().write_owned().await
    }

    #[doc(hidden)]
    pub async fn commit_signed_in_pairing_for_test(
        &self,
        commit: Arc<dyn GatewayPairingCommit>,
        base_url: &str,
        replaces: Option<&str>,
    ) -> Result<()> {
        let _authority = self.lock_model_authority_mutation().await;
        let _pairing = GATEWAY_PAIRING_WRITES.lock().await;
        let _sign_in = self.sign_in.lock().await;
        commit.commit(base_url, replaces).await
    }

    /// Revoke the session (best-effort at the gateway), clear local state, and
    /// drop the synced model snapshot. Managed-only, like sign-in.
    pub async fn sign_out(&self) -> Result<()> {
        // Authority is always outermost: an already-authorized request leg
        // dispatches before this writer proceeds, while a later leg observes
        // the cleared session and snapshot. The sign-in state lock nests
        // inside it, matching sign-in completion.
        let _model_sync = self.lock_model_authority_mutation().await;
        // Take the state lock for the whole operation and invalidate any
        // pending browser flow before revoking anything: an exchange that
        // completes during the revoke round-trip serializes behind this lock
        // and then observes the bump, so it abandons instead of re-saving
        // the session it just minted.
        let mut sign_in = self.sign_in.lock().await;
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let policy = self.policy()?;
        let base_url = require_managed(&policy)?;
        self.connection_at(base_url.clone())
            .await?
            .sign_out()
            .await?;
        {
            // Serialize the snapshot clear with the other snapshot writers so
            // it cannot land inside one of their recheck-and-write windows.
            // Lock order matches the sign-in task's sync path: the sign-in
            // state lock is already held, the snapshot lock nests inside it.
            let _lock = GATEWAY_STATE_WRITES.lock().await;
            if self
                .model_state
                .snapshot(&policy)
                .await?
                .is_some_and(|snapshot| !snapshot.models.is_empty())
            {
                self.model_state
                    .write_snapshot(&GatewayModelSnapshot {
                        gateway_url: base_url,
                        installation_id: None,
                        models: Vec::new(),
                        model_protocols: Default::default(),
                        model_reasoning_efforts: Default::default(),
                        member_catalog: None,
                        catalog_etag: None,
                    })
                    .await?;
            }
        }
        *sign_in = SignInProgress::Idle;
        Ok(())
    }

    /// Invalidate any in-flight browser sign-in and drop the pending pairing
    /// — the disconnect epilogue. Same discipline as sign-out: an exchange
    /// that completes afterwards serializes behind the state lock, observes
    /// the generation bump, and abandons rather than re-saving the session
    /// it just minted.
    pub async fn abandon_sign_in_and_pairing(&self) {
        let mut sign_in = self.sign_in.lock().await;
        self.sign_in_generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        *self.pending_pairing.lock().await = None;
        *sign_in = SignInProgress::Idle;
    }

    /// Retire the stored session against the current resolved policy: after
    /// a deprovision the profile is unmanaged, so [`sign_out`](Self::sign_out)
    /// (managed-only) cannot run, but the keychain session must still be
    /// revoked (best-effort) and cleared (unconditionally). Thin wrapper so
    /// callers without the secrets handle can reach
    /// [`retire_superseded_gateway_session`].
    pub async fn retire_session_for_current_policy(
        &self,
        _authority: &OwnedRwLockWriteGuard<()>,
    ) -> Result<()> {
        let policy = self.policy()?;
        retire_superseded_gateway_session(self.secrets.clone(), &policy).await
    }

    /// The connection for the policy's gateway, or `None` when the profile is
    /// unmanaged (or the managed policy is misconfigured).
    ///
    /// The deployment comes from the resolved policy and nowhere else. The
    /// retired provider row was renderer-writable while unmanaged, so
    /// honoring it here would let a pre-provisioning write redirect sign-in
    /// and every minted bearer; it is never read.
    pub async fn connection(&self) -> Result<Option<Arc<GatewayConnection>>> {
        let policy = self.policy()?;
        self.connection_for(&policy).await
    }

    /// [`connection`](Self::connection) against an already-resolved policy, for
    /// callers that have one in hand.
    pub(super) async fn connection_for(
        &self,
        policy: &GatewayPolicy,
    ) -> Result<Option<Arc<GatewayConnection>>> {
        let Some(base_url) = policy.gateway_url.clone().filter(|_| policy.managed) else {
            return Ok(None);
        };
        Ok(Some(self.connection_at(base_url).await?))
    }

    /// The connection for the managed gateway, refusing legibly when the
    /// profile is unmanaged: the sign-in surface (sign-in, sign-out, apps,
    /// model sync) exists only under managed policy.
    pub(super) async fn managed_connection(&self) -> Result<Arc<GatewayConnection>> {
        let policy = self.policy()?;
        let base_url = require_managed(&policy)?;
        self.connection_at(base_url).await
    }

    /// The cached connection for `base_url`, rebuilt when the URL changes.
    pub(super) async fn connection_at(&self, base_url: String) -> Result<Arc<GatewayConnection>> {
        let mut cached = self.cached.lock().await;
        if let Some((url, connection)) = cached.as_ref() {
            if *url == base_url {
                return Ok(connection.clone());
            }
        }
        let auth_config = GatewayAuthConfig::new(&base_url)?;
        let connection = Arc::new(GatewayConnection::new(
            GatewayAuth::new(auth_config)?,
            CredentialVault::new(self.secrets.clone()),
        ));
        *cached = Some((base_url, connection.clone()));
        Ok(connection)
    }
}
