//! Adapter grant credentials (docs/slack-sessions.md, stage 2).
//!
//! This layer owns the secrets: it mints the token pair, hashes it for
//! storage, and authenticates presented tokens. The store layer
//! (`tidebreak_core::db::code::grant`) sees only hashes. Revocation fans
//! out live through [`GrantRevocations`], so an event WebSocket holding a
//! revoked grant drops immediately instead of at its next read.

use std::sync::Arc;

use sha2::{Digest, Sha256};
use tidebreak_core::{CodeExternalGrant, CodeGrantId, GrantRotation, OwnerId};

use crate::error::ServerError;

/// A freshly minted token pair. The only copy of the secrets: the store
/// keeps hashes, and this value goes to the adapter once.
pub(crate) struct AdapterTokenPair {
    /// The access token every grant call presents (`tbg_` prefix).
    pub token: String,
    /// The refresh token rotation trades in (`tbr_` prefix).
    pub refresh: String,
}

/// Hex SHA-256 of a presented token, the only form the store sees.
pub(crate) fn hash_adapter_token(secret: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(secret.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn mint_secret(prefix: &str) -> String {
    // Two v4 UUIDs give 244 bits of OS-sourced entropy.
    format!(
        "{prefix}_{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

fn mint_pair() -> AdapterTokenPair {
    AdapterTokenPair {
        token: mint_secret("tbg"),
        refresh: mint_secret("tbr"),
    }
}

/// Live fan-out for revocations. The events WebSocket route subscribes and
/// closes its stream the moment its grant's id arrives; everything else
/// learns from the durable row.
pub(crate) struct GrantRevocations {
    sender: tokio::sync::broadcast::Sender<CodeGrantId>,
}

impl Default for GrantRevocations {
    fn default() -> Self {
        let (sender, _) = tokio::sync::broadcast::channel(64);
        Self { sender }
    }
}

impl GrantRevocations {
    pub(crate) fn subscribe(&self) -> tokio::sync::broadcast::Receiver<CodeGrantId> {
        self.sender.subscribe()
    }

    fn publish(&self, grant_id: CodeGrantId) {
        // No subscriber is fine: the durable row already refuses the
        // grant's next call.
        let _ = self.sender.send(grant_id);
    }
}

impl super::runtime::CodeRuntime {
    /// Mint the grant a connect approval produces. Refuses an identity that
    /// already holds a live grant — revoke first, so a re-link is an
    /// explicit replacement.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn mint_adapter_grant(
        &self,
        owner: &OwnerId,
        channel_kind: &str,
        external_identity: &str,
        workspace_identity: &str,
    ) -> Result<(CodeExternalGrant, AdapterTokenPair), ServerError> {
        let pair = mint_pair();
        let grant = tidebreak_core::db::code::mint_external_grant(
            &self.db,
            owner,
            channel_kind,
            external_identity,
            workspace_identity,
            &hash_adapter_token(&pair.token),
            &hash_adapter_token(&pair.refresh),
        )
        .await?;
        Ok((grant, pair))
    }

    /// The live grant a presented access token authenticates. A revoked
    /// grant's next call dies here.
    pub(crate) async fn authenticate_adapter_token(
        &self,
        token: &str,
    ) -> Result<Option<CodeExternalGrant>, ServerError> {
        Ok(tidebreak_core::db::code::grant_by_token_hash_all_owners(
            &self.db,
            &hash_adapter_token(token),
        )
        .await?)
    }

    /// Rotate a token pair against a presented refresh token. A replayed
    /// rotated token revokes the grant durably and severs its live event
    /// streams before this returns.
    pub(crate) async fn rotate_adapter_token(
        &self,
        refresh: &str,
    ) -> Result<(GrantRotation, Option<AdapterTokenPair>), ServerError> {
        let pair = mint_pair();
        let outcome = tidebreak_core::db::code::rotate_external_grant_all_owners(
            &self.db,
            &hash_adapter_token(refresh),
            &hash_adapter_token(&pair.token),
            &hash_adapter_token(&pair.refresh),
        )
        .await?;
        match &outcome {
            GrantRotation::Rotated(_) => Ok((outcome, Some(pair))),
            GrantRotation::ReuseDetected(grant) => {
                tracing::warn!(
                    grant = %grant.id,
                    channel = %grant.channel_kind,
                    "a rotated refresh token was replayed; the grant is revoked"
                );
                self.grant_revocations().publish(grant.id);
                Ok((outcome, None))
            }
            GrantRotation::Unknown => Ok((outcome, None)),
        }
    }

    /// Revoke one grant on the owner's word and sever its live streams.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) async fn revoke_adapter_grant(
        &self,
        owner: &OwnerId,
        grant_id: CodeGrantId,
        reason: &str,
    ) -> Result<Option<CodeExternalGrant>, ServerError> {
        let revoked =
            tidebreak_core::db::code::revoke_external_grant(&self.db, owner, grant_id, reason)
                .await?;
        if revoked.is_some() {
            self.grant_revocations().publish(grant_id);
        }
        Ok(revoked)
    }

    pub(crate) fn grant_revocations(&self) -> Arc<GrantRevocations> {
        self.grant_revocations.clone()
    }

    /// Whether the grant's durable row is still live.
    ///
    /// The event stream's handshake recheck: a revocation that commits
    /// between token authentication and the revocation subscription
    /// published into a channel nobody held, so the stream re-reads the
    /// row while holding its subscription before it starts.
    pub(crate) async fn adapter_grant_is_live(
        &self,
        owner: &OwnerId,
        grant_id: CodeGrantId,
    ) -> Result<bool, ServerError> {
        Ok(
            tidebreak_core::db::code::get_external_grant(&self.db, owner, grant_id)
                .await?
                .is_some_and(|grant| grant.revoked_at.is_none()),
        )
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tidebreak_core::{GrantRotation, OwnerId};

    use super::super::runtime::CodeRuntime;
    use super::hash_adapter_token;

    async fn runtime(root: &std::path::Path) -> Arc<CodeRuntime> {
        let db = tidebreak_core::DbStore::connect(&format!(
            "sqlite://{}?mode=rwc",
            root.join("code.db").display()
        ))
        .await
        .unwrap();
        Arc::new(CodeRuntime::new(
            Arc::new(db),
            root.to_path_buf(),
            None,
            None,
            None,
            None,
            None,
            None,
        ))
    }

    /// The whole credential life: mint authenticates, rotation retires the
    /// old pair, a replayed rotated refresh revokes the grant and severs a
    /// live subscriber, and the revoked grant's next call fails.
    #[tokio::test]
    async fn a_replayed_rotated_token_kills_the_grant_and_severs_streams() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        let owner = OwnerId::local();

        let (grant, pair) = runtime
            .mint_adapter_grant(&owner, "slack", "U7", "T7")
            .await
            .unwrap();
        assert!(pair.token.starts_with("tbg_"));
        assert!(pair.refresh.starts_with("tbr_"));
        let authenticated = runtime
            .authenticate_adapter_token(&pair.token)
            .await
            .unwrap()
            .expect("the minted token must authenticate");
        assert_eq!(authenticated.id, grant.id);

        let mut severed = runtime.grant_revocations().subscribe();

        let (rotated, next) = runtime.rotate_adapter_token(&pair.refresh).await.unwrap();
        assert!(matches!(rotated, GrantRotation::Rotated(_)));
        let next = next.expect("a rotation must issue a new pair");
        assert!(runtime
            .authenticate_adapter_token(&pair.token)
            .await
            .unwrap()
            .is_none());
        assert!(runtime
            .authenticate_adapter_token(&next.token)
            .await
            .unwrap()
            .is_some());

        // The discarded refresh token reappears: theft. The grant revokes
        // and the live subscriber hears it immediately.
        let (reuse, none) = runtime.rotate_adapter_token(&pair.refresh).await.unwrap();
        let GrantRotation::ReuseDetected(revoked) = reuse else {
            panic!("a replayed rotated refresh must be detected, got {reuse:?}");
        };
        assert_eq!(revoked.id, grant.id);
        assert!(none.is_none(), "theft must not mint a new pair");
        let heard = tokio::time::timeout(std::time::Duration::from_secs(1), severed.recv())
            .await
            .expect("the revocation must fan out immediately")
            .unwrap();
        assert_eq!(heard, grant.id);
        assert!(
            runtime
                .authenticate_adapter_token(&next.token)
                .await
                .unwrap()
                .is_none(),
            "a revoked grant's next call must fail"
        );

        // An owner-initiated revoke also fans out.
        let (second, pair_b) = runtime
            .mint_adapter_grant(&owner, "slack", "U8", "T7")
            .await
            .unwrap();
        let mut severed_b = runtime.grant_revocations().subscribe();
        runtime
            .revoke_adapter_grant(&owner, second.id, "owner unlinked the workspace")
            .await
            .unwrap()
            .expect("the grant exists");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), severed_b.recv())
                .await
                .unwrap()
                .unwrap(),
            second.id
        );
        assert!(runtime
            .authenticate_adapter_token(&pair_b.token)
            .await
            .unwrap()
            .is_none());
    }

    /// Hashing is stable and secret-free: the same secret hashes the same,
    /// different secrets differ, and the output is 64 hex digits.
    #[test]
    fn token_hashing_is_stable_hex() {
        let a = hash_adapter_token("tbg_example");
        assert_eq!(a, hash_adapter_token("tbg_example"));
        assert_ne!(a, hash_adapter_token("tbg_other"));
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The event stream's handshake recheck: a revocation that lands after
    /// token authentication but before the revocation subscription must
    /// still refuse the stream, because it published into a channel nobody
    /// held yet.
    #[tokio::test]
    async fn a_revocation_racing_the_handshake_fails_the_recheck() {
        let dir = tempfile::tempdir().unwrap();
        let runtime = runtime(dir.path()).await;
        let owner = OwnerId::local();
        let (grant, _pair) = runtime
            .mint_adapter_grant(&owner, "slack", "U9", "T9")
            .await
            .unwrap();
        assert!(runtime
            .adapter_grant_is_live(&owner, grant.id)
            .await
            .unwrap());
        // The revocation commits with no subscriber listening — exactly the
        // window between the extractor's auth and the stream's subscribe.
        runtime
            .revoke_adapter_grant(&owner, grant.id, "owner unlinked the workspace")
            .await
            .unwrap()
            .expect("the grant exists");
        let severed = runtime.grant_revocations().subscribe();
        assert!(
            !runtime
                .adapter_grant_is_live(&owner, grant.id)
                .await
                .unwrap(),
            "the recheck under the subscription must see the revoked row"
        );
        drop(severed);
    }
}
