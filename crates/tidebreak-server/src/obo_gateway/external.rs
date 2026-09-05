//! Gateway delegation for confirmed external connections.
//!
//! The store retains consent identifiers, never gateway tokens. Each grant
//! has a separate in-memory gateway client so browser credentials cannot
//! supply or replace its authority.

use super::*;
use tidebreak_core::db::DbStore;
use tidebreak_core::{CodeGrantId, CodeHandshakeId, SessionId};

struct DelegatedGateway {
    expires_at: u64,
    gateway: Arc<OboGateway>,
}

type DelegationSlot = Arc<tokio::sync::Mutex<Option<DelegatedGateway>>>;

pub struct ExternalDelegations {
    gateway: Arc<OboGateway>,
    db: Arc<DbStore>,
    slots: std::sync::Mutex<HashMap<CodeGrantId, DelegationSlot>>,
    sweep_started: std::sync::atomic::AtomicBool,
    revoked: std::sync::Mutex<std::collections::HashSet<CodeGrantId>>,
}

#[derive(serde::Deserialize)]
struct DelegationIdentity {
    delegation_id: String,
    resource: String,
    user_id: String,
}

#[derive(serde::Deserialize)]
struct DelegationToken {
    access_token: String,
    token_type: String,
    expires_in: u64,
    resource: String,
    user_id: String,
}

fn reconnect() -> AgentError {
    AgentError::SignInRequired(
        "this external connection has no live gateway delegation; reconnect it from Slack".into(),
    )
}

impl ExternalDelegations {
    pub fn new(gateway: Arc<OboGateway>, db: Arc<DbStore>) -> Self {
        Self {
            gateway,
            db,
            slots: std::sync::Mutex::new(HashMap::new()),
            sweep_started: std::sync::atomic::AtomicBool::new(false),
            revoked: std::sync::Mutex::new(std::collections::HashSet::new()),
        }
    }

    /// Reconcile durable revocations after restart and retry transport failures.
    pub fn start_revocation_sweep(self: &Arc<Self>) {
        if self
            .sweep_started
            .swap(true, std::sync::atomic::Ordering::AcqRel)
        {
            return;
        }
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(this) = weak.upgrade() else {
                    return;
                };
                if let Err(error) = this.reconcile_revocations().await {
                    tracing::warn!(%error, "could not reconcile external gateway revocations");
                }
                drop(this);
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        });
    }

    async fn reconcile_revocations(&self) -> Result<()> {
        for handshake in
            tidebreak_core::db::code::revoked_connect_handshakes_all_owners(&self.db).await?
        {
            if let (Some(owner), Some(grant)) = (handshake.approval_owner, handshake.grant_id) {
                self.revoke(&owner, grant).await?;
            }
        }
        Ok(())
    }

    fn endpoint(&self, suffix: &str) -> Result<reqwest::Url> {
        let base = normalized_gateway_base(&self.gateway.gateway_base_url)?;
        join_below(
            &base,
            &format!("api/v1/tidebreak/external-delegations{suffix}"),
        )
    }

    fn machine_auth(&self) -> Result<serde_json::Value> {
        let (client_id, client_secret) =
            self.gateway.machine_credentials.as_ref().ok_or_else(|| {
                AgentError::config("this machine has no gateway identity for external connections")
            })?;
        Ok(serde_json::json!({
            "client_id": client_id,
            "client_secret": client_secret,
            "resource": self.gateway.resource,
        }))
    }

    /// Enroll only after the approval route validates the owner and CSRF.
    /// The adapter still needs to confirm before any grant can use this ID.
    pub(crate) async fn enroll(
        &self,
        owner: &OwnerId,
        id: CodeHandshakeId,
        subject: &str,
    ) -> Result<()> {
        let _ = self.machine_auth()?;
        let response = self.gateway.client.post(self.endpoint("")?)
            .bearer_auth(subject)
            .json(&serde_json::json!({"delegation_id": id.to_string(), "resource": self.gateway.resource}))
            .send().await.map_err(|error| AgentError::msg(format!("gateway delegation approval failed: {error}")))?;
        let status = response.status();
        let body = read_bounded(response, RESPONSE_LIMIT).await?;
        if !status.is_success() {
            return Err(if status.is_client_error() {
                reconnect()
            } else {
                AgentError::msg("the gateway could not approve this external connection; try again")
            });
        }
        let identity: DelegationIdentity = serde_json::from_slice(&body).map_err(|_| {
            AgentError::msg("the gateway returned an unreadable delegation approval")
        })?;
        if identity.delegation_id != id.to_string()
            || identity.resource != self.gateway.resource
            || owner.as_str().strip_prefix("user:") != Some(identity.user_id.as_str())
        {
            return Err(AgentError::InvalidTarget(
                "the gateway approved a different external connection".into(),
            ));
        }
        Ok(())
    }

    async fn live_handshake(&self, owner: &OwnerId, grant: CodeGrantId) -> Result<CodeHandshakeId> {
        let row = tidebreak_core::db::code::get_external_grant(&self.db, owner, grant)
            .await?
            .ok_or_else(reconnect)?;
        if row.revoked_at.is_some() {
            // The durable local refusal applies even when the gateway is down.
            if let Err(error) = self.revoke(owner, grant).await {
                tracing::warn!(%grant, %error, "gateway delegation revocation will retry on the next request");
            }
            return Err(reconnect());
        }
        let handshake =
            tidebreak_core::db::code::completed_connect_handshake_for_grant(&self.db, owner, grant)
                .await?
                .ok_or_else(reconnect)?;
        Ok(handshake.id)
    }

    /// Validate durable consent before serving even a fresh cached token.
    pub async fn for_grant(&self, owner: &OwnerId, grant: CodeGrantId) -> Result<Arc<OboGateway>> {
        let handshake = self.live_handshake(owner, grant).await?;
        let slot = {
            let mut slots = self
                .slots
                .lock()
                .map_err(|_| AgentError::msg("external delegation state is unavailable"))?;
            slots
                .entry(grant)
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(None)))
                .clone()
        };
        let mut held = slot.lock().await;
        if let Some(current) = held.as_ref() {
            if current.expires_at > unix_time().saturating_add(EXPIRY_LEEWAY_SECONDS) {
                return Ok(current.gateway.clone());
            }
        }
        let response = self
            .gateway
            .client
            .post(self.endpoint(&format!("/{handshake}/token"))?)
            .json(&self.machine_auth()?)
            .send()
            .await
            .map_err(|error| {
                AgentError::msg(format!("gateway delegation token request failed: {error}"))
            })?;
        let status = response.status();
        let body = read_bounded(response, RESPONSE_LIMIT).await?;
        if !status.is_success() {
            *held = None;
            return Err(if status.is_client_error() {
                reconnect()
            } else {
                AgentError::msg(
                    "the gateway could not issue an external connection token; try again",
                )
            });
        }
        let token: DelegationToken = serde_json::from_slice(&body).map_err(|_| {
            AgentError::msg("the gateway returned an unreadable external connection token")
        })?;
        if owner.as_str().strip_prefix("user:") != Some(token.user_id.as_str())
            || token.resource != self.gateway.resource
            || token.token_type != "Bearer"
            || token.access_token.is_empty()
            || token.expires_in == 0
        {
            return Err(AgentError::InvalidTarget(
                "the gateway issued a token for a different external connection".into(),
            ));
        }
        // A revoke may commit while the mint is in flight. Recheck before
        // publishing its result into the slot or using it for an exchange.
        self.live_handshake(owner, grant).await?;
        let gateway = Arc::new(OboGateway::new(
            &self.gateway.gateway_base_url,
            self.gateway.resource.clone(),
        )?);
        gateway.record_caller(owner, token.access_token.into());
        *held = Some(DelegatedGateway {
            expires_at: unix_time().saturating_add(token.expires_in),
            gateway: gateway.clone(),
        });
        Ok(gateway)
    }

    /// Bound sessions always use their grant; an ordinary session retains
    /// its browser credential path. Multiple grants never choose by order.
    pub async fn for_session(
        &self,
        owner: &OwnerId,
        session: SessionId,
    ) -> Result<Option<Arc<OboGateway>>> {
        let bindings =
            tidebreak_core::db::code::list_bindings_for_session(&self.db, owner, session).await?;
        let Some(first) = bindings.first() else {
            return Ok(None);
        };
        if bindings
            .iter()
            .any(|binding| binding.grant_id != first.grant_id)
        {
            return Err(AgentError::InvalidTarget(
                "this session has conflicting external connections".into(),
            ));
        }
        self.for_grant(owner, first.grant_id).await.map(Some)
    }

    /// Local revocation commits first. This removes cached authority and
    /// revokes the gateway's delegation with the machine identity.
    pub async fn revoke(&self, owner: &OwnerId, grant: CodeGrantId) -> Result<()> {
        if let Ok(mut slots) = self.slots.lock() {
            slots.remove(&grant);
        }
        if self
            .revoked
            .lock()
            .is_ok_and(|revoked| revoked.contains(&grant))
        {
            return Ok(());
        }
        let Some(handshake) =
            tidebreak_core::db::code::completed_connect_handshake_for_grant(&self.db, owner, grant)
                .await?
        else {
            return Ok(());
        };
        let response = self
            .gateway
            .client
            .post(self.endpoint(&format!("/{}/revoke", handshake.id))?)
            .json(&self.machine_auth()?)
            .send()
            .await
            .map_err(|error| {
                AgentError::msg(format!("gateway delegation revocation failed: {error}"))
            })?;
        if response.status().is_success() {
            if let Ok(mut revoked) = self.revoked.lock() {
                revoked.insert(grant);
            }
            Ok(())
        } else {
            Err(AgentError::msg(
                "the gateway could not revoke this external connection",
            ))
        }
    }
}

#[cfg(test)]
mod tests;
