//! Server wiring for the remote code-session crate.

#[cfg(test)]
pub(crate) mod fixtures;
pub mod service;

pub use tidebreak_code_remote::{driver, gateway, wire};
pub use tidebreak_code_remote::{
    RemoteReapError, RemoteSandboxError, RuntimeToken, RuntimeTokenSource, SandboxProvisioner,
};

#[async_trait::async_trait]
impl tidebreak_code_remote::RemoteSessionHost for super::bus::CodeEventBus {
    fn publish(
        &self,
        session: tidebreak_core::SessionId,
        event: tidebreak_core::code::SequencedEvent,
    ) {
        super::bus::CodeEventBus::publish(self, session, event);
    }

    async fn persist_session(
        &self,
        store: &tidebreak_core::DbStore,
        session: &tidebreak_core::Session,
    ) -> Result<bool, tidebreak_core::AgentError> {
        super::attention::persist_session(store, self, session).await
    }

    async fn apply_attention(
        &self,
        store: &tidebreak_core::DbStore,
        owner: &tidebreak_core::OwnerId,
        session_id: tidebreak_core::SessionId,
        next: tidebreak_core::Attention,
    ) -> Result<(), tidebreak_core::AgentError> {
        let _ =
            super::attention::apply_attention(store, self, owner, session_id, next, false).await?;
        Ok(())
    }

    async fn journal_event(
        &self,
        store: &tidebreak_core::DbStore,
        owner: &tidebreak_core::OwnerId,
        session_id: tidebreak_core::SessionId,
        spawn_epoch: i64,
        event: tidebreak_core::Event,
    ) {
        let _ = super::session_worker::journal_event(
            store,
            self,
            owner,
            session_id,
            spawn_epoch,
            event,
        )
        .await;
    }

    async fn fence_session(
        &self,
        store: &tidebreak_core::DbStore,
        session: &mut tidebreak_core::Session,
        reason: tidebreak_core::FenceReason,
    ) -> Result<(), tidebreak_core::AgentError> {
        super::recovery::fence_session(store, self, session, reason).await
    }

    async fn recover_dead_worker(
        &self,
        store: &tidebreak_core::DbStore,
        session: &tidebreak_core::Session,
    ) -> Result<Option<tidebreak_core::Session>, tidebreak_core::AgentError> {
        super::recovery::recover_dead_worker(store, self, session).await
    }

    async fn reap_session(
        &self,
        store: &tidebreak_core::DbStore,
        session: tidebreak_core::Session,
    ) -> Result<tidebreak_core::Session, tidebreak_code_remote::RemoteReapError> {
        super::recovery::reap_session(store, self, session)
            .await
            .map_err(|error| match error {
                super::recovery::ReapSessionError::Store(error) => {
                    tidebreak_code_remote::RemoteReapError::Store(error)
                }
                other => tidebreak_code_remote::RemoteReapError::Host(other.to_string()),
            })
    }
}
